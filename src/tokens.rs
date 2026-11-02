//! Token counting for stats and reports.
//!
//! Claude's tokenizer is not public, so we use OpenAI's `o200k_base` as a
//! proxy. Absolute numbers differ from what Claude bills, but ratios
//! (before vs. after trimming) are what we report, and those track closely.

use std::sync::OnceLock;

use tiktoken_rs::CoreBPE;

fn bpe() -> &'static CoreBPE {
    static BPE: OnceLock<CoreBPE> = OnceLock::new();
    BPE.get_or_init(|| tiktoken_rs::o200k_base().expect("embedded o200k_base vocabulary loads"))
}

pub fn count(text: &str) -> usize {
    bpe().encode_ordinary(text).len()
}
