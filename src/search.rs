//! Partial expand: find lines in a stored output, with enough context to use
//! them, so the agent need not pull a whole page back into context.

use std::collections::BTreeSet;
use std::fmt::Write as _;

/// Matches returned before the note says how many more exist.
const MAX_HITS: usize = 30;
/// A hit brings its parent's whole subtree only if it is at most this long,
/// so a table cell arrives with its row but not with the whole table.
const MAX_CONTEXT_LINES: usize = 40;
const MAX_BYTES: usize = 24_000;

fn indent(line: &str) -> usize {
    line.len() - line.trim_start_matches(' ').len()
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
/// its ancestors and nearby siblings, in document order.
pub fn search(text: &str, query: &str) -> String {
    let terms: Vec<String> = query
        .split('|')
        .map(|t| t.trim().to_lowercase())
        .filter(|t| !t.is_empty())
        .collect();
    let lines: Vec<&str> = text.lines().collect();
    let is_json = text.trim_start().starts_with(['{', '[']);
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
        return format!(
            "No lines match {query:?}. Try a shorter word, or call meiose_expand without query for the full output."
        );
    }
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
            .map(|&i| lines[i].len() + 1)
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
    out.push_str(":\n");
    for i in keep {
        out.push_str(lines[i]);
        out.push('\n');
    }
    out
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
