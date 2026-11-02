//! Trims Playwright MCP accessibility snapshots.
//!
//! Input is the text of a tool result that contains
//!
//! ````text
//! ### Snapshot
//! ```yaml
//! - generic [active] [ref=e1]:
//!   - link "Home" [ref=e2] [cursor=pointer]:
//!     - /url: /
//!   - 'link "A: B" [ref=e3]'        (YAML-quoted when the key has ": ")
//!   - paragraph [ref=e4]: some text
//!   - text: loose text
//! ```
//! ````
//!
//! Everything outside the yaml block is kept verbatim. Inside it, each line
//! is parsed just enough to know its depth, role, name, attributes and inline
//! value. A line that does not parse is kept verbatim and treated as
//! interactive, so it (and its ancestors) can never be removed.
//!
//! Rules, all of which preserve every interactive element and its ref:
//! - collapse unnamed, non-interactive `generic` wrappers that have a single
//!   child (aggressive: any number of children), promoting the children;
//! - drop empty nodes: no name, no value, no children, nothing interactive;
//! - dedupe long runs of structurally identical siblings that contain nothing
//!   interactive, leaving a few and an "…and N more similar" line;
//! - shorten very long inline texts;
//! - remove `[cursor=pointer]` where the role already implies it;
//! - aggressive only: strip refs from non-interactive elements.

use std::borrow::Cow;
use std::fmt::Write as _;
use std::ops::Range;

use super::{Handler, Mode, TrimContext, Trimmed};

pub struct Playwright;

impl Handler for Playwright {
    fn name(&self) -> &'static str {
        "playwright"
    }

    fn trim(&self, text: &str, cx: &TrimContext) -> Option<Trimmed> {
        trim_snapshot(text, cx)
    }
}

/// Roles a user (or agent) can act on. Elements with these roles are never
/// removed, and neither is anything carrying `[cursor=pointer]`, `[active]` or
/// a widget state (`checked`, `expanded`, `pressed`, `selected`).
const INTERACTIVE_ROLES: &[&str] = &[
    "button",
    "checkbox",
    "combobox",
    "gridcell",
    "iframe",
    "link",
    "listbox",
    "menuitem",
    "menuitemcheckbox",
    "menuitemradio",
    "option",
    "radio",
    "scrollbar",
    "searchbox",
    "slider",
    "spinbutton",
    "switch",
    "tab",
    "textbox",
    "treeitem",
];

/// Roles where `[cursor=pointer]` adds no information.
const POINTER_IMPLIED: &[&str] = &[
    "button",
    "checkbox",
    "link",
    "menuitem",
    "menuitemcheckbox",
    "menuitemradio",
    "option",
    "radio",
    "switch",
    "tab",
    "treeitem",
];

const STATE_ATTRS: &[&str] = &["checked", "expanded", "pressed", "selected", "active"];

/// Purely structural / textual roles. Nobody clicks, fills, hovers or
/// screenshots these by ref, so conservative mode drops their `[ref=...]`
/// (refs are ~30% of snapshot tokens). Headings, images, landmarks and
/// anything interactive keep theirs.
const STRUCTURAL_ROLES: &[&str] = &[
    "blockquote",
    "caption",
    "cell",
    "code",
    "definition",
    "deletion",
    "emphasis",
    "generic",
    "group",
    "insertion",
    "list",
    "listitem",
    "mark",
    "paragraph",
    "row",
    "rowgroup",
    "separator",
    "strong",
    "subscript",
    "superscript",
    "term",
    "text",
    "time",
];

/// Outputs smaller than this are left alone: nothing worth saving, and the
/// marker would eat the gain.
const MIN_INPUT_BYTES: usize = 4096;

pub fn is_interactive_role(role: &str) -> bool {
    INTERACTIVE_ROLES.contains(&role)
}

