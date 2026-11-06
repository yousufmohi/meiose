//! Partial expand: find lines in a stored output, with enough context to use
//! them, so the agent need not pull a whole page back into context.

use std::borrow::Cow;
use std::collections::BTreeSet;
use std::fmt::Write as _;

/// Matches returned before the note says how many more exist.
const MAX_HITS: usize = 30;
/// A hit brings its parent's whole subtree only if it is at most this long,
/// so a table cell arrives with its row but not with the whole table.
const MAX_CONTEXT_LINES: usize = 40;
const MAX_BYTES: usize = 24_000;
/// JSON whose lines average more than this is compact (one line, as the
/// official GitHub MCP server sends it) and is pretty-printed before the
/// search. A line search over one line would return the whole document.
const COMPACT_LINE_BYTES: usize = 400;
/// A line longer than this is shown as a window around its match (or from
/// its start, for a context line), so one huge line cannot fill the result.
const LONG_LINE_BYTES: usize = 600;
/// Bytes kept on each side of the match inside a long line.
const WINDOW_BYTES: usize = 250;

fn indent(line: &str) -> usize {
    line.len() - line.trim_start_matches(' ').len()
}

/// The text to search: compact JSON pretty-printed, anything else as is.
fn searchable(text: &str) -> Cow<'_, str> {
    if !text.trim_start().starts_with(['{', '[']) {
        return Cow::Borrowed(text);
    }
    let lines = text.matches('\n').count() + 1;
    if text.len() / lines < COMPACT_LINE_BYTES {
        return Cow::Borrowed(text);
    }
    serde_json::from_str::<serde_json::Value>(text)
        .and_then(|v| serde_json::to_string_pretty(&v))
        .map_or(Cow::Borrowed(text), Cow::Owned)
}

/// Byte offset in `line` of its first (case-insensitive) match of any term.
fn find_term(line: &str, terms: &[String]) -> Option<usize> {
    let lower = line.to_lowercase();
    let at = terms.iter().filter_map(|t| lower.find(t.as_str())).min()?;
    // Map the offset back to `line`: lowercasing can change a character's
    // byte length, so walk both in step.
    let mut seen = 0;
    for (off, c) in line.char_indices() {
        if seen >= at {
            return Some(off);
        }
        seen += c.to_lowercase().map(char::len_utf8).sum::<usize>();
    }
    Some(line.len())
}

/// `line`, or a window of it when it is long: around `around` (a match) or
/// from its start. The indentation is kept so the line still reads as
/// nested, and a snapshot element line keeps its whole key (role, name and
/// the `[ref=…]` the agent needs): only the inline value after it is
/// windowed, so a query never returns an element without its ref.
fn window(line: &str, around: Option<usize>) -> Cow<'_, str> {
    if line.len() <= LONG_LINE_BYTES {
        return Cow::Borrowed(line);
    }
    let head = key_end(line).max(indent(line));
    if line.len() - head <= LONG_LINE_BYTES {
        return Cow::Borrowed(line); // the length is in the key: keep it all
    }
    let (start, end) = match around {
        Some(at) if at >= head => (
            at.saturating_sub(WINDOW_BYTES).max(head),
            (at + WINDOW_BYTES).min(line.len()),
        ),
        _ => (head, (head + 2 * WINDOW_BYTES).min(line.len())),
    };
    let start = floor_char(line, start);
    let end = ceil_char(line, end);
    let mut s = String::with_capacity(head + end - start + 32);
    s.push_str(&line[..head]);
    if start > head {
        s.push('…');
    }
    s.push_str(&line[start..end]);
    if end < line.len() {
        let _ = write!(s, "… (+{} chars)", line[end..].chars().count());
    }
    Cow::Owned(s)
}

