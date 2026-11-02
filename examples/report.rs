//! Savings report over the snapshot fixtures.
//!
//!   cargo run --release --example report [-- --dump DIR] [-- --markdown] [-- --local]
//!
//! Prints tokens before/after (o200k proxy) and trim time per fixture for
//! both modes. `--dump DIR` writes each trimmed output for eyeballing.
//! Only the committed fixtures are counted unless `--local` is given, which
//! adds your private ones from tests/fixtures/local/ (published numbers must
//! not include those).

use std::path::{Path, PathBuf};
use std::time::Instant;

use meiose::handlers::{Mode, TrimContext, playwright};
use meiose::tokens;

fn fixtures(include_local: bool) -> Vec<PathBuf> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let dirs: &[&str] = if include_local {
        &["playwright", "local/playwright"]
    } else {
        &["playwright"]
    };
    let mut v: Vec<PathBuf> = dirs
        .iter()
        .filter_map(|d| std::fs::read_dir(root.join(d)).ok())
        .flatten()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|e| e == "txt"))
        .collect();
    v.sort();
    v
}

fn time_ms(f: impl Fn()) -> f64 {
    let runs = 15;
    let mut t: Vec<f64> = (0..runs)
        .map(|_| {
            let s = Instant::now();
            f();
            s.elapsed().as_secs_f64() * 1000.0
        })
        .collect();
    t.sort_by(|a, b| a.partial_cmp(b).unwrap());
    t[runs / 2]
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let dump = args
        .iter()
        .position(|a| a == "--dump")
        .map(|i| PathBuf::from(&args[i + 1]));
    let markdown = args.iter().any(|a| a == "--markdown");
    let pages = fixtures(args.iter().any(|a| a == "--local"));
    if let Some(d) = &dump {
        std::fs::create_dir_all(d).unwrap();
    }

    let mut tot = [0usize; 3];
    if markdown {
        println!("| fixture | tokens | conservative | saved | aggressive | saved | ms (cons.) |");
        println!("|---|---:|---:|---:|---:|---:|---:|");
    } else {
        println!(
            "{:<56} {:>8} {:>8} {:>6} {:>8} {:>6} {:>7}",
            "fixture", "tokens", "cons", "saved", "aggr", "saved", "ms"
        );
    }
    for path in &pages {
        let text = std::fs::read_to_string(path).unwrap();
        let name = path.file_stem().unwrap().to_string_lossy();
        let before = tokens::count(&text);
        let mut row = vec![];
        for mode in [Mode::Conservative, Mode::Aggressive] {
            let cx = TrimContext {
                expand_id: "x",
                mode,
            };
            let out = playwright::trim_snapshot(&text, &cx)
                .map(|t| t.text)
                .unwrap_or_else(|| text.clone());
            if let (Some(d), Mode::Conservative) = (&dump, mode) {
                std::fs::write(d.join(format!("{name}.txt")), &out).unwrap();
            }
            row.push(tokens::count(&out));
        }
        let cx = TrimContext {
            expand_id: "x",
            mode: Mode::Conservative,
        };
        let ms = time_ms(|| {
            std::hint::black_box(playwright::trim_snapshot(&text, &cx));
        });
        tot[0] += before;
        tot[1] += row[0];
        tot[2] += row[1];
        let pct = |a: usize| 100.0 * (1.0 - a as f64 / before as f64);
        if markdown {
            println!(
                "| {name} | {before} | {} | {:.0}% | {} | {:.0}% | {ms:.2} |",
                row[0],
                pct(row[0]),
                row[1],
                pct(row[1])
            );
        } else {
            println!(
                "{:<56} {before:>8} {:>8} {:>5.0}% {:>8} {:>5.0}% {ms:>7.2}",
                &name[..name.len().min(56)],
                row[0],
                pct(row[0]),
                row[1],
                pct(row[1])
            );
        }
    }
    let pct = |a: usize| 100.0 * (1.0 - a as f64 / tot[0] as f64);
    println!(
        "TOTAL ({} pages) {} tokens → conservative {} ({:.0}% saved), aggressive {} ({:.0}% saved)",
        pages.len(),
        tot[0],
        tot[1],
        pct(tot[1]),
        tot[2],
        pct(tot[2])
    );
}
