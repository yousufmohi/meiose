//! Measure the JSON path on real MCP outputs saved as text files: tokens
//! before/after (default and aggressive), which handler ran, and whether
//! `meiose_expand(query=...)` recovers the last item of a list (which the
//! default mode drops) with enough context to use it.
//!
//!   cargo run --release --example json_probe -- <dir of .txt outputs>

use meiose::handlers::{self, Mode, TrimContext};
use meiose::{search, tokens};
use serde_json::Value;

/// The list in an output: a top-level array, or `items` (search results).
fn list(v: &Value) -> Option<&Vec<Value>> {
    v.as_array()
        .or_else(|| v.get("items").and_then(Value::as_array))
}

/// Something an agent would search for, and a field proving it got the item.
fn needle(item: &Value) -> Option<(String, String)> {
    let want = item.get("html_url").and_then(Value::as_str)?.to_string();
    let q = ["title", "full_name", "name"]
        .iter()
        .find_map(|k| item.get(k).and_then(Value::as_str).map(str::to_string))
        .or_else(|| {
            item.pointer("/commit/message")
                .and_then(Value::as_str)
                .map(|m| m.lines().next().unwrap_or("").to_string())
        })?;
    Some((q, want))
}

fn main() {
    let dir = std::env::args().nth(1).expect("directory of saved outputs");
    let hs = handlers::all();
    let mut paths: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .collect();
    paths.sort();
    println!(
        "{:<38} {:>8} {:>15} {:>15} {:>7}  last item via query",
        "output", "tokens", "default", "aggressive", "handler"
    );
    for path in paths {
        let text = std::fs::read_to_string(&path).unwrap();
        let before = tokens::count(&text);
        let mut cells = vec![];
        let mut handler = "-";
        for mode in [Mode::Conservative, Mode::Aggressive] {
            let cx = TrimContext {
                expand_id: "k3x9q2ab",
                mode,
            };
            match handlers::trim_text(&hs, &text, &cx) {
                Some((h, t)) => {
                    let after = tokens::count(&t.text);
                    if mode == Mode::Conservative {
                        handler = h;
                    }
                    cells.push(format!(
                        "{after} ({:+.0}%)",
                        -100.0 * (1.0 - after as f64 / before as f64)
                    ));
                }
                None => cells.push("untouched".into()),
            }
        }
        let probe = serde_json::from_str::<Value>(&text)
            .ok()
            .and_then(|v| list(&v).and_then(|l| l.last()).and_then(needle))
            .map(|(q, want)| {
                let r = search::search(&text, &q);
                format!(
                    "{} ({} tok) q={:?}",
                    if r.contains(&want) {
                        "found"
                    } else {
                        "MISSING url"
                    },
                    tokens::count(&r),
                    &q[..q.len().min(40)]
                )
            })
            .unwrap_or_else(|| "n/a (not a list)".into());
        println!(
            "{:<38} {:>8} {:>15} {:>15} {:>7}  {probe}",
            path.file_stem().unwrap().to_string_lossy(),
            before,
            cells[0],
            cells[1],
            handler
        );
    }
}