/// Where a line's key ends, so a window never cuts it off. For a snapshot
/// element: after the last `[…]` attribute following its `[ref=…]` (the
/// last `[ref=` is used, so a value that quotes one only keeps more of the
/// line) and the `: ` that introduces a value. For a pretty-printed JSON
/// field `"key": value`: after the `: `. 0 for any other line.
fn key_end(line: &str) -> usize {
    let ind = indent(line);
    if let Some(key) = line[ind..].strip_prefix('"') {
        let b = key.as_bytes();
        let mut i = 0;
        while i < b.len() && b[i] != b'"' {
            i += if b[i] == b'\\' { 2 } else { 1 };
        }
        return match key.get(i + 1..) {
            Some(rest) if rest.starts_with(": ") => ind + 1 + i + 1 + 2,
            _ => 0,
        };
    }
    let Some(r) = line.rfind("[ref=") else {
        return 0;
    };
    let Some(close) = line[r..].find(']') else {
        return 0;
    };
    let mut end = r + close + 1;
    while let Some(rest) = line[end..].strip_prefix(" [")
        && let Some(close) = rest.find(']')
    {
        end += 2 + close + 1;
    }
    if line[end..].starts_with('\'') {
        end += 1; // the closing quote of a YAML-quoted key
    }
    if line[end..].starts_with(": ") {
        end += 2;
    } else if line[end..].starts_with(':') {
        end += 1;
    }
    end
}

