//! Token counting for stats and reports.
//!
//! Claude's tokenizer is not public, so exact counts use OpenAI's
//! `o200k_base` as a proxy. Absolute numbers differ from what Claude bills,
//! but ratios (before vs. after trimming) are what we report.
//!
//! The proxy records stats with [`estimate`], which needs no vocabulary:
//! loading o200k costs about 36 MB per meiose process, and every wrapped MCP
//! server runs its own meiose. Set `MEIOSE_EXACT_TOKENS=1` to record exact
//! counts instead (the eval harness does).

use std::sync::OnceLock;

use tiktoken_rs::CoreBPE;

fn bpe() -> &'static CoreBPE {
    static BPE: OnceLock<CoreBPE> = OnceLock::new();
    BPE.get_or_init(|| tiktoken_rs::o200k_base().expect("embedded o200k_base vocabulary loads"))
}

/// Exact o200k token count.
pub fn count(text: &str) -> usize {
    bpe().encode_ordinary(text).len()
}

/// Bytes per token for ordinary text, fitted on the fixtures, JSON and prose.
const BYTES_PER_TOKEN: f64 = 3.75;
/// Leading indentation is cheap: runs of spaces become few tokens.
const INDENT_BYTES_PER_TOKEN: f64 = 8.0;
/// A `[ref=f2e3354]` is 5–8 tokens in about 10–14 bytes.
const TOKENS_PER_REF: f64 = 7.0;

/// Estimated o200k token count, with no vocabulary.
///
/// Refs and indentation are counted apart from the rest, because trimming
/// removes refs (dense in tokens) and keeps indentation (cheap), so a flat
/// bytes-per-token ratio would understate savings. On the 40 fixtures the
/// estimated saving is 31.8% against an exact 31.1%; totals per kind of
/// content are within about ±10%, single pages within about ±40%.
pub fn estimate(text: &str) -> usize {
    let mut indent = 0usize;
    let mut at_line_start = true;
    for b in text.bytes() {
        match b {
            b'\n' => at_line_start = true,
            b' ' if at_line_start => indent += 1,
            _ => at_line_start = false,
        }
    }
    let (mut refs, mut ref_bytes) = (0usize, 0usize);
    let mut rest = text;
    while let Some(i) = rest.find("[ref=") {
        let tail = &rest[i..];
        let len = tail.find(']').map_or(tail.len(), |e| e + 1);
        refs += 1;
        ref_bytes += len;
        rest = &tail[len..];
    }
    let other = text.len().saturating_sub(indent + ref_bytes);
    (other as f64 / BYTES_PER_TOKEN
        + indent as f64 / INDENT_BYTES_PER_TOKEN
        + refs as f64 * TOKENS_PER_REF)
        .round() as usize
}

/// What the store records: an estimate, or exact with `MEIOSE_EXACT_TOKENS=1`.
pub fn for_stats(text: &str) -> usize {
    static EXACT: OnceLock<bool> = OnceLock::new();
    if *EXACT.get_or_init(|| std::env::var("MEIOSE_EXACT_TOKENS").is_ok_and(|v| v == "1")) {
        count(text)
    } else {
        estimate(text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::handlers::{Mode, TrimContext, playwright};

    #[test]
    fn estimate_counts_refs_and_indent_apart() {
        assert_eq!(estimate(""), 0);
        // 15 bytes of text, no refs, no indent.
        assert_eq!(estimate("hello world foo"), 4);
        // Indentation is cheap, refs are dense.
        assert!(estimate("        x") < estimate("abcdefghx"));
        assert_eq!(estimate("[ref=e1]"), 7);
    }

    /// The saving the stats report must track the exact one closely.
    #[test]
    fn estimated_saving_tracks_exact_saving_on_the_fixtures() {
        let dir =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/playwright");
        let cx = TrimContext {
            expand_id: "k3x9q2ab",
            mode: Mode::Conservative,
        };
        let (mut eb, mut ea, mut xb, mut xa) = (0usize, 0usize, 0usize, 0usize);
        for e in std::fs::read_dir(dir).unwrap().flatten() {
            let text = std::fs::read_to_string(e.path()).unwrap();
            let after = playwright::trim_snapshot(&text, &cx).map_or(text.clone(), |t| t.text);
            eb += estimate(&text);
            ea += estimate(&after);
            xb += count(&text);
            xa += count(&after);
        }
        let saving = |b: usize, a: usize| 100.0 * (1.0 - a as f64 / b as f64);
        let (est, exact) = (saving(eb, ea), saving(xb, xa));
        assert!(
            (est - exact).abs() < 2.0,
            "estimated {est:.1}% vs exact {exact:.1}%"
        );
        let total_err = 100.0 * (eb as f64 / xb as f64 - 1.0);
        assert!(total_err.abs() < 10.0, "total off by {total_err:.1}%");
    }
}
