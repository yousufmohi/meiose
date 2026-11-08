//! M3 end-to-end: the real `meiose` binary in front of the scripted MCP
//! server in `tests/fake_server.py` (`mcp` mode). Covers tools/list
//! injection, trimming, meiose_expand byte-for-byte, diffs, passthrough,
//! id collisions, errors, and the `stats` / `log` commands.

mod common;

use std::process::Command;

use common::session::{Session, expand_id, result_raw_of, root, text_of};
use serde_json::{Value, json};

const HN: &str = "tests/fixtures/playwright/news-ycombinator-com.txt";
const FORM: &str = "tests/fixtures/proxy/form-page.txt";

#[test]
fn expand_tool_is_added_to_the_first_tools_page_only() {
    let mut s = Session::start(HN, &[]);
    let names = s.init();
    assert_eq!(names, ["browser_snapshot", "get_rows", "meiose_expand"]);
    let page2 = s.request("tools/list", json!({"cursor": "page2"}));
    let names: Vec<&str> = page2["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["small", "fails"]);
    s.finish();
}

#[test]
fn snapshot_is_trimmed_and_expand_returns_the_original_byte_for_byte() {
    let mut s = Session::start(HN, &[]);
    s.init();
    let original = std::fs::read_to_string(root().join(HN)).unwrap();

    let id = s.next_id + 1;
    let resp = s.call("browser_snapshot", json!({}));
    let trimmed = text_of(&resp);
    assert!(trimmed.len() < original.len());
    common::check_invariants(&original, trimmed, &expand_id(trimmed));

    let (line, _) = s.request_raw(
        "tools/call",
        json!({"name":"meiose_expand","arguments":{"id": expand_id(trimmed)}}),
    );
    assert_eq!(
        result_raw_of(&line),
        s.server_result_raw(id),
        "meiose_expand must return the server's original result byte-for-byte"
    );
    let expanded: Value = serde_json::from_str(&line).unwrap();
    assert_eq!(text_of(&expanded), original);
    // The expand call never reached the server.
    let log = std::fs::read_to_string(&s.server_log).unwrap();
    assert!(!log.contains("unexpected tool"));
    s.finish();
}

#[test]
fn unknown_expand_id_is_a_helpful_tool_error() {
    let mut s = Session::start(HN, &[]);
    s.init();
    let resp = s.call("meiose_expand", json!({"id": "nope1234"}));
    assert_eq!(resp["result"]["isError"], true);
    assert!(text_of(&resp).contains("no stored output for id \"nope1234\""));
    assert!(text_of(&resp).contains("Re-run the original tool call"));
    let resp = s.call("meiose_expand", json!({}));
    assert_eq!(resp["result"]["isError"], true);
    assert!(text_of(&resp).contains("needs an \"id\" argument"));
    s.finish();
}

#[test]
fn large_json_is_trimmed_and_expandable() {
    let mut s = Session::start(HN, &[]);
    s.init();
    let resp = s.call("get_rows", json!({}));
    let text = text_of(&resp);
    assert!(
        text.contains("…990 more items with keys: id, name, email, bio"),
        "{}",
        &text[text.len().saturating_sub(300)..]
    );
    let full = s.call("meiose_expand", json!({"id": expand_id(text)}));
    let rows: Vec<Value> = serde_json::from_str(text_of(&full)).unwrap();
    assert_eq!(rows.len(), 1000);
    s.finish();
}

#[test]
fn repeat_snapshot_of_the_same_page_is_sent_as_a_diff() {
    let mut s = Session::start(FORM, &[]);
    s.init();
    let first = s.call("browser_snapshot", json!({}));
    assert!(text_of(&first).contains("```yaml"));

    let second = s.call("browser_snapshot", json!({"checked": true}));
    let d = text_of(&second);
    assert!(d.contains("```diff"), "{d}");
    assert!(d.contains("+      - checkbox [checked] [ref=e5]"), "{d}");
    assert!(d.contains("-      - checkbox [ref=e5]"), "{d}");
    assert!(
        d.len() < text_of(&first).len() / 5,
        "a one-line change must be small"
    );

    let third = s.call("browser_snapshot", json!({"checked": true}));
    assert!(text_of(&third).contains("Unchanged since your previous snapshot"));

    // The full page is one expand away, byte-for-byte.
    let full = s.call("meiose_expand", json!({"id": expand_id(d)}));
    assert!(text_of(&full).contains("- checkbox [checked] [ref=e5]"));
    assert!(text_of(&full).contains("```yaml"));
    s.finish();
}

/// End to end: a page the Playwright handler cannot parse (one line with an
/// odd indent) reaches the agent exactly as the server sent it. Before, the
/// text handler cut it to a head and a tail, keeping about one ref in twenty.
#[test]
fn an_unparseable_snapshot_is_forwarded_byte_for_byte() {
    const LARGE: &str = "tests/fixtures/playwright/the-internet-herokuapp-com-large.txt";
    let text = std::fs::read_to_string(root().join(LARGE)).unwrap();
    assert!(
        text.len() > 40_000,
        "must be big enough for the text handler"
    );
    let line = text.lines().skip(40).find(|l| l.contains("[ref=")).unwrap();
    let mut s = Session::start(LARGE, &[]);
    s.init();
    let id = s.next_id + 1;
    let (resp, _) = s.request_raw(
        "tools/call",
        json!({"name": "browser_snapshot", "arguments": {"replace": [format!("\n{line}"), format!("\n {line}")]}}),
    );
    assert_eq!(
        result_raw_of(&resp),
        s.server_result_raw(id),
        "a snapshot the Playwright handler declines must be forwarded whole"
    );
    // The next, identical call is the first repeat of a page the agent saw
    // in full, so it may be a diff; it must still not be a head-and-tail cut.
    let again = s.call(
        "browser_snapshot",
        json!({"replace": [format!("\n{line}"), format!("\n {line}")]}),
    );
    assert!(!text_of(&again).contains("lines ("), "{}", text_of(&again));
    s.finish();
}

/// `--budget` treats a CRLF page exactly like the LF one: the guard and the
/// outline run on the normalised text.
#[test]
fn budget_applies_to_crlf_pages_too() {
    const WIKI: &str =
        "tests/fixtures/playwright/en-wikipedia-org-wiki-rust-programming-language.txt";
    let text = std::fs::read_to_string(root().join(WIKI)).unwrap();
    let mut s = Session::start(WIKI, &["--budget", "20000", "--no-diff"]);
    s.init();
    let lf = s.call("echo", json!({"text": text}));
    let crlf = s.call("echo", json!({"text": text.replace('\n', "\r\n")}));
    let (lf, crlf) = (text_of(&lf), text_of(&crlf));
    assert!(lf.contains("outline only"), "{}", &lf[lf.len() - 300..]);
    assert!(
        crlf.contains("outline only"),
        "{}",
        &crlf[crlf.len() - 300..]
    );
    assert!(!crlf.contains('\r'));
    assert_eq!(
        lf.replace(&expand_id(lf), "ID"),
        crlf.replace(&expand_id(crlf), "ID"),
        "the CRLF page must get the same outline"
    );
    s.finish();
}

#[test]
fn expand_with_a_query_returns_only_matching_lines() {
    let mut s = Session::start(FORM, &[]);
    s.init();
    let first = s.call("browser_snapshot", json!({}));
    let id = expand_id(text_of(&first)).to_string();
    let hit = s.call("meiose_expand", json!({"id": id, "query": "checkbox"}));
    let t = text_of(&hit);
    assert!(
        t.contains("match \"checkbox\"") && t.contains("[ref=e5]"),
        "{t}"
    );
    let full = s.call("meiose_expand", json!({"id": id}));
    assert!(t.len() < text_of(&full).len() / 2);
    let miss = s.call("meiose_expand", json!({"id": id, "query": "no such text"}));
    assert!(text_of(&miss).starts_with("No lines match"));
    s.finish();
}

/// "Unchanged" is claimed only when the page itself is unchanged. Here a
/// collapsed wrapper's ref changes: the trimmed view is identical, the page
/// is not.
#[test]
fn an_identical_view_of_a_changed_page_is_not_reported_as_unchanged() {
    let mut s = Session::start(FORM, &[]);
    s.init();
    let first = s.call("browser_snapshot", json!({}));
    assert!(
        !text_of(&first).contains("[ref=e100]"),
        "e100 is a collapsed wrapper"
    );
    let second = s.call(
        "browser_snapshot",
        json!({"replace": ["[ref=e100]", "[ref=e900]"]}),
    );
    let d = text_of(&second);
    assert!(!d.contains("Unchanged since"), "{d}");
    assert!(d.contains("parts the trimmed view does not show"), "{d}");
    assert!(d.contains("meiose_expand(\""), "{d}");
    // Really unchanged: say so.
    let third = s.call(
        "browser_snapshot",
        json!({"replace": ["[ref=e100]", "[ref=e900]"]}),
    );
    assert!(text_of(&third).contains("Unchanged since your previous snapshot"));
    s.finish();
}

/// Claude Code saves a text result over 50,000 characters to a file and
/// shows its model a note. A diff against such a view is relative to nothing
/// the model has, so the next snapshot is sent in full again.
#[test]
fn no_diff_for_claude_code_against_a_view_it_saved_to_a_file() {
    const LARGE: &str = "tests/fixtures/playwright/the-internet-herokuapp-com-large.txt";
    for (client, expect_diff) in [("Claude Code", false), ("t", true)] {
        let mut s = Session::start(LARGE, &[]);
        s.init_as(client);
        let first = s.call("browser_snapshot", json!({}));
        assert!(
            text_of(&first).len() > 50_000,
            "{client}: not over the limit"
        );
        let second = s.call("browser_snapshot", json!({}));
        let d = text_of(&second);
        assert_eq!(
            d.contains("Unchanged since your previous snapshot"),
            expect_diff,
            "{client}:\n{}",
            &d[..300.min(d.len())]
        );
        assert_eq!(d.contains("```yaml"), !expect_diff, "{client}");
        s.finish();
    }
    // Pages Claude Code does show are still diffed.
    let mut s = Session::start(FORM, &[]);
    s.init_as("Claude Code");
    let first = s.call("browser_snapshot", json!({}));
    assert!(text_of(&first).len() < 50_000);
    let second = s.call("browser_snapshot", json!({"checked": true}));
    let d = text_of(&second);
    assert!(d.contains("```diff"), "{d}");
    assert!(d.contains("no longer have the previous one"), "{d}");
    s.finish();
}

#[test]
fn no_diff_flag_always_sends_full_snapshots() {
    let mut s = Session::start(FORM, &["--no-diff"]);
    s.init();
    s.call("browser_snapshot", json!({}));
    let second = s.call("browser_snapshot", json!({"checked": true}));
    assert!(text_of(&second).contains("```yaml"));
    assert!(!text_of(&second).contains("```diff"));
    s.finish();
}

#[test]
fn passthrough_changes_nothing() {
    let mut s = Session::start(HN, &["--passthrough"]);
    s.request("initialize", json!({}));
    let id = s.next_id + 1;
    let (line, _) = s.request_raw("tools/list", json!({}));
    assert_eq!(result_raw_of(&line), s.server_result_raw(id));
    let id = s.next_id + 1;
    let (line, _) = s.request_raw(
        "tools/call",
        json!({"name":"browser_snapshot","arguments":{}}),
    );
    assert_eq!(result_raw_of(&line), s.server_result_raw(id));
    s.finish();
}

#[test]
fn nothing_is_trimmed_before_expand_was_advertised() {
    let mut s = Session::start(HN, &[]);
    s.request("initialize", json!({}));
    // No tools/list yet: the agent does not know meiose_expand exists.
    let id = s.next_id + 1;
    let (line, _) = s.request_raw(
        "tools/call",
        json!({"name":"browser_snapshot","arguments":{}}),
    );
    assert_eq!(result_raw_of(&line), s.server_result_raw(id));
    s.finish();
}

#[test]
fn server_request_reusing_a_client_id_is_not_confused_with_the_response() {
    let mut s = Session::start(HN, &[]);
    s.init();
    let (line, others) = s.request_raw("tools/call", json!({"name":"collide","arguments":{}}));
    // The server's own request came through untouched...
    assert_eq!(others.len(), 1);
    assert_eq!(others[0]["method"], "roots/list");
    // ...and the real response was still recognised and trimmed.
    let resp: Value = serde_json::from_str(&line).unwrap();
    assert!(text_of(&resp).contains("meiose_expand(\""));
    s.finish();
}

#[test]
fn error_responses_and_small_results_pass_through_untouched() {
    let mut s = Session::start(HN, &[]);
    s.init();
    let id = s.next_id + 1;
    let (line, _) = s.request_raw("tools/call", json!({"name":"fails","arguments":{}}));
    let v: Value = serde_json::from_str(&line).unwrap();
    assert_eq!(v["error"]["message"], "boom");
    let id2 = s.next_id + 1;
    let (line, _) = s.request_raw("tools/call", json!({"name":"small","arguments":{}}));
    assert_eq!(result_raw_of(&line), s.server_result_raw(id2));
    let _ = id;
    s.finish();
}

#[test]
fn stats_and_log_report_the_session() {
    let mut s = Session::start(HN, &[]);
    s.init();
    let snap = s.call("browser_snapshot", json!({}));
    let eid = expand_id(text_of(&snap));
    s.call("small", json!({}));
    s.call("meiose_expand", json!({"id": eid}));
    let data = s.finish();

    let run = |args: &[&str]| {
        let out = Command::new(env!("CARGO_BIN_EXE_meiose"))
            .args(args)
            .env("MEIOSE_DATA_DIR", data.path())
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap()
    };
    let stats = run(&["stats"]);
    assert!(stats.starts_with("Last 7 days: "), "{stats}");
    assert!(stats.contains("% saved"), "{stats}");
    assert!(
        stats.contains("2 tool calls, 1 trimmed · 1 expand ("),
        "{stats}"
    );
    assert!(stats.contains("browser_snapshot"), "{stats}");

    let log = run(&["log"]);
    assert!(log.contains("browser_snapshot"), "{log}");
    assert!(log.contains(&format!("id={eid}")), "{log}");
    assert!(log.contains("expand"), "{log}");

    let show = run(&["log", "--show", &eid]);
    assert!(show.contains("=== trimmed (what the agent saw) ==="));
    assert!(show.contains("=== original (what meiose_expand returns) ==="));
    assert!(
        show.contains("[cursor=pointer]"),
        "original keeps everything"
    );
}

#[test]
fn keep_days_zero_keeps_originals_in_memory_only() {
    let mut s = Session::start_with(HN, &[], &[("MEIOSE_KEEP_DAYS", "0")]);
    s.init();
    let snap = s.call("browser_snapshot", json!({}));
    let eid = expand_id(text_of(&snap));
    // Expand still works within the session...
    let full = s.call("meiose_expand", json!({"id": eid}));
    assert!(text_of(&full).contains("[cursor=pointer]"));
    let data = s.finish();
    // ...but nothing was written to disk.
    let conn = rusqlite::Connection::open(data.path().join("meiose.db")).unwrap();
    let n: i64 = conn
        .query_row("SELECT COUNT(*) FROM outputs", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 0);
}

#[test]
fn diff_after_a_budget_outline_only_refers_to_what_the_agent_saw() {
    // A budget small enough that the form page becomes an outline.
    let mut s = Session::start(FORM, &["--budget", "1500"]);
    s.init();
    let first = s.call("browser_snapshot", json!({}));
    let seen = text_of(&first).to_string();
    assert!(seen.contains("outline only"), "{seen}");
    let second = s.call("browser_snapshot", json!({"checked": true}));
    let d = text_of(&second);
    if let Some(start) = d.find("```diff\n") {
        // Every unchanged or removed line must be one the agent was shown.
        for line in d[start + 8..].lines().take_while(|l| *l != "```") {
            if let Some(old) = line.strip_prefix(' ').or_else(|| line.strip_prefix('-')) {
                assert!(seen.contains(old), "diff refers to an unseen line: {old:?}");
            }
        }
    } else {
        assert!(!d.contains("Unchanged since"), "{d}");
    }
    s.finish();
}

#[test]
fn query_and_full_expands_are_logged_apart() {
    let mut s = Session::start(HN, &[]);
    s.init();
    let snap = s.call("browser_snapshot", json!({}));
    let eid = expand_id(text_of(&snap));
    s.call("meiose_expand", json!({"id": eid, "query": "comments"}));
    s.call("meiose_expand", json!({"id": eid}));
    let data = s.finish();
    let conn = rusqlite::Connection::open(data.path().join("meiose.db")).unwrap();
    let mut kinds: Vec<Option<String>> = conn
        .prepare("SELECT handler FROM calls WHERE kind = 'expand' ORDER BY rowid")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    kinds.sort();
    assert_eq!(kinds, [None, Some("query".to_string())]);
}

#[test]
fn no_store_keeps_this_servers_originals_in_memory_only() {
    let mut s = Session::start(HN, &["--no-store"]);
    s.init();
    let snap = s.call("browser_snapshot", json!({}));
    let eid = expand_id(text_of(&snap));
    // Expand still works within the session...
    let full = s.call("meiose_expand", json!({"id": eid}));
    assert!(text_of(&full).contains("[cursor=pointer]"));
    let data = s.finish();
    let conn = rusqlite::Connection::open(data.path().join("meiose.db")).unwrap();
    // ...nothing was written to disk, but the call is still in the stats.
    let outputs: i64 = conn
        .query_row("SELECT COUNT(*) FROM outputs", [], |r| r.get(0))
        .unwrap();
    assert_eq!(outputs, 0);
    let calls: i64 = conn
        .query_row("SELECT COUNT(*) FROM calls WHERE kind = 'call'", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(calls, 1);
}
