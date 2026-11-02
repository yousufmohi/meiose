//! Shared helpers for handler tests. The interactive-element classifier here
//! is deliberately written independently of the handler's own, so a change to
//! the handler's role lists cannot silently weaken the "refs survive" check.

#![allow(dead_code)]

pub mod session;

use std::path::{Path, PathBuf};

/// Committed fixtures, plus any private ones in tests/fixtures/local/.
pub fn playwright_fixtures(include_local: bool) -> Vec<PathBuf> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let mut dirs = vec![root.join("playwright")];
    if include_local {
        dirs.push(root.join("local/playwright"));
    }
    let mut v: Vec<PathBuf> = dirs
        .iter()
        .filter_map(|d| std::fs::read_dir(d).ok())
        .flatten()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|e| e == "txt"))
        .collect();
    v.sort();
    v
}

const ACTIONABLE: &[&str] = &[
    "link",
    "button",
    "textbox",
    "searchbox",
    "checkbox",
    "radio",
    "combobox",
    "listbox",
    "option",
    "menuitem",
    "menuitemcheckbox",
    "menuitemradio",
    "tab",
    "switch",
    "slider",
    "spinbutton",
    "treeitem",
    "iframe",
    "gridcell",
    "scrollbar",
];

/// Refs of every element an agent could act on, in document order.
pub fn interactive_refs(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let Some(start) = text.find("```yaml\n") else {
        return out;
    };
    for line in text[start + 8..].lines() {
        if line == "```" {
            break;
        }
        let body = line
            .trim_start()
            .trim_start_matches("- ")
            .trim_start_matches('\'');
        let role: String = body
            .chars()
            .take_while(|c| c.is_ascii_lowercase())
            .collect();
        // Look only at the key (before an inline value) for attributes.
        let key = key_part(body);
        let actionable = ACTIONABLE.contains(&role.as_str())
            || key.contains("[cursor=pointer]")
            || ["[active]", "[checked", "[expanded", "[pressed", "[selected"]
                .iter()
                .any(|a| key.contains(a));
        if actionable && let Some(r) = ref_of(key) {
            out.push(r);
        }
    }
    out
}

fn key_part(body: &str) -> &str {
    // Skip a quoted name, then cut at the first ": " (start of the value).
    let mut in_name = false;
    let mut prev = '\0';
    for (i, c) in body.char_indices() {
        match c {
            '"' if prev != '\\' => in_name = !in_name,
            ':' if !in_name => return &body[..i],
            _ => {}
        }
        prev = c;
    }
    body
}

fn ref_of(key: &str) -> Option<String> {
    let s = key.rfind("[ref=")? + 5;
    let e = key[s..].find(']')? + s;
    Some(key[s..e].to_string())
}

/// Assert every invariant a trimmed output must satisfy.
pub fn check_invariants(input: &str, output: &str, expand_id: &str) {
    // 1. Never larger.
    assert!(
        output.len() < input.len(),
        "output must be smaller than input"
    );

    // 2. Every interactive ref survives, in the same order.
    let before = interactive_refs(input);
    let after_all: Vec<String> = {
        let mut v = Vec::new();
        let mut rest = output;
        while let Some(p) = rest.find("[ref=") {
            let s = p + 5;
            let e = rest[s..].find(']').map(|e| e + s).unwrap_or(rest.len());
            v.push(rest[s..e].to_string());
            rest = &rest[e..];
        }
        v
    };
    let mut it = after_all.iter();
    for r in &before {
        assert!(
            it.any(|a| a == r),
            "interactive ref {r} is missing or out of order in the output"
        );
    }

    // 3. Marker with the expand id.
    assert!(
        output.contains(&format!("meiose_expand(\"{expand_id}\")")),
        "output lacks the expand marker"
    );

    // 4. Everything outside the yaml block is unchanged.
    let (ib, ie) = block(input);
    let (ob, oe) = block(output);
    assert_eq!(
        &input[..ib],
        &output[..ob],
        "text before the snapshot changed"
    );
    let in_tail = &input[ie..];
    let out_tail = &output[oe..];
    assert!(
        out_tail.starts_with(in_tail.trim_end_matches('\n')),
        "text after the snapshot changed"
    );

    // 5. The block is still well-formed: even indents, "- " items, no skipped levels.
    let mut prev_depth: Option<usize> = None;
    for line in output[ob..oe].lines() {
        let t = line.trim_start_matches(' ');
        let indent = line.len() - t.len();
        assert!(indent % 2 == 0, "odd indent: {line:?}");
        assert!(t.starts_with("- "), "not a list item: {line:?}");
        let d = indent / 2;
        match prev_depth {
            None => assert_eq!(d, 0, "first line must be at depth 0"),
            Some(p) => assert!(d <= p + 1, "skipped a level: {line:?}"),
        }
        prev_depth = Some(d);
    }
}

/// Byte range of the yaml body (between the fences).
fn block(text: &str) -> (usize, usize) {
    let s = text.find("```yaml\n").expect("yaml block") + 8;
    let e = text[s..].find("\n```").expect("closing fence") + s;
    (s, e)
}
