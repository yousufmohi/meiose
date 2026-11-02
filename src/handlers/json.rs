//! Trims large JSON tool outputs (database rows, API listings).
//!
//! Long arrays keep their first few items, followed by one string element
//! saying how many were cut and which keys the items have, so the shape stays
//! readable. Very long strings are shortened. Numbers are preserved exactly
//! (serde_json `arbitrary_precision`). Pretty-printed input stays pretty.

use serde_json::{Map, Value};

use super::{Handler, Mode, TrimContext, Trimmed};

pub struct Json;

/// Outputs smaller than this are left alone.
const MIN_INPUT_BYTES: usize = 8 * 1024;

struct Limits {
    array_max: usize,
    array_keep: usize,
    string_max: usize,
    string_keep: usize,
    /// An array too short for `array_max` but larger than this (compact
    /// JSON) keeps whole items up to `array_bytes_keep`, at least one. A short list
    /// of big objects (16 GitHub pull requests: 85 KB) is otherwise left to
    /// the text handler, which cuts the JSON mid-item.
    array_bytes_max: usize,
    array_bytes_keep: usize,
    /// Output larger than this is shrunk to fit: Claude Code saves MCP text
    /// over 50,000 characters to a file instead of showing it.
    output_max: usize,
}

impl Limits {
    fn for_mode(mode: Mode) -> Limits {
        match mode {
            Mode::Conservative => Limits {
                array_max: 40,
                array_keep: 10,
                string_max: 4000,
                string_keep: 3000,
                array_bytes_max: 40 * 1024,
                array_bytes_keep: 16 * 1024,
                output_max: 45_000,
            },
            Mode::Aggressive => Limits {
                array_max: 10,
                array_keep: 3,
                string_max: 600,
                string_keep: 400,
                array_bytes_max: 12 * 1024,
                array_bytes_keep: 4 * 1024,
                output_max: 16_000,
            },
        }
    }
}

impl Handler for Json {
    fn name(&self) -> &'static str {
        "json"
    }

    fn trim(&self, text: &str, cx: &TrimContext) -> Option<Trimmed> {
        trim_json_min(text, cx, MIN_INPUT_BYTES)
    }
}

#[doc(hidden)]
pub fn trim_json_min(text: &str, cx: &TrimContext, min_bytes: usize) -> Option<Trimmed> {
    if text.len() < min_bytes {
        return None;
    }
    let t = text.trim();
    if !(t.starts_with('{') || t.starts_with('[')) {
        return None;
    }
    let mut v: Value = serde_json::from_str(t).ok()?;
    let limits = Limits::for_mode(cx.mode);
    let mut stats = Stats::default();
    walk(&mut v, &limits, &mut stats);
    let pretty = t.contains('\n');
    let render = |v: &Value| {
        if pretty {
            serde_json::to_string_pretty(v).ok()
        } else {
            serde_json::to_string(v).ok()
        }
    };
    let mut out = render(&v)?;
    // Still too big to reach the model: shrink to fit, keeping valid JSON.
    // Each round aims at a smaller compact size, since pretty printing adds
    // indentation the compact budget does not see.
    let mut budget = limits.output_max * size(&v) / out.len().max(1);
    for _ in 0..8 {
        if out.len() <= limits.output_max {
            break;
        }
        fit(&mut v, budget, &mut stats);
        out = render(&v)?;
        budget = budget * 7 / 10;
    }
    if stats.items == 0 && stats.strings == 0 && stats.summarized == 0 {
        return None;
    }
    out.push_str(&format!(
        "\n[meiose: {} array items trimmed{}{} — call meiose_expand(\"{}\") for the full output, or add query=\"…\" for only the lines you need]",
        stats.items,
        if stats.strings > 0 {
            format!(", {} long strings shortened", stats.strings)
        } else {
            String::new()
        },
        if stats.summarized > 0 {
            format!(
                ", {} nested values summarized to fit {} KB",
                stats.summarized,
                limits.output_max / 1000
            )
        } else {
            String::new()
        },
        cx.expand_id
    ));
    if out.len() >= text.len() {
        return None;
    }
    Some(Trimmed {
        text: out,
        nodes_trimmed: stats.items,
        texts_shortened: stats.strings,
    })
}

