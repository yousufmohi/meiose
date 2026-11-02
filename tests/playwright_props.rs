//! Property tests for the Playwright handler.
//!
//! 1. Generated snapshot trees (random roles, names with quotes/colons, refs,
//!    cursor/state attributes, long values, /url props): invariants hold.
//! 2. Random line windows cut from real fixtures: never panics, and when it
//!    trims, invariants hold.
//! 3. Arbitrary text: never panics.

mod common;

use meiose::handlers::{Mode, TrimContext, playwright};
use proptest::prelude::*;

#[derive(Debug, Clone)]
struct N {
    role: &'static str,
    name: Option<String>,
    cursor: bool,
    state: Option<&'static str>,
    value: Option<String>,
    url: Option<String>,
    children: Vec<N>,
}

const ROLES: &[&str] = &[
    "generic",
    "generic",
    "generic",
    "list",
    "listitem",
    "paragraph",
    "row",
    "cell",
    "heading",
    "img",
    "link",
    "button",
    "textbox",
    "checkbox",
    "combobox",
    "option",
    "text",
    "code",
    "navigation",
    "region",
    "group",
];

fn name_strategy() -> impl Strategy<Value = String> {
    prop_oneof![
        "[a-zA-Z ]{1,12}",
        // tricky: colons (forces YAML quoting), apostrophes, escaped quotes
        "[a-z]{1,5}: [a-z']{1,8}",
        "[a-z]{1,4}\\\\\"[a-z]{1,4}",
    ]
}

fn value_strategy() -> impl Strategy<Value = String> {
    prop_oneof![
        4 => "[a-zA-Z0-9 .,]{1,40}",
        1 => "\\|",
        1 => "[a-z ]{1300,1500}",
        1 => "\"[a-z\\\\\"]{1300,1400}\"",
    ]
}

fn leaf() -> impl Strategy<Value = N> {
    (
        prop::sample::select(ROLES),
        prop::option::of(name_strategy()),
        prop::bool::weighted(0.15),
        prop::option::weighted(
            0.05,
            prop::sample::select(&["checked", "expanded", "active"][..]),
        ),
        prop::option::weighted(0.3, value_strategy()),
        prop::option::weighted(0.1, "/[a-z]{1,10}"),
    )
        .prop_map(|(role, name, cursor, state, value, url)| N {
            role,
            name,
            cursor,
            state,
            value,
            url,
            children: vec![],
        })
}

fn tree() -> impl Strategy<Value = Vec<N>> {
    let node = leaf().prop_recursive(6, 200, 30, |inner| {
        (leaf(), prop::collection::vec(inner, 0..30)).prop_map(|(mut n, kids)| {
            if !kids.is_empty() {
                n.value = None; // an element has either children or a value
            }
            n.children = kids;
            n
        })
    });
    prop::collection::vec(node, 1..4)
}

fn render(nodes: &[N]) -> String {
    let mut out = String::new();
    let mut next_ref = 0usize;
    for n in nodes {
        render_node(n, 0, &mut out, &mut next_ref);
    }
    out.pop();
    format!("### Page\n- Page URL: https://gen.test/\n### Snapshot\n```yaml\n{out}\n```\n")
}

fn render_node(n: &N, depth: usize, out: &mut String, next_ref: &mut usize) {
    let mut key = String::from(n.role);
    if n.role != "text" {
        if let Some(name) = &n.name {
            key.push_str(&format!(" \"{name}\""));
        }
        if let Some(s) = n.state {
            key.push_str(&format!(" [{s}]"));
        }
        *next_ref += 1;
        key.push_str(&format!(" [ref=e{next_ref}]"));
        if n.cursor {
            key.push_str(" [cursor=pointer]");
        }
    }
    let has_kids = !n.children.is_empty() || n.url.is_some();
    let indent = "  ".repeat(depth);
    let line = if key.contains(": ") {
        // YAML single-quoted key, as Playwright emits it.
        let q = format!("'{}'", key.replace('\'', "''"));
        if has_kids { format!("{q}:") } else { q }
    } else if let (Some(v), false) = (&n.value, has_kids) {
        format!("{key}: {v}")
    } else if has_kids {
        format!("{key}:")
    } else if n.role == "text" {
        format!("text: {}", n.value.clone().unwrap_or_else(|| "t".into()))
    } else {
        key
    };
    out.push_str(&format!("{indent}- {line}\n"));
    if let Some(u) = &n.url {
        out.push_str(&format!("{indent}  - /url: {u}\n"));
    }
    for c in &n.children {
        render_node(c, depth + 1, out, next_ref);
    }
}

fn mode() -> impl Strategy<Value = Mode> {
    prop_oneof![Just(Mode::Conservative), Just(Mode::Aggressive)]
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 300, ..ProptestConfig::default() })]

    #[test]
    fn generated_trees_keep_invariants(nodes in tree(), mode in mode()) {
        let input = render(&nodes);
        let cx = TrimContext { expand_id: "gen", mode };
        if let Some(t) = playwright::trim_snapshot_min(&input, &cx, 0) {
            common::check_invariants(&input, &t.text, "gen");
        }
    }

    #[test]
    fn fixture_windows_keep_invariants(
        idx in any::<prop::sample::Index>(),
        start in any::<prop::sample::Index>(),
        len in 1usize..400,
        mode in mode(),
    ) {
        let fixtures = common::playwright_fixtures(false);
        let text = std::fs::read_to_string(idx.get(&fixtures)).unwrap();
        let s = text.find("```yaml\n").unwrap() + 8;
        let e = text[s..].find("\n```").unwrap() + s;
        let lines: Vec<&str> = text[s..e].lines().collect();
        let from = start.index(lines.len());
        // Re-base the window to depth 0 so it is a plausible snapshot.
        let window = &lines[from..(from + len).min(lines.len())];
        let base = window[0].len() - window[0].trim_start().len();
        let body: Vec<String> = window
            .iter()
            .map(|l| {
                let ind = l.len() - l.trim_start().len();
                format!("{}{}", " ".repeat(ind.saturating_sub(base)), l.trim_start())
            })
            .collect();
        let input = format!("{}{}\n```\n", &text[..s], body.join("\n"));
        let cx = TrimContext { expand_id: "win", mode };
        if let Some(t) = playwright::trim_snapshot_min(&input, &cx, 0) {
            common::check_invariants(&input, &t.text, "win");
        }
    }

    #[test]
    fn arbitrary_text_never_panics(s in "\\PC{0,400}", mode in mode()) {
        let cx = TrimContext { expand_id: "arb", mode };
        let wrapped = format!("### Snapshot\n```yaml\n{s}\n```\n");
        let _ = playwright::trim_snapshot_min(&s, &cx, 0);
        let _ = playwright::trim_snapshot_min(&wrapped, &cx, 0);
    }
}