fn floor_char(s: &str, mut i: usize) -> usize {
    while !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

fn ceil_char(s: &str, mut i: usize) -> usize {
    while !s.is_char_boundary(i) {
        i += 1;
    }
    i
}

fn subtree_end(lines: &[&str], i: usize) -> usize {
    let ind = indent(lines[i]);
    lines[i + 1..]
        .iter()
        .position(|l| indent(l) <= ind)
        .map_or(lines.len(), |p| p + i + 1)
}

/// Nearest earlier line indented less than `lines[i]`.
fn parent(lines: &[&str], i: usize) -> Option<usize> {
    let ind = indent(lines[i]);
    (0..i).rev().find(|&j| indent(lines[j]) < ind)
}

/// A pretty-printed JSON line that holds a whole value: `"state": "open",`,
/// not one that opens or closes an object or array.
fn is_one_line_field(line: &str) -> bool {
    let t = line.trim().trim_end_matches(',');
    !(t.is_empty()
        || t.ends_with('{')
        || t.ends_with('[')
        || t.starts_with('}')
        || t.starts_with(']'))
}

/// The lines of `text` containing any `|`-separated term in `query`, each with
/// its ancestors and nearby siblings, in document order. Compact JSON is
/// searched pretty-printed; a query written the way the compact preview
/// shows it (`"email":"x"` without a space, a `&` escape) matches
/// nothing there, so it falls back to the raw text, windowed.
pub fn search(text: &str, query: &str) -> String {
    let terms: Vec<String> = query
        .split('|')
        .map(|t| t.trim().to_lowercase())
        .filter(|t| !t.is_empty())
        .collect();
    let found = match searchable(text) {
        Cow::Owned(pretty) => search_in(&pretty, true, &terms, query)
            .or_else(|| search_in(text, false, &terms, query)),
        Cow::Borrowed(_) => search_in(text, false, &terms, query),
    };
    found.unwrap_or_else(|| {
        format!(
            "No lines match {query:?}. Try a shorter word, or call meiose_expand without query for the full output."
        )
    })
}

/// [`search`] over one text; `None` when no line matches.
fn search_in(text: &str, pretty_printed: bool, terms: &[String], query: &str) -> Option<String> {
    let lines: Vec<&str> = text.lines().collect();
    let is_json = text.trim_start().starts_with(['{', '[']);
    // In document order, so membership below is a binary search.
    let hits: Vec<usize> = lines
        .iter()
        .enumerate()
        .filter(|(_, l)| {
            let l = l.to_lowercase();
            terms.iter().any(|t| l.contains(t))
        })
        .map(|(i, _)| i)
        .collect();
    if hits.is_empty() {
        return None;
    }
    // A long line is windowed: a hit around its match, context from its start.
    let shown = |i: usize| -> Cow<'_, str> {
        let at = hits
            .binary_search(&i)
            .ok()
            .and_then(|_| find_term(lines[i], terms));
        window(lines[i], at)
    };
    let mut keep = BTreeSet::new();
    let mut bytes = 0;
    let mut used = 0;
    for &hit in hits.iter().take(MAX_HITS) {
        let mut add: Vec<usize> = Vec::new();
        let mut a = parent(&lines, hit);
        // Climb to the largest ancestor that is still small: a link inside a
        // cell brings its whole row, a method name brings its whole header
        // (with the version badge that sits beside the heading), but a hit
        // never brings a whole table or page.
        let mut start = hit;
        let mut up = a;
        while let Some(p) = up {
            if subtree_end(&lines, p) - p > MAX_CONTEXT_LINES {
                break;
            }
            start = p;
            up = parent(&lines, p);
        }
        let end = subtree_end(&lines, start).min(start + MAX_CONTEXT_LINES);
        add.extend(start..end);
        // JSON: when the enclosing object is too big to send whole (a GitHub
        // issue is 100+ lines), still send its one-line fields, so a match
        // on a title arrives with the number, state and URL beside it.
        if is_json && let Some(p) = up {
            let width = indent(lines[start]);
            let fields = (p + 1..subtree_end(&lines, p))
                .filter(|&j| indent(lines[j]) == width && is_one_line_field(lines[j]))
                .take(MAX_CONTEXT_LINES);
            add.extend(fields);
            add.push(p);
        }
        while let Some(p) = a {
            add.push(p);
            a = parent(&lines, p);
        }
        let size: usize = add
            .iter()
            .filter(|i| !keep.contains(*i))
            .map(|&i| shown(i).len() + 1)
            .sum();
        if bytes + size > MAX_BYTES && used > 0 {
            break;
        }
        bytes += size;
        used += 1;
        keep.extend(add);
    }
    let mut out = format!("{} line(s) match {query:?}", hits.len());
    if used < hits.len() {
        let _ = write!(
            out,
            "; showing the first {used}, narrow the query for the rest"
        );
    }
    if pretty_printed {
        out.push_str(" (JSON shown pretty-printed)");
    }
    out.push_str(":\n");
    for i in keep {
        out.push_str(&shown(i));
        out.push('\n');
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const PAGE: &str = "- main:\n  - table:\n    - row:\n      - cell \"Frank Bach\"\n      - cell \"$51.00\"\n    - row:\n      - cell \"Jason Doe\"\n      - cell \"$100.00\"\n";

    /// PAGE plus enough rows that the table is larger than the context limit,
    /// as real tables are (a tiny page fits whole, which is fine).
    fn big_table() -> String {
        let mut p = PAGE.to_string();
        for i in 0..30 {
            p.push_str(&format!(
                "    - row:\n      - cell \"Person {i}\"\n      - cell \"${i}.00\"\n"
            ));
        }
        p
    }

    #[test]
    fn a_cell_brings_its_row_and_ancestors_only() {
        let r = search(&big_table(), "frank");
        assert!(r.contains("$51.00") && r.contains("- main:"), "{r}");
        assert!(!r.contains("Jason Doe"), "{r}");
    }

    #[test]
    fn a_link_inside_a_cell_brings_its_whole_row() {
        // Wikipedia tables: the match is the country's link, the numbers are
        // in sibling cells of the same row.
        let mut p = String::from("- table:\n");
        for (name, pop) in [("Mongolia", "3,431,932"), ("Uruguay", "3,388,081")] {
            p.push_str(&format!(
                "  - row:\n    - cell:\n      - link \"{name}\" [ref=e1]\n    - cell \"{pop}\"\n"
            ));
        }
        for i in 0..30 {
            p.push_str(&format!(
                "  - row:\n    - cell \"C{i}\"\n    - cell \"{i}\"\n"
            ));
        }
        let r = search(&p, "mongolia");
        assert!(r.contains("3,431,932"), "{r}");
        assert!(!r.contains("Uruguay"), "{r}");
    }

    #[test]
    fn a_heading_match_brings_the_badge_beside_it() {
        // rustdoc: the "Stable since" version sits in a sibling just before
        // the method heading.
        let mut p = String::from("- main:\n");
        for (m, v) in [("dedup", "1.0.0"), ("extract_if", "1.87.0")] {
            p.push_str(&format!(
                "  - group:\n    - generic:\n      - generic:\n        - text: {v} ·\n        - link \"Source\" [ref=e2]\n      - heading [level=4]:\n        - text: pub fn\n        - link \"{m}\" [ref=e3]\n"
            ));
            for i in 0..25 {
                // Docs lines must not mention the method, or they would
                // match too and bring the badge in by another route.
                p.push_str(&format!("    - paragraph: docs line {i}\n"));
            }
        }
        let r = search(&p, "extract_if");
        assert!(r.contains("1.87.0"), "{r}");
        assert!(
            !r.contains("1.0.0"),
            "a neighbour's version must not leak in: {r}"
        );
    }

    /// GitHub-style JSON: each item is far over the context limit, so a
    /// match on its title must still bring its one-line fields.
    fn issues_json() -> String {
        let items: Vec<serde_json::Value> = (0..30)
            .map(|i| {
                let mut v = serde_json::json!({
                    "url": format!("https://api.github.com/repos/o/r/issues/{i}"),
                    "html_url": format!("https://github.com/o/r/issues/{i}"),
                    "number": i,
                    "title": format!("Issue title {i}"),
                    "state": "open",
                    "user": {"login": format!("user{i}"), "id": i},
                    "labels": [{"name": "bug", "color": "red"}],
                });
                // A long nested block, like GitHub's reactions/user objects.
                v["reactions"] = (0..60)
                    .map(|k| (format!("r{k}"), serde_json::json!(k)))
                    .collect::<serde_json::Map<_, _>>()
                    .into();
                v
            })
            .collect();
        serde_json::to_string_pretty(&items).unwrap()
    }

    #[test]
    fn a_json_match_brings_its_objects_one_line_fields() {
        let r = search(&issues_json(), "Issue title 27");
        assert!(r.contains("https://github.com/o/r/issues/27"), "{r}");
        assert!(r.contains("\"number\": 27"), "{r}");
        assert!(r.contains("\"state\": \"open\""), "{r}");
        // Nested blocks and other items stay out.
        assert!(!r.contains("\"r59\""), "{r}");
        assert!(!r.contains("issues/26\""), "{r}");
    }

    /// The official GitHub MCP server sends compact JSON: one line. A line
    /// search over it used to return the whole document as the one hit.
    #[test]
    fn compact_json_is_pretty_printed_before_the_search() {
        let items: serde_json::Value = serde_json::from_str(&issues_json()).unwrap();
        let compact = serde_json::to_string(&items).unwrap();
        assert_eq!(compact.lines().count(), 1);
        let r = search(&compact, "Issue title 27");
        assert!(r.contains("\"number\": 27"), "{r}");
        assert!(r.contains("https://github.com/o/r/issues/27"), "{r}");
        assert!(r.contains("\"state\": \"open\""), "{r}");
        assert!(!r.contains("issues/26\""), "{r}");
        assert!(!r.contains("\"r59\""), "{r}");
        assert!(r.contains("(JSON shown pretty-printed)"), "{r}");
        assert!(r.len() < 1_500, "{} bytes:\n{r}", r.len());
        // Already-pretty JSON is searched as it is.
        assert!(!search(&issues_json(), "Issue title 27").contains("pretty-printed"));
    }

    #[test]
    fn a_long_line_is_shown_as_a_window_around_its_match() {
        let line = format!("{}needle in here{}", "x".repeat(5_000), "y".repeat(5_000));
        let text = format!("first line\n{line}\nlast line\n");
        let r = search(&text, "NEEDLE");
        assert!(r.contains("…xxxxxxxx"), "{r}");
        assert!(r.contains("needle in here"), "{r}");
        assert!(r.contains("yyyy… (+"), "{r}");
        assert!(!r.contains("first line") && !r.contains("last line"), "{r}");
        assert!(r.len() < 1_000, "{} bytes", r.len());
        // Multi-byte text around the match must not break the cut.
        let line = format!("{}needle{}", "日本語".repeat(2_000), "é".repeat(2_000));
        let r = search(&line, "needle");
        assert!(r.contains("needle"), "{r}");
        assert!(r.len() < 2_000, "{} bytes", r.len());
    }

    /// The point of a query on a snapshot is to find refs: a windowed line
    /// must never lose the element's `[ref=…]`.
    #[test]
    fn a_long_snapshot_line_keeps_its_role_name_and_ref() {
        // A card link whose name is the whole card text: the match is deep
        // inside the name, the ref comes after it. Kept whole.
        let name = format!("{} needle {}", "a".repeat(400), "b".repeat(400));
        let line = format!("  - link \"{name}\" [ref=e9] [cursor=pointer]");
        let r = search(&format!("- main:\n{line}\n"), "needle");
        assert!(r.contains(&line), "{r}");

        // A long inline value: the key (through its ref) stays, the value
        // is windowed around the match.
        let value = format!("{} needle {}", "c".repeat(3_000), "d".repeat(3_000));
        let text = format!("- main:\n  - paragraph [ref=e7] [active]: {value}\n");
        let r = search(&text, "needle");
        assert!(r.contains("  - paragraph [ref=e7] [active]: …ccc"), "{r}");
        assert!(r.contains("needle"), "{r}");
        assert!(r.contains("ddd… (+"), "{r}");
        assert!(r.len() < 1_000, "{} bytes", r.len());

        // The value is long but the match is in the key: value from its start.
        let text = format!(
            "- main:\n  - link \"needle\" [ref=e8]: {}\n",
            "e".repeat(3_000)
        );
        let r = search(&text, "needle");
        assert!(r.contains("  - link \"needle\" [ref=e8]: eee"), "{r}");
        assert!(r.contains("… (+"), "{r}");

        let key = "- link \"x\" [ref=e1] [cursor=pointer]";
        assert_eq!(key_end(key), key.len());
        assert_eq!(key_end(&format!("{key}:")), key.len() + 1);
        assert_eq!(key_end(&format!("{key}: v")), key.len() + 2);
        assert_eq!(key_end("- text: no ref here"), 0);
    }

    /// The trimmed preview of compact JSON is compact too, so an agent may
    /// copy `"key":"value"` or a Go-style unicode escape (`&` as backslash
    /// u0026) from it. Those do not occur in the pretty-printed text; the raw
    /// text is searched instead.
    #[test]
    fn a_query_in_the_compact_form_falls_back_to_the_raw_text() {
        // The escapes are built at run time so the source holds no `\u`.
        let amp = format!("{}u0026", '\\');
        let lt = format!("{}u003c", '\\');
        let items: Vec<String> = (0..300)
            .map(|i| {
                format!(
                    r#"{{"id":{i},"email":"u{i}@x.test","url":"https://x.test/?a=1{amp}c={i}","name":"row{i}{lt}"}}"#
                )
            })
            .collect();
        let compact = format!("[{}]", items.join(","));
        let parsed: serde_json::Value = serde_json::from_str(&compact).unwrap();
        assert_eq!(
            parsed[7]["url"], "https://x.test/?a=1&c=7",
            "escapes decode"
        );
        for q in [
            r#""email":"u7@x.test""#.to_string(),
            r#""id":7,"#.to_string(),
            format!("{amp}c=7"),
            format!("row7{lt}"),
        ] {
            let r = search(&compact, &q);
            assert!(!r.starts_with("No lines match"), "{q}: {r}");
            assert!(r.contains("u7@x.test"), "{q}: {r}");
            assert!(!r.contains("pretty-printed"), "{q}: {r}");
            assert!(r.len() < 1_000, "{q}: {} bytes", r.len());
        }
        // The pretty-printed search still comes first.
        let r = search(&compact, "u7@x.test");
        assert!(
            r.contains("pretty-printed") && r.contains("\"id\": 7"),
            "{r}"
        );
        assert!(search(&compact, "nothing here").starts_with("No lines match"));
    }

    #[test]
    fn a_windowed_json_field_keeps_its_key() {
        let mut v = serde_json::json!({"title": "t", "number": 7});
        v["body"] = serde_json::Value::String(format!(
            "{} marker33 {}",
            "w ".repeat(2_000),
            "x ".repeat(2_000)
        ));
        let text = serde_json::to_string_pretty(&serde_json::json!([v])).unwrap();
        let r = search(&text, "marker33");
        // The key is kept whole; the value starts with an ellipsis.
        assert!(r.contains("\n    \"body\": …"), "{r}");
        assert!(!r.contains("\"body\": \"w"), "{r}");
        assert!(r.contains("marker33"), "{r}");
        assert!(r.len() < 1_000, "{} bytes", r.len());
        let key = "    \"a \\\"quoted\\\" key\": ";
        assert_eq!(key_end(&format!("{key}\"v\",")), key.len());
        assert_eq!(key_end("    \"just a string in an array\","), 0);
        let quoted = "  - 'link \"A: B\" [ref=e3]': ";
        assert_eq!(key_end(&format!("{quoted}v")), quoted.len());
    }

    #[test]
    fn the_long_line_threshold_is_exact() {
        let short = format!("- text: {}", "a".repeat(600 - 8));
        assert_eq!(short.len(), 600);
        assert!(search(&short, "aaa").contains(&short));
        let long = format!("{short}a");
        assert!(search(&long, "aaa").contains("… (+"));
    }

    #[test]
    fn a_long_context_line_is_cut_from_its_start_and_keeps_its_indent() {
        // A match on the title brings the issue's one-line fields, one of
        // which is a 5,000-character body.
        let mut v = serde_json::json!({"title": "the one", "number": 7});
        v["body"] = serde_json::Value::String("b".repeat(5_000));
        let text = serde_json::to_string_pretty(&serde_json::json!([v])).unwrap();
        let r = search(&text, "the one");
        assert!(r.contains("\"number\": 7"), "{r}");
        assert!(r.contains("    \"body\": \"bbbb"), "{r}");
        assert!(r.contains("… (+"), "{r}");
        assert!(r.len() < 1_000, "{} bytes", r.len());
    }

    #[test]
    fn one_line_fields_are_json_only() {
        // The same shape as a snapshot: no extra siblings come along.
        let mut p = String::from(
            "- list:
",
        );
        for i in 0..60 {
            p.push_str(&format!(
                "  - listitem: entry {i}
"
            ));
        }
        let r = search(&p, "entry 42");
        assert!(r.contains("entry 42") && !r.contains("entry 41"), "{r}");
    }

    #[test]
    fn alternatives_and_no_match() {
        assert!(search(PAGE, "frank|jason").contains("2 line(s)"));
        assert!(search(PAGE, "nothing").starts_with("No lines match"));
    }

    #[test]
    fn matches_are_capped() {
        let big: String = (0..500).map(|i| format!("- row: item {i}\n")).collect();
        let r = search(&big, "item");
        assert!(
            r.contains("500 line(s)") && r.contains("showing the first 30"),
            "{r}"
        );
        assert!(r.len() < 2_000);
    }
}