struct Limits {
    flatten_all_generics: bool,
    /// Shorten inline texts longer than this many chars...
    text_max: usize,
    /// ...down to this many.
    text_keep: usize,
    /// Dedupe runs of at least this many similar siblings...
    dedupe_min_run: usize,
    /// ...keeping this many.
    dedupe_keep: usize,
    /// Only dedupe siblings that have children of their own (rows, cards).
    dedupe_composite_only: bool,
    refs: RefPolicy,
    drop_urls: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum RefPolicy {
    /// Strip refs from non-interactive elements with a structural role.
    StripStructural,
    /// Strip refs from every non-interactive element.
    StripAllInert,
}

impl Limits {
    fn for_mode(mode: Mode) -> Limits {
        match mode {
            Mode::Conservative => Limits {
                flatten_all_generics: false,
                // Only true blobs: paragraphs are content agents read.
                text_max: 1200,
                text_keep: 1000,
                dedupe_min_run: 20,
                dedupe_keep: 10,
                dedupe_composite_only: true,
                refs: RefPolicy::StripStructural,
                drop_urls: false,
            },
            Mode::Aggressive => Limits {
                flatten_all_generics: true,
                text_max: 200,
                text_keep: 150,
                dedupe_min_run: 6,
                dedupe_keep: 3,
                dedupe_composite_only: false,
                refs: RefPolicy::StripAllInert,
                drop_urls: true,
            },
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// `- role "name" [attrs]: value`
    Element,
    /// `- /url: ...` — a property of its parent element.
    Prop,
    /// Anything we could not parse. Kept verbatim, never removed.
    Opaque,
}

const NONE: usize = usize::MAX;

struct Node<'a> {
    depth: usize,
    /// The line without indentation, starting at `- `.
    line: &'a str,
    kind: Kind,
    role: Cow<'a, str>,
    named: bool,
    interactive: bool,
    /// Editable spans, relative to `line`. Only set for unquoted lines.
    value: Option<Range<usize>>,
    cursor: Option<Range<usize>>,
    ref_attr: Option<Range<usize>>,
    has_value: bool,
    parent: usize,
    n_children: usize,
    /// One past the last node of this subtree (nodes are in preorder).
    end: usize,
    /// Self or any descendant is interactive.
    keeps: bool,
    /// Structural signature (role + child structure, text ignored).
    sig: u64,
}

/// Locate the yaml body: bytes between "```yaml\n" and the "```" fence line.
pub fn find_block(text: &str) -> Option<Range<usize>> {
    const OPEN: &str = "### Snapshot\n```yaml\n";
    let start = text.find(OPEN)? + OPEN.len();
    let mut off = 0;
    for line in text[start..].split_inclusive('\n') {
        if line.trim_end_matches('\n') == "```" {
            // The body excludes the '\n' before the fence, so `text[end..]`
            // starts with "\n```".
            return (off > 0).then(|| start..start + off - 1);
        }
        off += line.len();
    }
    None
}

pub fn trim_snapshot(text: &str, cx: &TrimContext) -> Option<Trimmed> {
    trim_snapshot_min(text, cx, MIN_INPUT_BYTES)
}

/// [`trim_snapshot`] with an explicit size floor (tests use 0 so small,
/// readable inputs exercise every rule).
#[doc(hidden)]
pub fn trim_snapshot_min(text: &str, cx: &TrimContext, min_bytes: usize) -> Option<Trimmed> {
    if text.len() < min_bytes {
        return None;
    }
    let block = find_block(text)?;
    let body = &text[block.clone()];
    if body.contains('\r') {
        return None; // unexpected line endings: don't risk it
    }
    let limits = Limits::for_mode(cx.mode);
    let mut nodes = parse(body)?;
    analyze(&mut nodes);

    let n = nodes.len();
    let mut drop = vec![false; n];
    let mut collapse = vec![false; n];
    // marker[i] = Some(count) → emit "…and N more similar" in place of node i
    let mut marker: Vec<Option<usize>> = vec![None; n];

    for (i, nd) in nodes.iter().enumerate() {
        if limits.drop_urls && nd.kind == Kind::Prop && nd.line.starts_with("- /url:") {
            drop[i] = true;
            continue;
        }
        if nd.kind == Kind::Element && nd.role == "text" && is_separator_text(nd) {
            drop[i] = true; // `- text: "|"` and friends
            continue;
        }
        if nd.kind != Kind::Element || nd.interactive || nd.named || nd.has_value {
            continue;
        }
        if nd.role == "generic"
            && nd.n_children > 0
            && (nd.n_children == 1 || limits.flatten_all_generics)
        {
            // A wrapper: removing its line keeps all of its children,
            // interactive or not.
            collapse[i] = true;
        } else if nd.n_children == 0 {
            // Empty and inert (no name, no value, no children). `interactive`
            // is false, and with no children nothing below it can keep it.
            drop[i] = true;
        }
    }

    dedupe(&nodes, &limits, &mut drop, &mut marker);

    // Emit.
    let mut out = String::with_capacity(text.len());
    out.push_str(&text[..block.start]);
    let mut shift = vec![0usize; n]; // collapsed ancestors of each node
    let mut nodes_trimmed = 0;
    let mut texts_shortened = 0;
    let mut refs_stripped = 0;
    let mut any_edit = false;
    let mut i = 0;
    while i < n {
        let nd = &nodes[i];
        let s = if nd.parent == NONE {
            0
        } else {
            shift[nd.parent] + usize::from(collapse[nd.parent])
        };
        shift[i] = s;
        if drop[i] {
            if let Some(count) = marker[i] {
                push_indent(&mut out, nd.depth - s);
                let _ = writeln!(out, "- text: \"…and {count} more similar {}\"", nd.role);
            }
            nodes_trimmed += nd.end - i;
            i = nd.end;
            continue;
        }
        if collapse[i] {
            nodes_trimmed += 1;
            i += 1;
            continue;
        }
        push_indent(&mut out, nd.depth - s);
        let e = emit_line(&mut out, nd, &limits);
        texts_shortened += usize::from(e.shortened);
        refs_stripped += usize::from(e.ref_stripped);
        any_edit |= e.cursor_stripped;
        out.push('\n');
        i += 1;
    }
    if nodes_trimmed == 0 && texts_shortened == 0 && refs_stripped == 0 && !any_edit {
        return None;
    }
    if out.len() == block.start {
        return None; // everything was removed: show the original instead
    }
    // `out` ends with '\n'; the original text resumes at that same '\n'.
    out.pop();
    out.push_str(&text[block.end..]);
    if !out.ends_with('\n') {
        out.push('\n');
    }
    out.push_str("[meiose: ");
    let _ = write!(out, "{nodes_trimmed} nodes trimmed");
    if texts_shortened > 0 {
        let _ = write!(out, ", {texts_shortened} long texts shortened");
    }
    if refs_stripped > 0 {
        let _ = write!(out, ", {refs_stripped} non-interactive refs hidden");
    }
    let _ = write!(
        out,
        " — call meiose_expand(\"{}\") for the full snapshot]",
        cx.expand_id
    );
    if out.len() >= text.len() {
        return None; // the marker would eat the savings
    }
    Some(Trimmed {
        text: out,
        nodes_trimmed,
        texts_shortened,
    })
}

fn push_indent(out: &mut String, depth: usize) {
    for _ in 0..depth {
        out.push_str("  ");
    }
}

#[derive(Default)]
struct Edits {
    shortened: bool,
    ref_stripped: bool,
    cursor_stripped: bool,
}

/// Write `nd.line` with edits applied.
fn emit_line(out: &mut String, nd: &Node, limits: &Limits) -> Edits {
    let line = nd.line;
    let mut edits = Edits::default();
    // Up to two removals inside the key; both lie before the value.
    let cursor = nd
        .cursor
        .clone()
        .filter(|_| POINTER_IMPLIED.contains(&nd.role.as_ref()));
    let ref_attr = nd.ref_attr.clone().filter(|_| {
        !nd.interactive
            && match limits.refs {
                RefPolicy::StripStructural => STRUCTURAL_ROLES.contains(&nd.role.as_ref()),
                RefPolicy::StripAllInert => true,
            }
    });
    edits.cursor_stripped = cursor.is_some();
    edits.ref_stripped = ref_attr.is_some();
    let mut cuts = [cursor, ref_attr];
    if let [Some(a), Some(b)] = &cuts
        && b.start < a.start
    {
        cuts.swap(0, 1);
    }

    let (head_end, value) = match nd.value.clone() {
        Some(v) if char_count_exceeds(&line[v.clone()], limits.text_max) => (v.start, Some(v)),
        _ => (line.len(), None),
    };

    let mut pos = 0;
    for c in cuts.iter().flatten() {
        out.push_str(&line[pos..c.start]);
        pos = c.end;
    }
    out.push_str(&line[pos..head_end]);
    if let Some(v) = value {
        push_shortened(out, &line[v], limits.text_keep);
        edits.shortened = true;
    }
    edits
}

fn char_count_exceeds(s: &str, max: usize) -> bool {
    s.len() > max && s.chars().nth(max).is_some()
}

/// Write the first `keep` chars of a YAML scalar plus an ellipsis, keeping a
/// double-quoted scalar well-formed (never cut inside an escape sequence).
fn push_shortened(out: &mut String, v: &str, keep: usize) {
    let total = v.chars().count();
    let quoted = v.len() >= 2 && v.starts_with('"') && v.ends_with('"');
    let inner = if quoted { &v[1..v.len() - 1] } else { v };
    let mut cut = inner
        .char_indices()
        .nth(keep)
        .map(|(b, _)| b)
        .unwrap_or(inner.len());
    if quoted {
        // Back off so we do not end on an unpaired backslash.
        let trailing = inner[..cut]
            .bytes()
            .rev()
            .take_while(|&b| b == b'\\')
            .count();
        if trailing % 2 == 1 {
            cut -= 1;
        }
        // Also back off out of a \uXXXX / \xXX escape we may be inside of.
        if let Some(bs) = inner[..cut].rfind('\\')
            && cut - bs < 10
            && inner[..bs]
                .bytes()
                .rev()
                .take_while(|&b| b == b'\\')
                .count()
                % 2
                == 0
        {
            cut = bs;
        }
    }
    let dropped = total - inner[..cut].chars().count() - if quoted { 2 } else { 0 };
    if quoted {
        out.push('"');
    }
    out.push_str(&inner[..cut]);
    let _ = write!(out, "… (+{dropped} chars)");
    if quoted {
        out.push('"');
    }
}

fn parse(body: &str) -> Option<Vec<Node<'_>>> {
    let mut nodes: Vec<Node> = Vec::with_capacity(body.len() / 40);
    // stack of open node indices by depth
    let mut stack: Vec<usize> = Vec::new();
    for raw in body.split('\n') {
        let trimmed = raw.trim_start_matches(' ');
        let indent = raw.len() - trimmed.len();
        if indent % 2 != 0 || !trimmed.starts_with("- ") {
            return None; // not the format we know: leave the snapshot alone
        }
        let depth = indent / 2;
        if depth > stack.len() {
            return None; // skipped a level
        }
        while stack.len() > depth {
            let done = stack.pop().expect("non-empty");
            nodes[done].end = nodes.len();
        }
        let parent = stack.last().copied().unwrap_or(NONE);
        let idx = nodes.len();
        nodes.push(parse_line(trimmed, depth, parent));
        if parent != NONE {
            nodes[parent].n_children += 1;
        }
        stack.push(idx);
    }
    let len = nodes.len();
    for i in stack {
        nodes[i].end = len;
    }
    Some(nodes)
}

fn parse_line(line: &str, depth: usize, parent: usize) -> Node<'_> {
    let body = &line[2..];
    let mut nd = Node {
        depth,
        line,
        kind: Kind::Opaque,
        role: Cow::Borrowed(""),
        named: false,
        interactive: true,
        value: None,
        cursor: None,
        ref_attr: None,
        has_value: false,
        parent,
        n_children: 0,
        end: 0,
        keeps: true,
        sig: 0,
    };
    if body.starts_with('/') {
        nd.kind = Kind::Prop;
        nd.interactive = false;
        nd.role = Cow::Borrowed("/prop");
        return nd;
    }
    if let Some(quoted) = body.strip_prefix('\'') {
        // YAML single-quoted key; '' is an escaped quote.
        let bytes = quoted.as_bytes();
        let mut j = 0;
        let close = loop {
            match bytes.get(j) {
                None => return nd, // unterminated: opaque
                Some(b'\'') if bytes.get(j + 1) == Some(&b'\'') => j += 2,
                Some(b'\'') => break j,
                Some(_) => j += 1,
            }
        };
        let key_raw = &quoted[..close];
        let key: Cow<str> = if key_raw.contains("''") {
            Cow::Owned(key_raw.replace("''", "'"))
        } else {
            Cow::Borrowed(key_raw)
        };
        let rest = &quoted[close + 1..];
        if !(rest.is_empty() || rest == ":" || rest.starts_with(": ")) {
            return nd;
        }
        let Some(k) = parse_key(&key) else {
            return nd;
        };
        nd.kind = Kind::Element;
        nd.role = match key {
            Cow::Borrowed(b) => Cow::Borrowed(&b[..k.role_end]),
            Cow::Owned(o) => Cow::Owned(o[..k.role_end].to_string()),
        };
        nd.named = k.named;
        nd.interactive = k.interactive || is_interactive_role(&nd.role);
        nd.has_value = rest.len() > 2;
        // Quoted lines are never edited, so no spans.
        return nd;
    }
    let Some(k) = parse_key(body) else {
        return nd;
    };
    let rest = &body[k.key_end..];
    let value_start = if rest.is_empty() || rest == ":" {
        None
    } else if let Some(v) = rest.strip_prefix(": ") {
        (!v.is_empty()).then_some(2 + k.key_end + 2)
    } else {
        return nd;
    };
    nd.kind = Kind::Element;
    nd.role = Cow::Borrowed(&body[..k.role_end]);
    nd.named = k.named;
    nd.interactive = k.interactive || is_interactive_role(&nd.role);
    nd.has_value = value_start.is_some();
    nd.value = value_start.map(|s| s..line.len());
    nd.cursor = k.cursor.map(|r| r.start + 2..r.end + 2);
    nd.ref_attr = k.ref_attr.map(|r| r.start + 2..r.end + 2);
    nd
}

