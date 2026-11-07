//! Measures the trimming paths through the real Interceptor (no network):
//! totals over the 40 committed fixtures for the default mode and for
//! `--budget`, how many pages stay over Claude Code's limits (25,000 tokens,
//! and 50,000 characters for text results), and the tokens needed to recover
//! one fact with a full expand vs `meiose_expand(id, query=...)`.
//!
//!   cargo run --release --example measure
use meiose::handlers::Mode;
use meiose::intercept::{ClientAction, Config, Interceptor};
use meiose::store::StoreHandle;
use meiose::tokens;
use serde_json::{Value, json};
use std::path::Path;

fn page(name: &str) -> String {
    std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/playwright")
            .join(format!("{name}.txt")),
    )
    .unwrap()
}
struct Px {
    ic: Interceptor,
    n: u64,
}
impl Px {
    fn new(budget: usize, db: &Path, tag: &str) -> Px {
        let store = StoreHandle::open(&db.join(format!("{tag}.db"))).unwrap();
        let ic = Interceptor::new(
            Config {
                mode: Mode::Conservative,
                trim: true,
                diff: true,
                budget_bytes: budget * 3,
                auto_budget: false,
                server: "m".into(),
                server_key: "m".into(),
            },
            Some(store),
        );
        let p = Px { ic, n: 0 };
        let f = br#"{"jsonrpc":"2.0","id":0,"method":"tools/list"}"#;
        p.ic.on_client_frame(&f[..]);
        p.ic.on_server_frame(br#"{"jsonrpc":"2.0","id":0,"result":{"tools":[]}}"#.to_vec());
        p
    }
    fn call(&mut self, text: &str) -> String {
        self.n += 1;
        let id = self.n;
        let req = json!({"jsonrpc":"2.0","id":id,"method":"tools/call","params":{"name":"browser_snapshot"}}).to_string();
        self.ic.on_client_frame(req.as_bytes());
        let resp =
            json!({"jsonrpc":"2.0","id":id,"result":{"content":[{"type":"text","text":text}]}})
                .to_string();
        let out = self.ic.on_server_frame(resp.into_bytes());
        let v: Value = serde_json::from_slice(&out).unwrap();
        v["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .to_string()
    }
    fn expand(&mut self, eid: &str, q: Option<&str>) -> String {
        self.n += 1;
        let mut args = json!({"id":eid});
        if let Some(q) = q {
            args["query"] = json!(q);
        }
        let req = json!({"jsonrpc":"2.0","id":self.n,"method":"tools/call","params":{"name":"meiose_expand","arguments":args}}).to_string();
        let ClientAction::Expand {
            id_raw,
            expand_id,
            query,
            terminator,
        } = self.ic.on_client_frame(req.as_bytes())
        else {
            panic!()
        };
        let out = self
            .ic
            .expand_reply(&id_raw, expand_id.as_deref(), query.as_deref(), terminator);
        let v: Value = serde_json::from_slice(&out).unwrap();
        v["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .to_string()
    }
}
fn eid(t: &str) -> String {
    let s = t.find("meiose_expand(\"").unwrap() + 15;
    let e = t[s..].find('"').unwrap();
    t[s..s + e].to_string()
}

fn main() {
    let dir = tempfile::tempdir().unwrap();
    let mut names: Vec<String> =
        std::fs::read_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/playwright"))
            .unwrap()
            .map(|e| {
                e.unwrap()
                    .path()
                    .file_stem()
                    .unwrap()
                    .to_string_lossy()
                    .to_string()
            })
            .collect();
    names.sort();
    let before: usize = names.iter().map(|n| tokens::count(&page(n))).sum();
    for (b, label) in [
        (0, "default (no budget)"),
        (20000, "--budget 20000"),
        (12000, "--budget 12000"),
    ] {
        let (mut tot, mut over, mut over_chars, mut maxt) = (0, 0, 0, 0);
        for (i, n) in names.iter().enumerate() {
            let mut p = Px::new(b, dir.path(), &format!("a{b}-{i}"));
            let out = p.call(&page(n));
            let t = tokens::count(&out);
            over_chars += usize::from(out.chars().count() > 50_000);
            tot += t;
            over += usize::from(t > 25000);
            maxt = maxt.max(t);
        }
        println!(
            "{label:<26} {before} -> {tot}  ({:.1}% saved) pages>25K tokens: {over}  >50K chars: {over_chars}  max {maxt}",
            100.0 * (before - tot) as f64 / before as f64
        );
    }
    println!("-- expand: tokens to recover a fact from a page");
    for (n, q, want) in [
        (
            "en-wikipedia-org-wiki-list-of-countries-by-population-united-nations",
            "Canada",
            "Canada",
        ),
        (
            "doc-rust-lang-org-std-vec-struct-vec-html",
            "dedup_by_key",
            "dedup_by_key",
        ),
        (
            "en-wikipedia-org-wiki-rust-programming-language",
            "Graydon",
            "Graydon",
        ),
        (
            "developer-mozilla-org-en-us-docs-web-html-element-input",
            "webkitdirectory",
            "webkitdirectory",
        ),
    ] {
        let mut p = Px::new(0, dir.path(), &format!("e-{}", &n[..12]));
        let t = p.call(&page(n));
        let id = eid(&t);
        let full = tokens::count(&p.expand(&id, None));
        let r = p.expand(&id, Some(q));
        println!(
            "{:<34} q={q:<16} full {full:>7}  query {:>6}  found:{}",
            &n[..n.len().min(34)],
            tokens::count(&r),
            r.contains(want)
        );
    }
}
