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
//! The header line before the fence has been `### Snapshot` since Playwright
//! MCP 1.5x; older releases wrote `- Page Snapshot` (≤ 0.0.29) or
//! `- Page Snapshot:` (0.0.3x), and all three are accepted. CRLF line endings
//! are normalised to LF before trimming.
//!
//! Everything outside the yaml block is kept verbatim. Inside it, each line
//! is parsed just enough to know its depth, role, name, attributes and inline
//! value. A line that does not parse is kept verbatim and treated as
//! interactive, so it (and its ancestors) can never be removed. A block that
//! does not parse at all (odd indentation, a skipped level) is left alone,
//! and [`looks_like_snapshot`] keeps the other handlers off it too.
//!
//! Rules, all of which preserve every interactive element and its ref:
//! - collapse unnamed, non-interactive `generic` wrappers that have a single
//!   child (aggressive: any number of children), promoting the children;
//! - drop empty nodes: no name, no value, no children, nothing interactive —
//!   except table cells, whose position in the row is their meaning;
//! - dedupe long runs of structurally identical siblings that contain nothing
//!   interactive, leaving a few and an "…and N more similar" line;
//! - shorten very long inline texts;
//! - remove `[cursor=pointer]` where the role already implies it;
//! - aggressive only: strip refs from non-interactive elements.

use std::borrow::Cow;
use std::collections::HashSet;
use std::fmt::Write as _;
use std::hash::{BuildHasherDefault, Hasher};
use std::ops::Range;
use std::sync::atomic::{AtomicBool, Ordering};

use super::{Handler, Mode, TrimContext, Trimmed};

pub struct Playwright;

impl Handler for Playwright {
    fn name(&self) -> &'static str {
        "playwright"
    }

    fn trim(&self, text: &str, cx: &TrimContext) -> Option<Trimmed> {
        trim_snapshot(text, cx)
    }

    fn claims(&self, text: &str) -> bool {
        looks_like_snapshot(text)
    }
}

/// Does `text` look like a Playwright snapshot, whether or not this handler
/// can parse it: a ```yaml fence holding a `- … [ref=…]` list line? Such an
/// output must never be cut by the generic handlers. (JSON that quotes a
/// snapshot inside a string has no fence line of its own and does not count.)
pub fn looks_like_snapshot(text: &str) -> bool {
    if !text.contains("```yaml") {
        return false;
    }
    let mut in_fence = false;
    for line in text.lines() {
        let line = line.trim_end_matches('\r');
        if !in_fence {
            in_fence = line == "```yaml";
            continue;
        }
        if line == "```" {
            in_fence = false;
            continue;
        }
        let item = line.trim_start_matches(' ');
        if item.starts_with("- ") && item.contains("[ref=") {
            return true;
        }
    }
    false
}

