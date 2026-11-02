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

    // The requirement itself: a 400K-token snapshot. Cut the doubled tree at
    // a line boundary where the o200k count reaches 400K.
    let body: Vec<&str> = doubled[s..doubled.len() - (text.len() - e)]
        .split('\n')
        .collect();
    let (mut lo, mut hi) = (1, body.len());
    let page = |n: usize| format!("{}{}{}", &text[..s], body[..n].join("\n"), &text[e..]);
    while lo < hi {
        let mid = (lo + hi) / 2;
        if meiose::tokens::count(&page(mid)) < 400_000 {
            lo = mid + 1;
        } else {
            hi = mid;
        }
    }
    let p400 = page(lo);
    let mut g = c.benchmark_group(format!(
        "playwright/synthetic-{}k-tokens",
        meiose::tokens::count(&p400) / 1000
    ));
    g.throughput(Throughput::Bytes(p400.len() as u64));
    g.bench_function("conservative", |b| {
        b.iter(|| playwright::trim_snapshot(black_box(&p400), &cx))
    });
    g.finish();
}

criterion_group!(benches, bench);
criterion_main!(benches);
