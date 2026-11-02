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

/// The lines of `text` containing any `|`-separated term in `query`, each with
/// its ancestors and nearby siblings, in document order.
pub fn search(text: &str, query: &str) -> String {
    let terms: Vec<String> = query
        .split('|')
        .map(|t| t.trim().to_lowercase())
        .filter(|t| !t.is_empty())
        .collect();
    let lines: Vec<&str> = text.lines().collect();
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
        let start = a
            .filter(|&p| subtree_end(&lines, p) - p <= MAX_CONTEXT_LINES)
            .unwrap_or(hit);
        let end = subtree_end(&lines, start).min(start + MAX_CONTEXT_LINES);
        add.extend(start..end);
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

    #[test]
    fn a_cell_brings_its_row_and_ancestors_only() {
        let r = search(PAGE, "frank");
        assert!(r.contains("$51.00") && r.contains("- main:"), "{r}");
        assert!(!r.contains("Jason Doe"), "{r}");
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
