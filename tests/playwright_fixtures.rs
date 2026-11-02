//! Every Playwright fixture through the handler, in both modes:
//! - snapshot test (insta) of the conservative output, the shipped default;
//! - invariants (refs survive, smaller, marker, framing untouched, well-formed).
//!
//! Review snapshot changes with `cargo insta review`.

mod common;

use meiose::handlers::{Mode, TrimContext, playwright};

#[test]
fn conservative_output_matches_approved_snapshots() {
    let fixtures = common::playwright_fixtures(false);
    assert!(fixtures.len() >= 30, "expected the committed fixture set");
    let cx = TrimContext {
        expand_id: "EXPAND_ID",
        mode: Mode::Conservative,
    };
    for path in fixtures {
        let input = std::fs::read_to_string(&path).unwrap();
        let name = path.file_stem().unwrap().to_string_lossy().into_owned();
        let output = match playwright::trim_snapshot(&input, &cx) {
            Some(t) => t.text,
            None => format!("<untouched>\n{input}"),
        };
        insta::with_settings!({
            snapshot_path => "snapshots/playwright",
            prepend_module_to_snapshot => false,
            omit_expression => true,
        }, {
            insta::assert_snapshot!(name, output);
        });
    }
}

#[test]
fn invariants_hold_on_every_fixture_in_both_modes() {
    let mut trimmed = 0;
    for path in common::playwright_fixtures(true) {
        let input = std::fs::read_to_string(&path).unwrap();
        for mode in [Mode::Conservative, Mode::Aggressive] {
            let cx = TrimContext {
                expand_id: "inv",
                mode,
            };
            if let Some(t) = playwright::trim_snapshot(&input, &cx) {
                trimmed += 1;
                std::panic::catch_unwind(|| common::check_invariants(&input, &t.text, "inv"))
                    .unwrap_or_else(|_| panic!("invariant broken: {} ({mode:?})", path.display()));
            } else {
                // Only small outputs may be left alone.
                assert!(
                    input.len() < 4096,
                    "{} ({mode:?}) is large but was not trimmed",
                    path.display()
                );
            }
        }
    }
    assert!(trimmed > 0);
}

#[test]
fn trimming_is_deterministic() {
    let cx = TrimContext {
        expand_id: "d",
        mode: Mode::Conservative,
    };
    for path in common::playwright_fixtures(false).into_iter().take(10) {
        let input = std::fs::read_to_string(&path).unwrap();
        let a = playwright::trim_snapshot(&input, &cx).map(|t| t.text);
        let b = playwright::trim_snapshot(&input, &cx).map(|t| t.text);
        assert_eq!(a, b);
    }
}