struct Key {
    role_end: usize,
    named: bool,
    interactive: bool,
    /// Span of " [cursor=pointer]" including the leading space.
    cursor: Option<Range<usize>>,
    /// Span of " [ref=...]" including the leading space.
    ref_attr: Option<Range<usize>>,
    key_end: usize,
}

/// Parse `role "name" [attr] [attr=value]` at the start of `s`.
fn parse_key(s: &str) -> Option<Key> {
    let b = s.as_bytes();
    let role_end = b.iter().take_while(|c| c.is_ascii_lowercase()).count();
    if role_end == 0 {
        return None;
    }
    let mut i = role_end;
    let mut named = false;
    if b.get(i) == Some(&b' ') && b.get(i + 1) == Some(&b'"') {
        let mut j = i + 2;
        loop {
            match b.get(j)? {
                b'\\' => j += 2,
                b'"' => break,
                _ => j += 1,
            }
        }
        named = j > i + 2;
        i = j + 1;
    }
    let mut interactive = false;
    let mut cursor = None;
    let mut ref_attr = None;
    while b.get(i) == Some(&b' ') && b.get(i + 1) == Some(&b'[') {
        let close = i + 2 + s[i + 2..].find(']')?;
        let attr = &s[i + 2..close];
        let name = attr.split('=').next().unwrap_or("");
        if attr == "cursor=pointer" {
            interactive = true;
            cursor = Some(i..close + 1);
        } else if name == "ref" {
            ref_attr = Some(i..close + 1);
        } else if STATE_ATTRS.contains(&name) {
            interactive = true;
        }
        i = close + 1;
    }
    Some(Key {
        role_end,
        named,
        interactive,
        cursor,
        ref_attr,
        key_end: i,
    })
}