/// Logged once per process: a format change upstream would otherwise repeat
/// it on every call. The output itself is forwarded whole either way (see
/// `Handler::claims`), so this is diagnostic only.
fn warn_unparsed(bytes: usize, reason: &str) {
    static WARNED: AtomicBool = AtomicBool::new(false);
    if !WARNED.swap(true, Ordering::Relaxed) {
        tracing::warn!(
            bytes,
            reason,
            "a Playwright snapshot could not be trimmed safely and was forwarded untouched (logged \
             once per session). If this keeps happening the server's output format may have \
             changed: capture a session with `meiose record` and report it at \
             https://github.com/yousufmohi/meiose/issues"
        );
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

/// Table cells are kept even when empty: a row is read by position, so a
/// missing cell shifts every value after it into the wrong column.
const TABLE_CELL_ROLES: &[&str] = &["cell", "columnheader", "gridcell", "rowheader"];

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
    /// Siblings must match including their text, not just their structure.
    /// Conservative: only truly repeated UI is collapsed, never unique data
    /// (the eval showed structural dedupe hiding the row the agent needed).
    dedupe_same_text: bool,
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
                dedupe_same_text: true,
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
                dedupe_same_text: false,
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

/// The line Playwright MCP writes just before the yaml fence, by release:
/// `### Snapshot` (1.5x, current), `- Page Snapshot:` (0.0.3x) and
/// `- Page Snapshot` (0.0.29 and older).
const HEADERS: &[&str] = &["### Snapshot", "- Page Snapshot:", "- Page Snapshot"];

/// Locate the yaml body: bytes between the "```yaml" line that follows a
/// known header line and the "```" fence line. A body is never returned
/// with a carriage return in it (the diff and outline code counts on LF
/// lines); [`trim_snapshot_min`] normalises CRLF before calling this.
pub fn find_block(text: &str) -> Option<Range<usize>> {
    let mut off = 0;
    let mut prev = "";
    let mut start = None;
    for line in text.split_inclusive('\n') {
        let content = line.trim_end_matches('\n').trim_end_matches('\r');
        match start {
            None if content == "```yaml" && HEADERS.contains(&prev) => {
                start = Some(off + line.len());
            }
            Some(s) if content == "```" => {
                // The body excludes the '\n' before the fence, so `text[end..]`
                // starts with "\n```".
                return (off > s && !text[s..off - 1].contains('\r')).then(|| s..off - 1);
            }
            _ => {}
        }
        prev = content;
        off += line.len();
    }
    None
}

pub fn trim_snapshot(text: &str, cx: &TrimContext) -> Option<Trimmed> {
    trim_snapshot_min(text, cx, MIN_INPUT_BYTES)
}

/// `text` with CRLF line endings turned into LF, whole text, so everything
/// that works on a snapshot (trimming, the budget outline, diffs) sees one
/// line ending throughout. Never larger than the input.
pub fn normalized(text: &str) -> Cow<'_, str> {
    if text.contains("\r\n") {
        Cow::Owned(text.replace("\r\n", "\n"))
    } else {
        Cow::Borrowed(text)
    }
}

/// [`trim_snapshot`] with an explicit size floor (tests use 0 so small,
/// readable inputs exercise every rule).
#[doc(hidden)]
pub fn trim_snapshot_min(text: &str, cx: &TrimContext, min_bytes: usize) -> Option<Trimmed> {
    if text.len() < min_bytes {
        return None;
    }
    let normalized = normalized(text);
    let text: &str = &normalized;
    let Some(block) = find_block(text) else {
        if looks_like_snapshot(text) {
            warn_unparsed(
                text.len(),
                "no known header line before the yaml fence, or a stray carriage return",
            );
        }
        return None;
    };
    let body = &text[block.clone()];
    let limits = Limits::for_mode(cx.mode);
    let Some(mut nodes) = parse(body) else {
        warn_unparsed(text.len(), "a line with an odd indent or a skipped level");
        return None;
    };
    analyze(&mut nodes, limits.dedupe_same_text, limits.dedupe_min_run);

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
        } else if nd.n_children == 0 && !TABLE_CELL_ROLES.contains(&nd.role.as_ref()) {
            // Empty and inert (no name, no value, no children). `interactive`
            // is false, and with no children nothing below it can keep it.
            // An empty table cell stays: its position is its meaning.
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
    let mut urls_hidden = 0;
    // Only URLs actually emitted count as seen, so a URL is never lost.
    let mut seen_urls: HashSet<&str, BuildHasherDefault<WordHasher>> = HashSet::default();
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
        if is_url(nd) && !seen_urls.insert(nd.line) {
            urls_hidden += 1; // already shown once on this page
            i += 1;
            continue;
        }
        push_indent(&mut out, nd.depth - s);
        let e = emit_line(&mut out, nd, &limits);
        texts_shortened += usize::from(e.shortened);
        refs_stripped += usize::from(e.ref_stripped);
        any_edit |= e.cursor_stripped;
        // `- link "x":` whose only child is its URL fits on one line.
        if nd.kind == Kind::Element
            && !nd.has_value
            && nd.n_children == 1
            && nd.line.ends_with(':')
            && !nd.line.starts_with("- '") // quoted lines are never edited
            && nodes.get(i + 1).is_some_and(is_url)
        {
            let url = nodes[i + 1].line;
            if drop[i + 1] {
                out.pop();
                nodes_trimmed += 1;
            } else if seen_urls.insert(url) {
                out.push(' ');
                out.push_str(&url["- /url: ".len()..]);
            } else {
                out.pop();
                urls_hidden += 1;
            }
            any_edit = true;
            out.push('\n');
            i += 2;
            continue;
        }
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
    if urls_hidden > 0 {
        let _ = write!(out, ", {urls_hidden} repeated URLs hidden");
    }
    let _ = write!(
        out,
        " — call meiose_expand(\"{}\") for the full snapshot, or add query=\"…\" for only the lines you need]",
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

/// `fnv` (8 bytes per step) as a `Hasher`: the URL set hashes every URL on
/// the page, and the default SipHash showed up in the trim time. Keys come
/// from the page itself, so hash flooding only slows that one page.
#[derive(Default)]
struct WordHasher(u64);

impl Hasher for WordHasher {
    fn write(&mut self, bytes: &[u8]) {
        self.0 = self.0.rotate_left(7) ^ fnv(bytes);
    }
    fn finish(&self) -> u64 {
        self.0
    }
}

fn is_url(nd: &Node) -> bool {
    nd.kind == Kind::Prop && nd.line.starts_with("- /url: ")
}

/// What an outline keeps first: the page's structure, then its form controls.
const OUTLINE_STRUCTURE: &[&str] = &[
    "banner",
    "contentinfo",
    "form",
    "heading",
    "main",
    "navigation",
    "region",
    "search",
];
const OUTLINE_CONTROLS: &[&str] = &[
    "button",
    "checkbox",
    "combobox",
    "listbox",
    "radio",
    "searchbox",
    "slider",
    "spinbutton",
    "switch",
    "tab",
    "textbox",
];

/// Last resort for a page that is over budget even when trimmed aggressively:
/// structure and form controls first, then the rest of the page in document
/// order until `max_bytes`. Unlike every other rule this CAN omit interactive
/// elements, so its marker says so and points at search.
pub fn outline(snapshot: &str, expand_id: &str, max_bytes: usize) -> Option<String> {
    let block = find_block(snapshot)?;
    let lines: Vec<&str> = snapshot[block.clone()].split('\n').collect();
    let tier = |l: &str| {
        let s = l.trim_start_matches(' ').trim_start_matches("- ");
        let s = s.trim_start_matches('\'');
        let role = &s[..s.bytes().take_while(u8::is_ascii_lowercase).count()];
        if OUTLINE_STRUCTURE.contains(&role) {
            0
        } else if OUTLINE_CONTROLS.contains(&role) {
            1
        } else {
            2
        }
    };
    let mut keep = vec![false; lines.len()];
    let mut used = 0;
    for pass in 0..3 {
        for (i, l) in lines.iter().enumerate() {
            if !keep[i] && tier(l) == pass && used + l.len() < max_bytes {
                keep[i] = true;
                used += l.len() + 1;
            }
        }
    }
    let shown = keep.iter().filter(|k| **k).count();
    if shown == 0 || shown == lines.len() {
        return None;
    }
    let mut out = String::with_capacity(max_bytes + 512);
    out.push_str(&snapshot[..block.start]);
    // Omitted lines can skip levels; clamp so the block stays well-formed.
    let mut prev = None;
    for (i, l) in lines.iter().enumerate() {
        if keep[i] {
            let s = l.trim_start_matches(' ');
            let depth = (l.len() - s.len()) / 2;
            let depth = prev.map_or(0, |p: usize| depth.min(p + 1));
            prev = Some(depth);
            push_indent(&mut out, depth);
            out.push_str(s);
            out.push('\n');
        }
    }
    out.pop();
    for line in snapshot[block.end..].split_inclusive('\n') {
        if !line.starts_with("[meiose: ") {
            out.push_str(line);
        }
    }
    if !out.ends_with('\n') {
        out.push('\n');
    }
    let _ = write!(
        out,
        "[meiose: page too large, outline only ({shown} of {} lines); some links may be missing — \
         call meiose_expand(\"{expand_id}\", query=\"…\") to search the page, or \
         meiose_expand(\"{expand_id}\") for all of it]",
        lines.len()
    );
    (out.len() < snapshot.len()).then_some(out)
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
fn analyze(nodes: &mut [Node], with_text: bool, min_run: usize) {
    let n = nodes.len();
    let mut acc = vec![0u64; n];
    let mut child_keeps = vec![false; n];
    // Text only matters for subtrees that dedupe can look at: children (and
    // their descendants) of lists with at least `min_run` siblings.
    let mut in_long_list = vec![false; n];
    if with_text {
        let roots = nodes.iter().filter(|nd| nd.parent == NONE).count();
        if roots >= min_run {
            in_long_list.fill(true);
        } else {
            for i in 0..n {
                if nodes[i].n_children >= min_run && !in_long_list[i] {
                    let end = nodes[i].end;
                    in_long_list[i + 1..end].fill(true);
                }
            }
        }
    }
    for i in (0..n).rev() {
        let nd = &mut nodes[i];
        let mut base = fnv(nd.role.as_bytes()) ^ nd.kind as u64;
        if in_long_list[i] {
            // The whole line except its ref (refs are unique per element).
            base ^= match &nd.ref_attr {
                Some(r) => {
                    fnv(&nd.line.as_bytes()[..r.start]).rotate_left(17)
                        ^ fnv(&nd.line.as_bytes()[r.end..])
                }
                None => fnv(nd.line.as_bytes()),
            };
        }
        nd.sig = mix(base.wrapping_add(acc[i]));
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

/// Fast non-cryptographic hash over 8-byte words (dedupe signatures hash
/// every line of large snapshots, so a byte-at-a-time hash is too slow).
fn fnv(bytes: &[u8]) -> u64 {
    const K: u64 = 0x9E37_79B9_7F4A_7C15;
    let mut h: u64 = 0xcbf2_9ce4_8422_2325 ^ (bytes.len() as u64).wrapping_mul(K);
    let (words, rest) = bytes.as_chunks::<8>();
    for w in words {
        h = (h.rotate_left(5) ^ u64::from_le_bytes(*w)).wrapping_mul(K);
    }
    let mut tail = [0u8; 8];
    tail[..rest.len()].copy_from_slice(rest);
    h = (h.rotate_left(5) ^ u64::from_le_bytes(tail)).wrapping_mul(K);
    h ^ (h >> 32)
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
            "[meiose: {} nodes trimmed — call meiose_expand(\"id1\") for the full snapshot, or add query=\"…\" for only the lines you need]",
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
    fn empty_table_cells_keep_their_place_in_the_row() {
        // MDN's compatibility table: the corner cell is empty, and so is a
        // "no data" cell. Dropping them would shift every column after them.
        let out = trim(
            "- table [ref=e1]:\n  - row [ref=e2]:\n    - cell [ref=e3]\n    - columnheader \"Chrome\" [ref=e4]\n    - columnheader \"Firefox\" [ref=e5]\n  - row [ref=e6]:\n    - rowheader \"basic\" [ref=e7]\n    - cell [ref=e8]\n    - cell \"0 (no)\" [ref=e9]\n  - generic [ref=e10]",
            Mode::Conservative,
        )
        .unwrap();
        assert_eq!(
            body(&out),
            "- table [ref=e1]:\n  - row:\n    - cell\n    - columnheader \"Chrome\" [ref=e4]\n    - columnheader \"Firefox\" [ref=e5]\n  - row:\n    - rowheader \"basic\" [ref=e7]\n    - cell\n    - cell \"0 (no)\""
        );
        // Aggressive mode keeps them too (and hides every inert ref).
        let out = trim(
            "- table [ref=e1]:\n  - row [ref=e2]:\n    - cell [ref=e3]\n    - cell \"x\" [ref=e4]",
            Mode::Aggressive,
        )
        .unwrap();
        assert_eq!(
            body(&out),
            "- table:\n  - row:\n    - cell\n    - cell \"x\""
        );
    }

    #[test]
    fn older_playwright_headers_and_crlf_are_accepted() {
        let cx = TrimContext {
            expand_id: "id1",
            mode: Mode::Conservative,
        };
        let yaml = padded("- link \"x\" [ref=e1] [cursor=pointer]");
        let modern = snap(&yaml);
        let expected = trim_snapshot_min(&modern, &cx, 0).unwrap().text;
        for header in ["- Page Snapshot", "- Page Snapshot:"] {
            let old = modern.replacen("### Snapshot", header, 1);
            let out = trim_snapshot_min(&old, &cx, 0).unwrap().text;
            assert_eq!(out, expected.replacen("### Snapshot", header, 1));
        }
        // CRLF everywhere → trimmed, and the output is LF throughout.
        let crlf = modern.replace('\n', "\r\n");
        let out = trim_snapshot_min(&crlf, &cx, 0).unwrap().text;
        assert_eq!(out, expected);
        // A lone CR inside the block is still refused.
        let lone = modern.replacen("[ref=e1]", "[ref=e1]\r", 1);
        assert!(trim_snapshot_min(&lone, &cx, 0).is_none());
    }

    #[test]
    fn find_block_needs_a_known_header_right_before_the_fence() {
        let body = "- link \"x\" [ref=e1]";
        for text in [
            format!("### Snapshot\n```yaml\n{body}\n```\n"),
            format!("- Page Snapshot\n```yaml\n{body}\n```\n"),
            format!("### Page state\n- Page Snapshot:\n```yaml\n{body}\n```\n"),
        ] {
            let r = find_block(&text).unwrap_or_else(|| panic!("{text:?}"));
            assert!(text[r.clone()].starts_with(body), "{text:?}");
            assert!(text[r.end..].starts_with("\n```"), "{text:?}");
        }
        assert!(find_block(&format!("### Other\n```yaml\n{body}\n```\n")).is_none());
        assert!(find_block(&format!("### Snapshot\n\n```yaml\n{body}\n```\n")).is_none());
        assert!(find_block("### Snapshot\n```yaml\n```\n").is_none());
        assert!(find_block(&format!("### Snapshot\n```yaml\n{body}\n")).is_none());
        // A body with carriage returns is never returned: the trimmer
        // normalises CRLF first, and nothing else may work on CR lines.
        assert!(find_block(&format!("### Snapshot\r\n```yaml\r\n{body}\r\n```\r\n")).is_none());
        assert!(find_block(&format!("### Snapshot\n```yaml\n{body}\r\n```\n")).is_none());
    }

    #[test]
    fn looks_like_snapshot_needs_a_fence_line_holding_a_ref_item() {
        assert!(looks_like_snapshot("x\n```yaml\n- a [ref=e1]\n```"));
        assert!(looks_like_snapshot(
            "```yaml\r\n  - link \"x\" [ref=e1]\r\n```\r\n"
        ));
        // A markdown file whose example snapshot is a real fence counts too.
        assert!(looks_like_snapshot(
            "# Docs\n\n```yaml\n- button \"Go\" [ref=e4]\n```\n"
        ));
        assert!(!looks_like_snapshot("```yaml\nkey: value\n```"));
        // JSON quoting a snapshot inside a string: no fence line of its own.
        assert!(!looks_like_snapshot(
            r#"{"body": "see ```yaml\n- link [ref=e1]\n``` above"}"#
        ));
        assert!(!looks_like_snapshot(
            "- link [ref=e1]\n```yaml\nkey: v\n```"
        ));
        assert!(!looks_like_snapshot("[ref=e1] and ```yaml on one line"));
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
            "- main:\n  - heading \"T\" [level=1]\n  - link \"x\" [ref=e3]\n  - paragraph: a\n  - paragraph: b"
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

    fn rows(n: usize, distinct: bool) -> String {
        let mut y = String::from("- table [ref=t]:\n  - rowgroup [ref=g]:\n");
        for i in 0..n {
            let v = if distinct {
                format!("v{i}")
            } else {
                "same".into()
            };
            y.push_str(&format!(
                "    - row [ref=r{i}]:\n      - cell [ref=c{i}]: {v}\n"
            ));
        }
        y.pop();
        y
    }

    #[test]
    fn conservative_never_dedupes_rows_with_distinct_data() {
        // Regression from the eval: a 250-country list was cut to 10 and
        // the agent had to expand to find its row.
        let out = trim(&rows(25, true), Mode::Conservative).unwrap();
        let b = body(&out);
        for i in 0..25 {
            assert!(
                b.contains(&format!("cell: v{i}\n")) || b.ends_with(&format!("cell: v{i}")),
                "row {i} hidden"
            );
        }
        assert!(!b.contains("more similar"));
    }

    #[test]
    fn conservative_dedupes_truly_identical_rows() {
        let out = trim(&rows(25, false), Mode::Conservative).unwrap();
        let b = body(&out);
        assert_eq!(b.matches("cell: same").count(), 10);
        assert!(b.contains("- text: \"…and 15 more similar row\""), "{b}");
    }

    #[test]
    fn aggressive_dedupes_structurally_similar_rows() {
        let out = trim(&rows(25, true), Mode::Aggressive).unwrap();
        let b = body(&out);
        assert!(b.contains("cell: v2"));
        assert!(!b.contains("cell: v3\n"));
        assert!(b.contains("- text: \"…and 22 more similar row\""), "{b}");
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
            // A lone CR (CRLF is normalised and accepted).
            "- generic [ref=e1]:\n  - link\r [ref=e2]",
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