#[derive(Default)]
struct Stats {
    items: usize,
    strings: usize,
    summarized: usize,
}

fn size(v: &Value) -> usize {
    serde_json::to_string(v).map_or(0, |s| s.len())
}

fn is_container(v: &Value) -> bool {
    v.is_object() || v.is_array()
}

/// A note this handler wrote at the end of an array: `…12 more items` plus
/// whatever follows (` with keys: …`). Returns the count and that rest.
fn parse_note(s: &str) -> Option<(usize, String)> {
    let s = s.strip_prefix('…')?;
    let digits = s.find(|c: char| !c.is_ascii_digit())?;
    let n = s[..digits].parse().ok()?;
    let rest = s[digits..].strip_prefix(" more items")?;
    Some((n, rest.to_string()))
}

/// What a container held, as a one-line string.
fn summary(v: &Value) -> String {
    match v {
        Value::Object(m) => {
            let keys: Vec<&str> = m.keys().take(8).map(String::as_str).collect();
            let more = if m.len() > 8 { ", …" } else { "" };
            format!("{{…{} keys: {}{more}}}", m.len(), keys.join(", "))
        }
        Value::Array(a) => format!("[…{} items]", a.len()),
        _ => String::new(),
    }
}

/// Shrink `v` to about `budget` bytes of compact JSON, keeping it valid.
/// Objects keep their one-line fields first (ids, states, URLs: cheap and
/// what an agent acts on), then nested values in order while they fit; the
/// rest become one-line summaries. Arrays keep whole items in order.
fn fit(v: &mut Value, budget: usize, stats: &mut Stats) {
    if size(v) <= budget {
        return;
    }
    match v {
        Value::Array(items) => {
            let (prior, rest) = match items.last() {
                Some(Value::String(s)) => parse_note(s).unwrap_or((0, String::new())),
                _ => (0, String::new()),
            };
            if prior > 0 {
                items.pop();
            }
            let rest = if prior > 0 {
                rest
            } else {
                let keys = item_keys(&items[..]);
                if keys.is_empty() {
                    String::new()
                } else {
                    format!(" with keys: {}", keys.join(", "))
                }
            };
            let n = items.len();
            let mut left = budget.saturating_sub(64 + rest.len());
            let mut keep = 0;
            for item in items.iter_mut() {
                let s = size(item) + 1;
                if s <= left {
                    left -= s;
                    keep += 1;
                } else {
                    if keep == 0 {
                        fit(item, left, stats);
                        keep = 1;
                    }
                    break;
                }
            }
            items.truncate(keep);
            stats.items += n - keep;
            let total = prior + n - keep;
            if total > 0 {
                items.push(Value::String(format!("…{total} more items{rest}")));
            }
        }
        Value::Object(map) => {
            let field = |k: &String, x: &Value| k.len() + 4 + size(x);
            let mut scalars: usize = map
                .iter()
                .filter(|(_, x)| !is_container(x))
                .map(|(k, x)| field(k, x))
                .sum();
            if scalars > budget / 2 {
                for (_, x) in map.iter_mut() {
                    if let Value::String(s) = x
                        && s.chars().nth(200).is_some()
                    {
                        let total = s.chars().count();
                        let keep: String = s.chars().take(200).collect();
                        *s = format!("{keep}… (+{} chars)", total - 200);
                        stats.strings += 1;
                    }
                }
                scalars = map
                    .iter()
                    .filter(|(_, x)| !is_container(x))
                    .map(|(k, x)| field(k, x))
                    .sum();
            }
            if scalars > budget {
                // Too many fields even one line each: keep them in order
                // while they fit, name the rest under one "…" key.
                let mut left = budget.saturating_sub(256);
                let mut kept = Map::new();
                let mut dropped: Vec<String> = Vec::new();
                for (k, mut x) in std::mem::take(map) {
                    if is_container(&x) {
                        x = Value::String(summary(&x));
                    }
                    let s = field(&k, &x);
                    if dropped.is_empty() && s <= left {
                        left -= s;
                        kept.insert(k, x);
                    } else {
                        dropped.push(k);
                    }
                }
                let names: Vec<&str> = dropped.iter().take(8).map(String::as_str).collect();
                kept.insert(
                    "…".to_string(),
                    Value::String(format!(
                        "{} more keys: {}{}",
                        dropped.len(),
                        names.join(", "),
                        if dropped.len() > 8 { ", …" } else { "" }
                    )),
                );
                stats.summarized += 1;
                *map = kept;
                return;
            }
            let mut left = budget.saturating_sub(scalars);
            for (k, x) in map.iter_mut() {
                if !is_container(x) {
                    continue;
                }
                let s = field(k, x);
                if s <= left {
                    left -= s;
                } else if left > 512 {
                    fit(x, left - k.len() - 4, stats);
                    left = left.saturating_sub(field(k, x));
                } else {
                    *x = Value::String(summary(x));
                    stats.summarized += 1;
                    left = left.saturating_sub(field(k, x));
                }
            }
        }
        _ => {}
    }
}

