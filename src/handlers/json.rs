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
}

impl Limits {
    fn for_mode(mode: Mode) -> Limits {
        match mode {
            Mode::Conservative => Limits {
                array_max: 40,
                array_keep: 10,
                string_max: 4000,
                string_keep: 3000,
            },
            Mode::Aggressive => Limits {
                array_max: 10,
                array_keep: 3,
                string_max: 600,
                string_keep: 400,
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
    if stats.items == 0 && stats.strings == 0 {
        return None;
    }
    let pretty = t.contains('\n');
    let mut out = if pretty {
        serde_json::to_string_pretty(&v).ok()?
    } else {
        serde_json::to_string(&v).ok()?
    };
    out.push_str(&format!(
        "\n[meiose: {} array items trimmed{} — call meiose_expand(\"{}\") for the full output, or add query=\"…\" for only the lines you need]",
        stats.items,
        if stats.strings > 0 {
            format!(", {} long strings shortened", stats.strings)
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
}

fn walk(v: &mut Value, limits: &Limits, stats: &mut Stats) {
    match v {
        Value::Array(items) => {
            if items.len() > limits.array_max {
                let cut = items.len() - limits.array_keep;
                let keys = item_keys(&items[..]);
                items.truncate(limits.array_keep);
                stats.items += cut;
                let note = if keys.is_empty() {
                    format!("…{cut} more items")
                } else {
                    format!("…{cut} more items with keys: {}", keys.join(", "))
                };
                for item in items.iter_mut() {
                    walk(item, limits, stats);
                }
                items.push(Value::String(note));
            } else {
                for item in items.iter_mut() {
                    walk(item, limits, stats);
                }
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
