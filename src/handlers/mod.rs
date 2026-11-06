//! Output handlers: each one knows how to shrink one kind of tool output.
//!
//! Handlers are pure `&str -> Option<String>` transforms. They never see the
//! JSON-RPC envelope, and [`trim_text`] wraps every call so that a handler
//! that panics, or produces something no smaller than its input, results in
//! the original text being forwarded unchanged (fail open).
//!
//! Failing open means the *whole* output is forwarded: when a handler
//! recognises an output as its own kind but cannot trim it, the generic
//! handlers after it are skipped ([`Handler::claims`]). Otherwise a snapshot
//! with one odd line would reach the text handler and be cut to a head and a
//! tail, losing most of the page's interactive refs. The handler that gave
//! up is the one that knows why, so it does the logging.

pub mod json;
pub mod playwright;
pub mod text;

use std::panic::{AssertUnwindSafe, catch_unwind};

/// How hard to trim. Conservative is the zero-config default.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Mode {
    #[default]
    Conservative,
    Aggressive,
}

pub struct TrimContext<'a> {
    /// Id the agent passes to `meiose_expand` to get the original back.
    pub expand_id: &'a str,
    pub mode: Mode,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Trimmed {
    pub text: String,
    /// Snapshot lines removed (wrappers, empty nodes, deduped siblings).
    pub nodes_trimmed: usize,
    /// Long texts that were shortened in place.
    pub texts_shortened: usize,
}

pub trait Handler: Send + Sync {
    fn name(&self) -> &'static str;
    /// Return `None` to leave `text` untouched (not ours, or nothing to gain).
    fn trim(&self, text: &str, cx: &TrimContext) -> Option<Trimmed>;
    /// Is `text` this handler's kind of output even when [`Handler::trim`]
    /// declines it (too small, nothing to gain, or a format it could not
    /// parse)? Then no later handler may touch it either: forwarding the
    /// whole output is the safe failure, a generic cut is not.
    fn claims(&self, _text: &str) -> bool {
        false
    }
}

pub fn all() -> Vec<Box<dyn Handler>> {
    vec![
        Box::new(playwright::Playwright),
        Box::new(json::Json),
        Box::new(text::Text),
    ]
}

/// Try each handler in order; first one to produce a smaller result wins.
/// Panics and non-shrinking results are treated as "no change", except that
/// a handler which [claims](Handler::claims) the text ends the chain: the
/// output is forwarded whole rather than cut by a handler that does not
/// understand it.
pub fn trim_text(
    handlers: &[Box<dyn Handler>],
    text: &str,
    cx: &TrimContext,
) -> Option<(&'static str, Trimmed)> {
    for h in handlers {
        match catch_unwind(AssertUnwindSafe(|| h.trim(text, cx))) {
            Ok(Some(t)) if t.text.len() < text.len() => return Some((h.name(), t)),
            Ok(_) => {}
            Err(_) => {
                tracing::error!(
                    handler = h.name(),
                    "handler panicked; forwarding the original output unchanged"
                );
            }
        }
        if h.claims(text) {
            return None;
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Panics;
    impl Handler for Panics {
        fn name(&self) -> &'static str {
            "panics"
        }
        fn trim(&self, _: &str, _: &TrimContext) -> Option<Trimmed> {
            panic!("boom")
        }
    }

    struct Grows;
    impl Handler for Grows {
        fn name(&self) -> &'static str {
            "grows"
        }
        fn trim(&self, text: &str, _: &TrimContext) -> Option<Trimmed> {
            Some(Trimmed {
                text: format!("{text}!"),
                nodes_trimmed: 1,
                texts_shortened: 0,
            })
        }
    }

    const CX: TrimContext = TrimContext {
        expand_id: "t",
        mode: Mode::Conservative,
    };

    #[test]
    fn panicking_handler_fails_open() {
        let hs: Vec<Box<dyn Handler>> = vec![Box::new(Panics)];
        assert!(trim_text(&hs, "anything", &CX).is_none());
    }

    #[test]
    fn growing_result_is_rejected() {
        let hs: Vec<Box<dyn Handler>> = vec![Box::new(Grows)];
        assert!(trim_text(&hs, "anything", &CX).is_none());
    }

    /// A snapshot big enough for the text handler (> 40 KB) with something
    /// for the Playwright handler to trim (implied cursors).
    fn big_snapshot() -> String {
        let mut y = String::from("- main [ref=e0]:\n");
        for i in 1..1500 {
            y.push_str(&format!(
                "  - link \"item {i}\" [ref=e{i}] [cursor=pointer]\n"
            ));
        }
        y.pop();
        format!("### Page\n- Page URL: https://x.test/\n### Snapshot\n```yaml\n{y}\n```\n")
    }

    #[test]
    fn a_snapshot_the_playwright_handler_rejects_never_reaches_the_text_handler() {
        let good = big_snapshot();
        assert!(good.len() > 40_000);
        let (h, _) = trim_text(&all(), &good, &CX).expect("trimmed");
        assert_eq!(h, "playwright");

        // One odd-indented line: the Playwright handler declines the whole
        // snapshot, and nothing else may cut it.
        let odd = good.replacen("\n  - link \"item 700\"", "\n   - link \"item 700\"", 1);
        assert_ne!(odd, good);
        assert!(trim_text(&all(), &odd, &CX).is_none());

        // Even if the Playwright handler panicked on it.
        struct PanicsOnSnapshots;
        impl Handler for PanicsOnSnapshots {
            fn name(&self) -> &'static str {
                "boom"
            }
            fn trim(&self, _: &str, _: &TrimContext) -> Option<Trimmed> {
                panic!("boom")
            }
            fn claims(&self, text: &str) -> bool {
                playwright::looks_like_snapshot(text)
            }
        }
        let hs: Vec<Box<dyn Handler>> = vec![Box::new(PanicsOnSnapshots), Box::new(text::Text)];
        assert!(trim_text(&hs, &good, &CX).is_none());
    }

    #[test]
    fn plain_text_that_merely_mentions_yaml_is_still_trimmed() {
        let text = "see the ```yaml block below\n".repeat(2_000);
        assert!(text.len() > 40_000);
        let (h, _) = trim_text(&all(), &text, &CX).expect("trimmed");
        assert_eq!(h, "text");
    }
}