/// Compute `keeps` and `sig` bottom-up. Nodes are in preorder, so walking
/// backwards visits every child before its parent.
fn analyze(nodes: &mut [Node]) {
    let n = nodes.len();
    let mut acc = vec![0u64; n];
    let mut child_keeps = vec![false; n];
    for i in (0..n).rev() {
        let nd = &mut nodes[i];
        nd.sig = mix((fnv(nd.role.as_bytes()) ^ nd.kind as u64).wrapping_add(acc[i]));
        // Props never force their parent to stay; opaque lines always do.
        let self_keeps = match nd.kind {
            Kind::Opaque => true,
            Kind::Prop => false,
            Kind::Element => nd.interactive,
        };
        nd.keeps = self_keeps || child_keeps[i];
        let (p, sig, keeps) = (nd.parent, nd.sig, nd.keeps);
        if p != NONE {
            acc[p] = acc[p].wrapping_add(sig);
            child_keeps[p] |= keeps;
        }
    }
}

fn dedupe(nodes: &[Node], limits: &Limits, drop: &mut [bool], marker: &mut [Option<usize>]) {
    let n = nodes.len();
    // Each sibling list: the root list, plus every node with enough children.
    let mut lists: Vec<(usize, usize)> = vec![(0, n)];
    lists.extend(
        (0..n)
            .filter(|&i| nodes[i].n_children >= limits.dedupe_min_run)
            .map(|i| (i + 1, nodes[i].end)),
    );
    let mut children = Vec::new();
    for (start, end) in lists {
        children.clear();
        let mut j = start;
        while j < end {
            // Nodes already removed (empty, separators) neither count toward
            // a run nor deserve an "…and N more" line.
            if !drop[j] {
                children.push(j);
            }
            j = nodes[j].end;
        }
        let mut i = 0;
        while i < children.len() {
            let first = &nodes[children[i]];
            if !eligible(first, limits) {
                i += 1;
                continue;
            }
            let mut k = i + 1;
            while k < children.len()
                && eligible(&nodes[children[k]], limits)
                && nodes[children[k]].sig == first.sig
            {
                k += 1;
            }
            if k - i >= limits.dedupe_min_run {
                let dropped = &children[i + limits.dedupe_keep..k];
                for &d in dropped {
                    drop[d] = true;
                }
                marker[dropped[0]] = Some(dropped.len());
            }
            i = k;
        }
    }
}