fn walk(v: &mut Value, limits: &Limits, stats: &mut Stats) {
    match v {
        Value::Array(items) => {
            let n = items.len();
            let keys = if n > 1 { item_keys(&items[..]) } else { vec![] };
            if n > limits.array_max {
                items.truncate(limits.array_keep);
            }
            for item in items.iter_mut() {
                walk(item, limits, stats);
            }
            // Only lists the count rule left whole: it already decides how
            // many samples a long list shows.
            if n <= limits.array_max && n > 1 {
                let sizes: Vec<usize> = items
                    .iter()
                    .map(|i| serde_json::to_string(i).map_or(0, |t| t.len()))
                    .collect();
                if sizes.iter().sum::<usize>() > limits.array_bytes_max {
                    let mut total = sizes[0];
                    let mut keep = 1;
                    while keep < sizes.len() && total + sizes[keep] <= limits.array_bytes_keep {
                        total += sizes[keep];
                        keep += 1;
                    }
                    items.truncate(keep);
                }
            }
            let cut = n - items.len();
            if cut > 0 {
                stats.items += cut;
                items.push(Value::String(if keys.is_empty() {
                    format!("…{cut} more items")
                } else {
                    format!("…{cut} more items with keys: {}", keys.join(", "))
                }));
            }
        }
        Value::Object(map) => {
            for (_, val) in map.iter_mut() {
                walk(val, limits, stats);
            }
        }
        Value::String(s)
            if s.len() > limits.string_max && s.chars().nth(limits.string_max).is_some() =>
        {
            let total = s.chars().count();
            let keep: String = s.chars().take(limits.string_keep).collect();
            *s = format!("{keep}… (+{} chars)", total - limits.string_keep);
            stats.strings += 1;
        }
        _ => {}
    }
}

