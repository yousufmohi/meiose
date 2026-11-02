//! Does `meiose_expand(query=...)` return what an agent needs to answer?
//! Runs `search::search` on fixtures for known facts and prints whether the
//! answer text appears in the result, and its size in tokens.
//!
//!   cargo run --release --example search_probe

use meiose::{search, tokens};

fn fixture(name: &str) -> String {
    std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/playwright")
            .join(format!("{name}.txt")),
    )
    .unwrap()
}

fn main() {
    let vec = "doc-rust-lang-org-std-vec-struct-vec-html";
    let cases = [
        // (page, query, text the answer needs)
        (vec, "extract_if", "1.87.0"),
        (vec, "dedup_by_key", "1.16.0"),
        (vec, "retain_mut", "1.61.0"),
        (vec, "split_off", "1.4.0"),
        (
            "en-wikipedia-org-wiki-list-of-countries-by-population-united-nations",
            "Mongolia",
            "3,431,932",
        ),
        (
            "en-wikipedia-org-wiki-rust-programming-language",
            "Graydon",
            "Graydon Hoare",
        ),
    ];
    let mut found = 0;
    for (page, q, want) in cases {
        let r = search::search(&fixture(page), q);
        let ok = r.contains(want);
        found += usize::from(ok);
        println!(
            "{:<6} {q:<14} wants {want:<14} {:>6} tokens",
            if ok { "found" } else { "MISS" },
            tokens::count(&r)
        );
    }
    println!("{found}/{} answers present", cases.len());
}
