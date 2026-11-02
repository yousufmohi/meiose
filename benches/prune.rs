//! Trim time for the largest fixture (the Rust `Vec` docs page, ~924K chars,
//! ~244K tokens). Requirement: < 5 ms on a laptop.
//!
//!   cargo bench --bench prune

use std::hint::black_box;

use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use meiose::handlers::{Mode, TrimContext, playwright};

fn largest_fixture() -> (String, String) {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/playwright");
    let mut best: Option<(u64, std::path::PathBuf)> = None;
    for e in std::fs::read_dir(dir).expect("fixtures dir").flatten() {
        let len = e.metadata().map(|m| m.len()).unwrap_or(0);
        if best.as_ref().is_none_or(|(l, _)| len > *l) {
            best = Some((len, e.path()));
        }
    }
    let path = best.expect("at least one fixture").1;
    let name = path.file_stem().unwrap().to_string_lossy().into_owned();
    (name, std::fs::read_to_string(path).unwrap())
}

fn bench(c: &mut Criterion) {
    let (name, text) = largest_fixture();
    let mut g = c.benchmark_group(format!("playwright/{name}"));
    g.throughput(Throughput::Bytes(text.len() as u64));
    for (label, mode) in [
        ("conservative", Mode::Conservative),
        ("aggressive", Mode::Aggressive),
    ] {
        let cx = TrimContext {
            expand_id: "bench",
            mode,
        };
        g.bench_function(label, |b| {
            b.iter(|| playwright::trim_snapshot(black_box(&text), &cx))
        });
    }
    g.finish();

    // No real page reaches the 400K-token requirement, so double the largest
    // one's tree (~490K tokens) to bench past it.
    let s = text.find("```yaml\n").unwrap() + 8;
    let e = text[s..].find("\n```").unwrap() + s;
    let doubled = format!(
        "{}{}\n{}{}",
        &text[..s],
        &text[s..e],
        &text[s..e],
        &text[e..]
    );
    let mut g = c.benchmark_group("playwright/synthetic-2x-largest");
    g.throughput(Throughput::Bytes(doubled.len() as u64));
    let cx = TrimContext {
        expand_id: "bench",
        mode: Mode::Conservative,
    };
    g.bench_function("conservative", |b| {
        b.iter(|| playwright::trim_snapshot(black_box(&doubled), &cx))
    });
    g.finish();
}

criterion_group!(benches, bench);
criterion_main!(benches);