/// Union of object keys across array items, in first-seen order (max 30).
fn item_keys(items: &[Value]) -> Vec<String> {
    let mut seen: Map<String, Value> = Map::new();
    for it in items.iter().take(200) {
        if let Value::Object(m) = it {
            for k in m.keys() {
                if !seen.contains_key(k) {
                    seen.insert(k.clone(), Value::Null);
                }
            }
        }
    }
    seen.keys().take(30).cloned().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cx(mode: Mode) -> TrimContext<'static> {
        TrimContext {
            expand_id: "j1",
            mode,
        }
    }

    #[test]
    fn long_arrays_keep_samples_and_describe_the_rest() {
        let rows: Vec<Value> = (0..100)
            .map(|i| serde_json::json!({"id": i, "name": format!("user{i}"), "email": "x@y.z"}))
            .collect();
        let text = serde_json::to_string(&serde_json::json!({ "rows": rows })).unwrap();
        let out = trim_json_min(&text, &cx(Mode::Conservative), 0).unwrap();
        let (json_part, marker) = out.text.rsplit_once('\n').unwrap();
        let v: Value = serde_json::from_str(json_part).unwrap();
        let arr = v["rows"].as_array().unwrap();
        assert_eq!(arr.len(), 11);
        assert_eq!(arr[9]["name"], "user9");
        assert_eq!(arr[10], "…90 more items with keys: id, name, email");
        assert!(marker.contains("90 array items trimmed"));
        assert!(marker.contains("meiose_expand(\"j1\")"));
    }

    /// GitHub-like: few items, each a big nested object, no long strings.
    fn big_items(n: usize) -> String {
        let items: Vec<Value> = (0..n)
            .map(|i| {
                let repo: Map<String, Value> = (0..150)
                    .map(|k| (format!("field_{k}_url"), Value::String(format!("https://api.example.com/repos/o/r/{k}"))))
                    .collect();
                serde_json::json!({"number": i, "title": format!("PR {i}"), "head": repo, "base": repo})
            })
            .collect();
        serde_json::to_string_pretty(&items).unwrap()
    }

    #[test]
    fn a_short_list_of_big_items_keeps_whole_items_and_stays_valid_json() {
        let text = big_items(16);
        assert!(text.len() > 150_000);
        let out = trim_json_min(&text, &cx(Mode::Conservative), 0).unwrap();
        let (json_part, marker) = out.text.rsplit_once('\n').unwrap();
        let v: Value = serde_json::from_str(json_part).expect("still valid JSON");
        let arr = v.as_array().unwrap();
        let kept = arr.len() - 1;
        assert!((1..16).contains(&kept), "kept {kept}");
        for (i, item) in arr[..kept].iter().enumerate() {
            assert_eq!(item["number"], i, "whole items, in order");
            assert_eq!(item["head"].as_object().unwrap().len(), 150);
        }
        assert_eq!(
            arr[kept],
            format!(
                "…{} more items with keys: number, title, head, base",
                16 - kept
            )
        );
        assert!(marker.contains(&format!("{} array items trimmed", 16 - kept)));
        assert!(json_part.len() < 40 * 1024);
    }

    #[test]
    fn long_lists_of_big_items_fit_the_output_cap_with_whole_items() {
        // 50 items of 16 KB: the count rule keeps 10 (180 KB pretty), more
        // than Claude Code shows; the cap keeps whole items in order and
        // the note counts everything left out.
        let text = big_items(50);
        let out = trim_json_min(&text, &cx(Mode::Conservative), 0).unwrap();
        let (json_part, marker) = out.text.rsplit_once('\n').unwrap();
        assert!(json_part.len() <= 45_000, "{}", json_part.len());
        let v: Value = serde_json::from_str(json_part).unwrap();
        let arr = v.as_array().unwrap();
        let kept = arr.len() - 1;
        assert!((1..10).contains(&kept), "kept {kept}");
        for (i, item) in arr[..kept].iter().enumerate() {
            assert_eq!(item["number"], i);
            assert_eq!(item["head"].as_object().unwrap().len(), 150);
        }
        assert_eq!(
            arr[kept],
            format!(
                "…{} more items with keys: number, title, head, base",
                50 - kept
            )
        );
        assert!(marker.contains(&format!("{} array items trimmed", 50 - kept)));
    }

    #[test]
    fn long_lists_of_small_items_keep_their_ten_samples() {
        let rows: Vec<Value> = (0..50)
            .map(|i| serde_json::json!({"id": i, "name": format!("user{i}"), "pad": "x".repeat(150)}))
            .collect();
        let text = serde_json::to_string_pretty(&rows).unwrap();
        let out = trim_json_min(&text, &cx(Mode::Conservative), 0).unwrap();
        let (json_part, _) = out.text.rsplit_once('\n').unwrap();
        let v: Value = serde_json::from_str(json_part).unwrap();
        assert_eq!(v.as_array().unwrap().len(), 11);
    }

    /// One big object with no long array or string, like a database row
    /// dump or an API resource with large nested parts.
    fn big_object() -> String {
        let section = |n: usize| -> Value {
            (0..n)
                .map(|k| {
                    (
                        format!("key_{k}"),
                        Value::String(format!("value number {k} of this section")),
                    )
                })
                .collect::<Map<_, _>>()
                .into()
        };
        serde_json::to_string_pretty(&serde_json::json!({
            "id": 12345678901234567890u64,
            "state": "open",
            "html_url": "https://example.com/things/1",
            "config": section(900),
            "metadata": section(900),
            "history": (0..30).map(|i| section(30 + i)).collect::<Vec<_>>(),
        }))
        .unwrap()
    }

    #[test]
    fn a_big_object_is_shrunk_to_the_cap_and_stays_valid_json() {
        let text = big_object();
        assert!(text.len() > 150_000);
        let out = trim_json_min(&text, &cx(Mode::Conservative), 0).unwrap();
        let (json_part, marker) = out.text.rsplit_once('\n').unwrap();
        assert!(json_part.len() <= 45_000, "{}", json_part.len());
        let v: Value = serde_json::from_str(json_part).expect("still valid JSON");
        // One-line fields survive, numbers exactly.
        assert!(json_part.contains("12345678901234567890"));
        assert_eq!(v["state"], "open");
        assert_eq!(v["html_url"], "https://example.com/things/1");
        // Every key is still there; what did not fit is summarized.
        for k in ["config", "metadata", "history"] {
            assert!(v.get(k).is_some(), "{k} missing");
        }
        assert!(
            json_part.contains("{…900 keys: key_0, key_1"),
            "{json_part:.2000}"
        );
        assert!(marker.contains("summarized to fit 45 KB"), "{marker}");
    }

    #[test]
    fn a_flat_object_keeps_its_first_fields_and_names_the_rest() {
        let m: Map<String, Value> = (0..3000)
            .map(|k| {
                (
                    format!("field_{k}"),
                    Value::String(format!("value {k} xxxxxxxxxx")),
                )
            })
            .collect();
        let text = serde_json::to_string_pretty(&Value::Object(m)).unwrap();
        let out = trim_json_min(&text, &cx(Mode::Conservative), 0).unwrap();
        let (json_part, _) = out.text.rsplit_once('\n').unwrap();
        assert!(json_part.len() <= 45_000, "{}", json_part.len());
        let v: Value = serde_json::from_str(json_part).unwrap();
        let m = v.as_object().unwrap();
        assert_eq!(m["field_0"], "value 0 xxxxxxxxxx");
        let kept = m.len() - 1;
        assert_eq!(
            m["…"].as_str().unwrap(),
            format!(
                "{} more keys: {}, …",
                3000 - kept,
                (kept..kept + 8)
                    .map(|k| format!("field_{k}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        );
    }

    #[test]
    fn every_shape_comes_out_valid_and_within_the_cap() {
        // Nested objects and short or long lists of every size class.
        for width in [1usize, 5, 20, 45, 200] {
            for depth in 1..=4 {
                for leaf in [10usize, 300, 5000] {
                    fn build(w: usize, d: usize, leaf: usize, i: usize) -> Value {
                        if d == 0 {
                            return Value::String(format!("{i}:{}", "y".repeat(leaf)));
                        }
                        if d.is_multiple_of(2) {
                            Value::Array((0..w).map(|j| build(w, d - 1, leaf, j)).collect())
                        } else {
                            let mut m: Map<String, Value> = (0..w)
                                .map(|j| (format!("k{j}"), build(w.min(8), d - 1, leaf, j)))
                                .collect();
                            m.insert("n".into(), Value::from(i));
                            Value::Object(m)
                        }
                    }
                    // Approximate size: objects below the top are 8 wide.
                    let est = leaf
                        * match depth {
                            1 => width,
                            2 => width * 8,
                            3 => width * 64,
                            _ => width * width * 64,
                        };
                    if est > 5_000_000 {
                        continue;
                    }
                    let text = serde_json::to_string_pretty(&build(width, depth, leaf, 0)).unwrap();
                    for mode in [Mode::Conservative, Mode::Aggressive] {
                        let cap = Limits::for_mode(mode).output_max;
                        let Some(out) = trim_json_min(&text, &cx(mode), 0) else {
                            assert!(
                                text.len() <= cap.max(40 * 1024) * 2,
                                "untouched {} B",
                                text.len()
                            );
                            continue;
                        };
                        let (json_part, _) = out.text.rsplit_once('\n').unwrap();
                        let _: Value = serde_json::from_str(json_part).unwrap_or_else(|e| {
                            panic!("w{width} d{depth} l{leaf} {mode:?}: invalid JSON: {e}")
                        });
                        assert!(
                            json_part.len() <= cap,
                            "w{width} d{depth} l{leaf} {mode:?}: {} > {cap}",
                            json_part.len()
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn the_cap_never_applies_below_it() {
        // 37.7 KB pretty: under the cap, no rule fires.
        assert!(trim_json_min(&big_items(2), &cx(Mode::Conservative), 0).is_none());
    }

    #[test]
    fn one_big_item_is_kept_whole() {
        let text = big_items(1);
        // Nothing to cut: no long array, no long string.
        assert!(trim_json_min(&text, &cx(Mode::Conservative), 0).is_none());
    }

    #[test]
    fn lists_under_the_size_limit_are_untouched() {
        // 2 items of 16.4 KB compact each: 33 KB, under 40 KB.
        let text = big_items(2);
        assert!(trim_json_min(&text, &cx(Mode::Conservative), 0).is_none());
    }

    #[test]
    fn numbers_are_preserved_exactly() {
        let mut text = String::from("[");
        for i in 0..60 {
            text.push_str(&format!(
                "{{\"big\":123456789012345678901234567890,\"f\":1.10,\"i\":{i}}},"
            ));
        }
        text.pop();
        text.push(']');
        let out = trim_json_min(&text, &cx(Mode::Conservative), 0).unwrap();
        assert!(out.text.contains("123456789012345678901234567890"));
        assert!(out.text.contains("1.10"));
    }

    #[test]
    fn pretty_input_stays_pretty() {
        let rows: Vec<Value> = (0..50).map(|i| serde_json::json!({"i": i})).collect();
        let text = serde_json::to_string_pretty(&rows).unwrap();
        let out = trim_json_min(&text, &cx(Mode::Conservative), 0).unwrap();
        assert!(out.text.starts_with("[\n  {"));
    }

    #[test]
    fn small_or_non_json_is_untouched() {
        let c = cx(Mode::Aggressive);
        assert!(trim_json_min("not json {", &c, 0).is_none());
        assert!(trim_json_min("{\"a\": [1,2,3]}", &c, 0).is_none());
        assert!(trim_json_min(&"[1]".repeat(10), &c, 0).is_none());
        let big = serde_json::to_string(&(0..5000).collect::<Vec<_>>()).unwrap();
        assert!(trim_json_min(&big, &c, MIN_INPUT_BYTES).is_some());
        assert!(trim_json_min("[1,2]", &c, MIN_INPUT_BYTES).is_none());
    }

    #[test]
    fn long_strings_are_shortened_on_char_boundaries() {
        let s = "é".repeat(5000);
        let text = serde_json::to_string(&serde_json::json!({ "body": s })).unwrap();
        let out = trim_json_min(&text, &cx(Mode::Conservative), 0).unwrap();
        let (json_part, _) = out.text.rsplit_once('\n').unwrap();
        let v: Value = serde_json::from_str(json_part).unwrap();
        assert!(v["body"].as_str().unwrap().ends_with("… (+2000 chars)"));
    }
}
