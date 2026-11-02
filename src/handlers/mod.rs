//! Output handlers: each one knows how to shrink one kind of tool output.
//!
//! Handlers are pure `&str -> Option<String>` transforms. They never see the
//! JSON-RPC envelope, and [`trim_text`] wraps every call so that a handler
//! that panics, or produces something no smaller than its input, results in
//! the original text being forwarded unchanged (fail open).

pub mod playwright;

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
}

pub fn all() -> Vec<Box<dyn Handler>> {
    vec![Box::new(playwright::Playwright)]
}

/// Try each handler in order; first one to produce a smaller result wins.
/// Panics and non-shrinking results are treated as "no change".
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
}