/// A text node whose value is only separator punctuation (`|`, `·`, `•`, …).
fn is_separator_text(nd: &Node) -> bool {
    let Some(v) = &nd.value else { return false };
    let v = nd.line[v.clone()].trim_matches('"').trim();
    !v.is_empty()
        && v.chars().count() <= 3
        && v.chars()
            .all(|c| matches!(c, '|' | '·' | '•' | '/' | '-' | '–' | '—' | ',' | ' '))
}

fn eligible(nd: &Node, limits: &Limits) -> bool {
    nd.kind == Kind::Element && !nd.keeps && (!limits.dedupe_composite_only || nd.n_children > 0)
}

fn fnv(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h
}

fn mix(mut x: u64) -> u64 {
    x ^= x >> 33;
    x = x.wrapping_mul(0xff51_afd7_ed55_8ccd);
    x ^= x >> 33;
    x
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap(yaml: &str) -> String {
        format!("### Page\n- Page URL: https://x.test/\n### Snapshot\n```yaml\n{yaml}\n```\n")
    }

    /// Empty inert root-level nodes appended to every example. They are always
    /// removed (so they never show up in `body()`), and they make trimming
    /// worth more than the marker costs, which the handler requires.
    const PAD: usize = 20;

    fn padded(yaml: &str) -> String {
        let mut y = yaml.to_string();
        for i in 0..PAD {
            y.push_str(&format!("\n- generic [ref=pad{i}]"));
        }
        y
    }

    fn trim(yaml: &str, mode: Mode) -> Option<String> {
        let cx = TrimContext {
            expand_id: "id1",
            mode,
        };
        trim_snapshot_min(&snap(&padded(yaml)), &cx, 0).map(|t| t.text)
    }

    #[test]
    fn padding_alone_is_removed_entirely() {
        let out = trim("- link \"x\" [ref=e1]", Mode::Conservative).unwrap();
        assert_eq!(body(&out), "- link \"x\" [ref=e1]");
    }

    #[test]
    fn output_never_larger_than_input() {
        // One empty node saves less than the marker costs: leave it alone.
        let cx = TrimContext {
            expand_id: "id1",
            mode: Mode::Conservative,
        };
        let text = snap("- link \"x\" [ref=e1]\n- generic [ref=e2]");
        assert!(trim_snapshot_min(&text, &cx, 0).is_none());
    }

    fn body(out: &str) -> &str {
        let s = out.find("```yaml\n").unwrap() + 8;
        let e = out[s..].find("\n```").unwrap() + s;
        &out[s..e]
    }

    #[test]
    fn collapses_single_child_generic_chain() {
        let out = trim(
            "- generic [ref=e1]:\n  - generic [ref=e2]:\n    - generic [ref=e3]:\n      - button \"Go\" [ref=e4] [cursor=pointer]",
            Mode::Conservative,
        )
        .unwrap();
        assert_eq!(body(&out), "- button \"Go\" [ref=e4]");
        // 3 wrappers + the padding nodes.
        assert!(out.ends_with(&format!(
            "[meiose: {} nodes trimmed — call meiose_expand(\"id1\") for the full snapshot]",
            3 + PAD
        )));
    }

    #[test]
    fn keeps_multi_child_generic_but_hides_its_ref() {
        let out = trim(
            "- generic [ref=e1]:\n  - link \"a\" [ref=e2] [cursor=pointer]\n  - link \"b\" [ref=e3]",
            Mode::Conservative,
        )
        .unwrap();
        assert_eq!(
            body(&out),
            "- generic:\n  - link \"a\" [ref=e2]\n  - link \"b\" [ref=e3]"
        );
    }

    #[test]
    fn drops_empty_inert_nodes_but_never_interactive_ones() {
        let out = trim(
            "- list [ref=e1]:\n  - img [ref=e2]\n  - generic [ref=e3]\n  - button [ref=e4]\n  - textbox [ref=e5]\n  - generic [ref=e6] [cursor=pointer]\n  - text: hi",
            Mode::Conservative,
        )
        .unwrap();
        assert_eq!(
            body(&out),
            "- list:\n  - button [ref=e4]\n  - textbox [ref=e5]\n  - generic [ref=e6] [cursor=pointer]\n  - text: hi"
        );
    }

    #[test]
    fn heading_and_img_refs_survive_in_conservative_mode() {
        let out = trim(
            "- generic [ref=e1]:\n  - heading \"T\" [level=1] [ref=e2]\n  - img \"logo\" [ref=e3]\n  - paragraph [ref=e4]: text",
            Mode::Conservative,
        )
        .unwrap();
        assert_eq!(
            body(&out),
            "- generic:\n  - heading \"T\" [level=1] [ref=e2]\n  - img \"logo\" [ref=e3]\n  - paragraph: text"
        );
    }

    #[test]
    fn aggressive_strips_all_inert_refs_and_urls() {
        let out = trim(
            "- main [ref=e1]:\n  - heading \"T\" [level=1] [ref=e2]\n  - link \"x\" [ref=e3]:\n    - /url: /x\n  - generic [ref=e4]:\n    - paragraph [ref=e5]: a\n    - paragraph [ref=e6]: b",
            Mode::Aggressive,
        )
        .unwrap();
        assert_eq!(
            body(&out),
            "- main:\n  - heading \"T\" [level=1]\n  - link \"x\" [ref=e3]:\n  - paragraph: a\n  - paragraph: b"
        );
    }

    #[test]
    fn separator_texts_are_dropped() {
        let out = trim(
            "- generic [ref=e1]:\n  - link \"a\" [ref=e2]\n  - text: \"|\"\n  - link \"b\" [ref=e3]\n  - text: \"not | a separator\"",
            Mode::Conservative,
        )
        .unwrap();
        assert_eq!(
            body(&out),
            "- generic:\n  - link \"a\" [ref=e2]\n  - link \"b\" [ref=e3]\n  - text: \"not | a separator\""
        );
    }

    #[test]
    fn dedupes_long_runs_of_inert_composite_siblings() {
        let mut y = String::from("- table [ref=t]:\n  - rowgroup [ref=g]:\n");
        for i in 0..25 {
            y.push_str(&format!(
                "    - row [ref=r{i}]:\n      - cell [ref=c{i}]: v{i}\n"
            ));
        }
        y.pop();
        let out = trim(&y, Mode::Conservative).unwrap();
        let b = body(&out);
        assert!(b.contains("cell: v9"));
        assert!(!b.contains("cell: v10"));
        assert!(b.contains("- text: \"…and 15 more similar row\""), "{b}");
    }

    #[test]
    fn never_dedupes_siblings_with_interactive_content() {
        let mut y = String::from("- list [ref=l]:\n");
        for i in 0..30 {
            y.push_str(&format!(
                "  - listitem [ref=i{i}]:\n    - link \"n{i}\" [ref=a{i}]\n"
            ));
        }
        y.pop();
        let out = trim(&y, Mode::Aggressive).unwrap();
        for i in 0..30 {
            assert!(out.contains(&format!("[ref=a{i}]")), "lost link {i}");
        }
        assert!(!out.contains("more similar"));
    }

    #[test]
    fn quoted_entries_are_parsed_and_kept_verbatim() {
        let y = "- generic [ref=e1]:\n  - 'link \"A: B''s\" [ref=e2] [cursor=pointer]':\n    - /url: /a\n  - 'generic \"C: D\" [ref=e3]'";
        let out = trim(y, Mode::Conservative).unwrap();
        assert_eq!(
            body(&out),
            "- generic:\n  - 'link \"A: B''s\" [ref=e2] [cursor=pointer]':\n    - /url: /a\n  - 'generic \"C: D\" [ref=e3]'"
        );
    }

    #[test]
    fn shortens_blobs_without_breaking_quoted_scalars() {
        let long = format!("\"{}\\\"{}\"", "a".repeat(990), "b".repeat(500));
        let y = format!(
            "- generic [ref=e1]:\n  - paragraph [ref=e2]: {long}\n  - paragraph [ref=e3]: short"
        );
        let out = trim(&y, Mode::Conservative).unwrap();
        let line = body(&out).lines().nth(1).unwrap();
        assert!(line.starts_with("  - paragraph: \"aaa"));
        assert!(line.ends_with(" chars)\""), "{line}");
        // The cut never leaves a dangling backslash before the closing quote.
        let inner = &line[line.find('"').unwrap() + 1..line.len() - 1];
        assert!(!inner.ends_with('\\'));
        assert!(out.contains("1 long texts shortened"));
    }

    #[test]
    fn unknown_format_is_left_alone() {
        for y in [
            "- generic [ref=e1]:\n   - odd indent",
            "- generic [ref=e1]:\n    - skipped level",
            "not a list item",
            "- generic [ref=e1]:\r\n  - link [ref=e2]",
        ] {
            assert!(trim(y, Mode::Aggressive).is_none(), "{y:?}");
        }
        let cx = TrimContext {
            expand_id: "x",
            mode: Mode::Aggressive,
        };
        assert!(trim_snapshot_min("no snapshot here", &cx, 0).is_none());
        assert!(trim_snapshot_min("### Snapshot\n```yaml\n```\n", &cx, 0).is_none());
    }

    #[test]
    fn unparseable_lines_are_kept_and_protect_their_ancestors() {
        let out = trim(
            "- list [ref=e1]:\n  - listitem [ref=e2]:\n    - weird [ref=e3] trailing junk\n  - generic [ref=e4]",
            Mode::Aggressive,
        )
        .unwrap();
        assert_eq!(
            body(&out),
            "- list:\n  - listitem:\n    - weird [ref=e3] trailing junk"
        );
    }

    #[test]
    fn text_outside_the_block_is_untouched() {
        let yaml = padded("- generic [ref=e1]:\n  - link \"x\" [ref=e2]");
        let text = format!(
            "### Page\n- Page URL: https://a\n### Snapshot\n```yaml\n{yaml}\n```\n### Events\n- New console entries: x.log\n"
        );
        let cx = TrimContext {
            expand_id: "z",
            mode: Mode::Conservative,
        };
        let out = trim_snapshot_min(&text, &cx, 0).unwrap().text;
        assert!(out.starts_with("### Page\n- Page URL: https://a\n### Snapshot\n```yaml\n- link \"x\" [ref=e2]\n```\n### Events\n- New console entries: x.log\n[meiose: "));
    }

    #[test]
    fn small_inputs_are_not_touched_by_default() {
        let cx = TrimContext {
            expand_id: "z",
            mode: Mode::Aggressive,
        };
        assert!(trim_snapshot(&snap("- generic [ref=e1]:\n  - link [ref=e2]"), &cx).is_none());
    }
}
