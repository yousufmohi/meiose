//! Snapshot diffing: when the agent snapshots the same page again, send only
//! what changed since the snapshot it saw last.
//!
//! Playwright refs are stable within one loaded document (verified: two
//! snapshots are byte-identical; a click changes exactly the affected lines),
//! so unchanged elements and their refs remain valid in the agent's context.
//! They are not stable across navigations: a reload of the same URL renames
//! every ref (`e3` became `f2e3`), so almost every line differs, and the
//! changed-share limit below falls back to a full snapshot. We diff exactly
//! the view the agent was sent.
//!
//! Output keeps the `### Page` header, replaces the yaml block with a
//! ```diff block (changed lines plus their ancestor lines as context), and
//! carries the expand marker for the full snapshot.

use std::collections::HashSet;
use std::time::Duration;

use similar::{Algorithm, DiffOp, capture_diff_slices_deadline};

use crate::handlers::playwright::find_block;

/// Send the full snapshot instead when more than this share of lines changed.
const MAX_CHANGED_RATIO: f64 = 0.4;
/// Bound diff time on pathological inputs (falls back to a coarser diff).
const DIFF_DEADLINE: Duration = Duration::from_millis(20);

/// `- Page URL: …` from the `### Page` section.
pub fn page_url(text: &str) -> Option<&str> {
    let block_start = find_block(text)?.start;
    text[..block_start]
        .lines()
        .find_map(|l| l.strip_prefix("- Page URL: "))
}

/// The yaml body of a snapshot text (the agent's view of the page).
pub fn body(text: &str) -> Option<&str> {
    find_block(text).map(|r| &text[r])
}

/// Render `new_text` (a full, possibly trimmed snapshot) as a diff against
/// `prev_body`. `None` if the change is too large for a diff to help.
pub fn render(prev_body: &str, new_text: &str, expand_id: &str) -> Option<String> {
    let block = find_block(new_text)?;
    let new_body = &new_text[block.clone()];
    let old: Vec<&str> = prev_body.split('\n').collect();
    let new: Vec<&str> = new_body.split('\n').collect();

    let deadline = std::time::Instant::now() + DIFF_DEADLINE;
    let ops = capture_diff_slices_deadline(Algorithm::Myers, &old, &new, Some(deadline));

    let changed: usize = ops
        .iter()
        .map(|op| match *op {
            DiffOp::Equal { .. } => 0,
            DiffOp::Delete { old_len, .. } => old_len,
            DiffOp::Insert { new_len, .. } => new_len,
            DiffOp::Replace {
                old_len, new_len, ..
            } => old_len.max(new_len),
        })
        .sum();
    if changed as f64 > MAX_CHANGED_RATIO * new.len().max(1) as f64 {
        return None;
    }

    // Map old line index → new line index for unchanged lines, so a deleted
    // line's ancestors can be shown once, in new-document terms.
    let mut old_to_new = vec![None; old.len()];
    for op in &ops {
        if let DiffOp::Equal {
            old_index,
            new_index,
            len,
        } = *op
        {
            for k in 0..len {
                old_to_new[old_index + k] = Some(new_index + k);
            }
        }
    }

    let mut out = String::new();
    let mut shown_context: HashSet<usize> = HashSet::new(); // new indices
    let context = |lines: &[&str], idx: usize| -> Vec<usize> { ancestors(lines, idx) };

    for op in &ops {
        let (dels, ins) = match *op {
            DiffOp::Equal { .. } => continue,
            DiffOp::Delete {
                old_index, old_len, ..
            } => (old_index..old_index + old_len, 0..0),
            DiffOp::Insert {
                new_index, new_len, ..
            } => (0..0, new_index..new_index + new_len),
            DiffOp::Replace {
                old_index,
                old_len,
                new_index,
                new_len,
            } => (
                old_index..old_index + old_len,
                new_index..new_index + new_len,
            ),
        };
        for i in dels {
            for a in context(&old, i) {
                if let Some(n) = old_to_new[a]
                    && shown_context.insert(n)
                {
                    push_line(&mut out, ' ', new[n]);
                }
            }
            push_line(&mut out, '-', old[i]);
        }
        for i in ins {
            for a in context(&new, i) {
                // Only unchanged ancestors are context; changed ones are
                // already printed as +/- lines.
                if is_equal_line(&ops, a) && shown_context.insert(a) {
                    push_line(&mut out, ' ', new[a]);
                }
            }
            push_line(&mut out, '+', new[i]);
        }
    }

    let mut text = String::with_capacity(block.start + out.len() + 300);
    // Header up to "### Snapshot\n"; drop the "```yaml\n" opener.
    let header_end = block.start - "```yaml\n".len();
    text.push_str(&new_text[..header_end]);
    if changed == 0 {
        text.push_str(
            "Unchanged since your previous snapshot of this page: every element and ref is as before.\n",
        );
    } else {
        text.push_str(&format!(
            "Changes since your previous snapshot of this page ({changed} of {} lines changed). \
             Everything not shown is unchanged and its refs are still valid. \
             Lines starting with '-' were removed, '+' added, ' ' are unchanged context:\n```diff\n{out}```\n",
            new.len()
        ));
    }
    // Keep whatever followed the yaml block (### Events, …) but drop the
    // full snapshot's own marker line; we add a diff-specific one.
    let after = &new_text[block.end..];
    let after = after.strip_prefix("\n```").unwrap_or(after);
    for line in after.split_inclusive('\n') {
        if !line.starts_with("[meiose: ") {
            text.push_str(line);
        }
    }
    if !text.ends_with('\n') {
        text.push('\n');
    }
    text.push_str(&format!(
        "[meiose: diff vs your previous snapshot — call meiose_expand(\"{expand_id}\") for the full snapshot]"
    ));
    (text.len() < new_text.len()).then_some(text)
}

