//! The handler chain as a whole (`handlers::trim_text`), not one handler at a
//! time. A Playwright snapshot the Playwright handler cannot parse must never
//! fall through to the text handler: that would cut the page to a head and a
//! tail and silently lose most of its interactive refs. Format variants the
//! handler does understand (older headers, CRLF) must still be trimmed by it.

mod common;

use meiose::handlers::{self, Mode, TrimContext};

const MODES: [Mode; 2] = [Mode::Conservative, Mode::Aggressive];

fn trim(text: &str, mode: Mode) -> Option<(&'static str, String)> {
    let cx = TrimContext {
        expand_id: "chain",
        mode,
    };
    handlers::trim_text(&handlers::all(), text, &cx).map(|(h, t)| (h, t.text))
}

/// Fixtures the text handler would want (over its 40 KB threshold).
fn big_fixtures() -> Vec<(String, String)> {
    let v: Vec<(String, String)> = common::playwright_fixtures(false)
        .into_iter()
        .map(|p| {
            let name = p.file_stem().unwrap().to_string_lossy().into_owned();
            (name, std::fs::read_to_string(&p).unwrap())
        })
        .filter(|(_, t)| t.len() > 40_000)
        .collect();
    assert!(v.len() >= 10, "expected the committed fixture set");
    v
}

/// A line deep inside the yaml block that carries a ref.
fn inner_line(text: &str) -> &str {
    let block = meiose::handlers::playwright::find_block(text).unwrap();
    text[block]
        .lines()
        .skip(20)
        .find(|l| l.contains("[ref="))
        .unwrap()
}

/// `text` with the first occurrence of `line` (as a whole line) rewritten.
fn with_line(text: &str, line: &str, replacement: &str) -> String {
    let out = text.replacen(&format!("\n{line}\n"), &format!("\n{replacement}\n"), 1);
    assert_ne!(out, text, "line not found: {line:?}");
    out
}

#[test]
fn format_variants_are_still_trimmed_by_the_playwright_handler() {
    for (name, text) in big_fixtures() {
        let variants = [
            ("crlf", text.replace('\n', "\r\n")),
            (
                "0.0.29 header",
                text.replacen("### Snapshot\n", "- Page Snapshot\n", 1),
            ),
            (
                "0.0.3x header",
                text.replacen("### Snapshot\n", "- Page Snapshot:\n", 1),
            ),
        ];
        for (label, variant) in &variants {
            assert_ne!(variant, &text, "{name}: {label} variant is the original");
            for mode in MODES {
                let (h, out) = trim(variant, mode)
                    .unwrap_or_else(|| panic!("{name} ({label}, {mode:?}): not trimmed"));
                assert_eq!(h, "playwright", "{name} ({label}, {mode:?})");
                assert!(out.len() < variant.len());
                common::assert_refs_survive(&variant.replace("\r\n", "\n"), &out);
                assert!(!out.contains('\r'), "{name} ({label}): CR in the output");
            }
        }
    }
}

#[test]
fn a_snapshot_the_playwright_handler_rejects_is_forwarded_whole() {
    for (name, text) in big_fixtures() {
        let line = inner_line(&text);
        let variants = [
            // An odd indent, and a level skipped: the parser refuses both.
            ("odd indent", with_line(&text, line, &format!(" {line}"))),
            (
                "skipped level",
                with_line(&text, line, &format!("{}{line}", " ".repeat(20))),
            ),
            // A stray CR inside a line (CRLF endings are fine, this is not).
            (
                "lone CR",
                with_line(&text, line, &line.replacen("[ref=", "\r[ref=", 1)),
            ),
        ];
        for (label, variant) in &variants {
            for mode in MODES {
                assert!(
                    trim(variant, mode).is_none(),
                    "{name} ({label}, {mode:?}): a snapshot the Playwright handler declined was cut by another handler"
                );
            }
        }
    }
}

#[test]
fn an_unrecognised_header_is_forwarded_whole_too() {
    // The next upstream rename must fail open, not fall through.
    for (name, text) in big_fixtures() {
        let renamed = text.replacen("### Snapshot\n", "### Accessibility tree\n", 1);
        assert_ne!(renamed, text);
        for mode in MODES {
            assert!(trim(&renamed, mode).is_none(), "{name} ({mode:?})");
        }
    }
}
