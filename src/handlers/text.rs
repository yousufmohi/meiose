//! Last-resort trimming for any very long text output: keep the head and the
//! tail (cut at line boundaries) and put the expand marker in the middle.
//!
//! The threshold is deliberately high: text outputs are often files the
//! agent wants to edit, and the agent can always call `meiose_expand`.

use super::{Handler, Mode, TrimContext, Trimmed};

pub struct Text;

struct Limits {
    /// Only trim texts longer than this many bytes.
    min_bytes: usize,
    head: usize,
    tail: usize,
}

impl Limits {
    fn for_mode(mode: Mode) -> Limits {
        match mode {
            // ~10K tokens before anything happens; keep ~5K tokens.
            Mode::Conservative => Limits {
                min_bytes: 40_000,
                head: 16_000,
                tail: 4_000,
            },
            Mode::Aggressive => Limits {
                min_bytes: 12_000,
                head: 6_000,
                tail: 2_000,
            },
        }
    }
}

impl Handler for Text {
    fn name(&self) -> &'static str {
        "text"
    }

    fn trim(&self, text: &str, cx: &TrimContext) -> Option<Trimmed> {
        let l = Limits::for_mode(cx.mode);
        if text.len() <= l.min_bytes {
            return None;
        }
        let head_end = cut_back_to_line(text, floor_char(text, l.head));
        let tail_start = cut_forward_to_line(text, floor_char(text, text.len() - l.tail));
        if tail_start <= head_end {
            return None;
        }
        let middle = &text[head_end..tail_start];
        let lines = middle.matches('\n').count();
        let chars = middle.chars().count();
        let out = format!(
            "{}\n[meiose: {lines} lines ({chars} chars) trimmed here — call meiose_expand(\"{}\") for the full output, or add query=\"…\" for only the lines you need]\n{}",
            text[..head_end].trim_end_matches('\n'),
            cx.expand_id,
            &text[tail_start..]
        );
        (out.len() < text.len()).then_some(Trimmed {
            text: out,
            nodes_trimmed: lines,
            texts_shortened: 1,
        })
    }
}

fn floor_char(s: &str, mut i: usize) -> usize {
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

/// Back up to just after the previous newline, unless that loses most of it.
fn cut_back_to_line(s: &str, at: usize) -> usize {
    match s[..at].rfind('\n') {
        Some(nl) if nl + 1 >= at / 2 => nl + 1,
        _ => at,
    }
}

/// Move forward to just after the next newline, if one is near.
fn cut_forward_to_line(s: &str, at: usize) -> usize {
    match s[at..].find('\n') {
        Some(off) if off < (s.len() - at) / 2 => at + off + 1,
        _ => at,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CX: TrimContext = TrimContext {
        expand_id: "t1",
        mode: Mode::Conservative,
    };

    #[test]
    fn keeps_head_and_tail_at_line_boundaries() {
        let text: String = (0..5000).map(|i| format!("line {i}\n")).collect();
        let out = Text.trim(&text, &CX).unwrap().text;
        assert!(out.starts_with("line 0\nline 1\n"));
        assert!(out.ends_with("line 4999\n"));
        let marker = out.lines().find(|l| l.starts_with("[meiose:")).unwrap();
        assert!(marker.contains("meiose_expand(\"t1\")"));
        // Every kept line is a whole original line.
        for l in out.lines().filter(|l| !l.starts_with("[meiose:")) {
            assert!(l.starts_with("line "), "{l:?}");
        }
    }

    #[test]
    fn short_text_is_untouched() {
        assert!(Text.trim(&"x".repeat(39_000), &CX).is_none());
    }

    #[test]
    fn multibyte_text_without_newlines_does_not_panic() {
        let text = "日本語".repeat(20_000);
        let out = Text.trim(&text, &CX).unwrap();
        assert!(out.text.len() < text.len());
    }
}