fn push_line(out: &mut String, tag: char, line: &str) {
    out.push(tag);
    out.push_str(line);
    out.push('\n');
}

fn indent(line: &str) -> usize {
    line.len() - line.trim_start_matches(' ').len()
}

/// Indices of `idx`'s ancestors (lines above it with smaller indent), root first.
fn ancestors(lines: &[&str], idx: usize) -> Vec<usize> {
    let mut chain = Vec::new();
    let mut want = indent(lines[idx]);
    for i in (0..idx).rev() {
        if want == 0 {
            break;
        }
        let d = indent(lines[i]);
        if d < want {
            chain.push(i);
            want = d;
        }
    }
    chain.reverse();
    chain
}

fn is_equal_line(ops: &[DiffOp], new_idx: usize) -> bool {
    ops.iter().any(|op| {
        matches!(*op, DiffOp::Equal { new_index, len, .. } if (new_index..new_index + len).contains(&new_idx))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap(url: &str, yaml: &str) -> String {
        format!(
            "### Page\n- Page URL: {url}\n- Page Title: T\n### Snapshot\n```yaml\n{yaml}\n```\n"
        )
    }

    fn page(checked: bool) -> String {
        let mut y = String::from(
            "- generic [ref=e1]:\n  - heading \"Checkboxes\" [level=3] [ref=e2]\n  - generic [ref=e3]:\n",
        );
        for i in 0..20 {
            y.push_str(&format!(
                "    - paragraph [ref=p{i}]: filler text number {i}\n"
            ));
        }
        y.push_str(&format!(
            "    - checkbox{} [ref=e10]\n    - checkbox [checked] [ref=e11]",
            if checked { " [checked]" } else { "" }
        ));
        snap("https://x.test/a", &y)
    }

    #[test]
    fn extracts_url_and_body() {
        let t = page(false);
        assert_eq!(page_url(&t), Some("https://x.test/a"));
        assert!(body(&t).unwrap().starts_with("- generic [ref=e1]:"));
    }

    #[test]
    fn small_change_becomes_a_diff_with_ancestor_context() {
        let before = page(false);
        let after = page(true);
        let d = render(body(&before).unwrap(), &after, "x1").unwrap();
        assert!(d.starts_with(
            "### Page\n- Page URL: https://x.test/a\n- Page Title: T\n### Snapshot\n"
        ));
        assert!(d.contains("```diff\n"), "{d}");
        assert!(d.contains("\n - generic [ref=e1]:\n   - generic [ref=e3]:\n-    - checkbox [ref=e10]\n+    - checkbox [checked] [ref=e10]\n"), "{d}");
        assert!(
            !d.contains("filler text"),
            "unchanged lines must not be repeated"
        );
        assert!(d.ends_with("call meiose_expand(\"x1\") for the full snapshot]"));
        assert!(d.len() < after.len());
    }

    #[test]
    fn identical_snapshot_says_so() {
        let t = page(false);
        let d = render(body(&t).unwrap(), &t, "x2").unwrap();
        assert!(d.contains("Unchanged since your previous snapshot"));
        assert!(!d.contains("```diff"));
    }

    #[test]
    fn big_change_falls_back_to_full_snapshot() {
        let a = snap(
            "https://x.test/",
            "- generic [ref=e1]:\n  - link \"a\" [ref=e2]\n  - link \"b\" [ref=e3]",
        );
        let b = snap(
            "https://x.test/",
            "- main [ref=m1]:\n  - button \"z\" [ref=z1]\n  - button \"y\" [ref=z2]",
        );
        assert!(render(body(&a).unwrap(), &b, "x3").is_none());
    }

    #[test]
    fn trailing_sections_survive_and_old_marker_is_replaced() {
        let before = page(false);
        let after = page(true).replace(
            "```\n",
            "```\n### Events\n- New console entries: c.log\n[meiose: 3 nodes trimmed — call meiose_expand(\"old\") for the full snapshot]",
        );
        let d = render(body(&before).unwrap(), &after, "x4").unwrap();
        assert!(d.contains("### Events\n- New console entries: c.log\n"));
        assert!(!d.contains("\"old\""));
        assert!(d.contains("meiose_expand(\"x4\")"));
    }
}
