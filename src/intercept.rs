//! Where meiose intentionally changes traffic. Everything else is forwarded
//! byte-for-byte by the proxy.
//!
//! - agent → server `tools/call` / `tools/list`: remember the request id
//!   (only *client* requests are tracked, so ids reused by server-initiated
//!   requests in the other direction can never be confused with them).
//! - agent → server `tools/call meiose_expand`: answered here, not forwarded.
//! - server → agent `tools/list` result (first page): `meiose_expand` appended.
//! - server → agent `tools/call` result: text items trimmed (and repeat
//!   Playwright snapshots diffed); the original `result` is stored first.
//!
//! Safety rules:
//! - Nothing is trimmed until this process has advertised `meiose_expand`
//!   to the agent, and never when the store is unavailable.
//! - Every entry point catches panics and falls back to the original frame.

use std::collections::HashMap;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::diff;
use crate::handlers::{self, Handler, Mode, TrimContext, playwright};
use crate::rpc::{self, Envelope};
use crate::search;
use crate::store::{CallRecord, StoreHandle};

pub const EXPAND_TOOL: &str = "meiose_expand";
/// Only diff against a snapshot the agent saw this recently.
const DIFF_MAX_AGE: Duration = Duration::from_secs(10 * 60);
/// Claude Code saves an MCP text result over this many characters to a file
/// and shows the model a note instead. The model never saw such a view, so
/// a diff against it would be relative to nothing it has.
const CLAUDE_CODE_VISIBLE_CHARS: usize = 50_000;

/// The largest text the client shows its model, when known from the name
/// it gave in `initialize`. A modern-era session has no `initialize`, so
/// its client stays unknown and every repeat snapshot is diffed as before.
fn visible_chars(client_name: &str) -> Option<usize> {
    match client_name.to_ascii_lowercase().as_str() {
        "claude code" | "claude-code" => Some(CLAUDE_CODE_VISIBLE_CHARS),
        _ => None,
    }
}
/// Requests whose responses never come (cancelled, crashed) must not leak.
const MAX_PENDING: usize = 10_000;

pub struct Config {
    pub mode: Mode,
    /// False with `--passthrough`: nothing is modified (stats still recorded).
    pub trim: bool,
    pub diff: bool,
    /// Largest Playwright snapshot to send, in bytes; 0 means no limit.
    pub budget_bytes: usize,
    /// Short label for stats, e.g. "npx-playwright-mcp".
    pub server: String,
    /// Identity of this server ([`crate::store::server_key`]): expand only
    /// returns outputs stored under it.
    pub server_key: String,
}

enum Pending {
    ToolsList { first_page: bool },
    ToolCall { name: String },
}

#[derive(Default)]
struct State {
    pending: HashMap<String, Pending>,
    expand_advertised: bool,
    server_has_expand: bool,
    last_snapshot: Option<LastSnapshot>,
    /// See [`visible_chars`]; `None` when the client is not known.
    visible_chars: Option<usize>,
}

/// The previous Playwright snapshot, to diff the next one against.
struct LastSnapshot {
    url: String,
    /// The yaml body exactly as the agent was sent it.
    view_body: String,
    /// The yaml body as the server sent it. Two views can be identical
    /// while the page differs in parts the view does not show.
    original_body: String,
    /// Length of the text actually sent (the view, or a diff of it), to
    /// tell whether the agent's client could show it at all. Bytes, compared
    /// with a limit in characters, so multi-byte text errs towards not
    /// diffing; per text item, which is all Playwright sends.
    sent_len: usize,
    when: Instant,
}

pub enum ClientAction {
    /// Forward the frame to the server unchanged.
    Forward,
    /// Answer `meiose_expand` locally; build the reply with [`Interceptor::expand_reply`].
    Expand {
        id_raw: String,
        expand_id: Option<String>,
        query: Option<String>,
        terminator: &'static [u8],
    },
}

pub struct Interceptor {
    cfg: Config,
    handlers: Vec<Box<dyn Handler>>,
    store: Option<StoreHandle>,
    state: Mutex<State>,
}

impl Interceptor {
    pub fn new(cfg: Config, store: Option<StoreHandle>) -> Interceptor {
        Interceptor {
            cfg,
            handlers: handlers::all(),
            store,
            state: Mutex::default(),
        }
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn active(&self) -> bool {
        self.cfg.trim && self.store.is_some()
    }

    /// A fresh expand id like `k3x9q2abm7tw4`: 13 symbols of a 32-letter
    /// alphabet, 65 random bits from the OS. `None` if the OS has no
    /// randomness to give (then the output is not trimmed).
    fn new_expand_id(&self) -> Option<String> {
        new_expand_id()
    }

    // ---------------- agent → server ----------------

    pub fn on_client_frame(&self, frame: &[u8]) -> ClientAction {
        match catch_unwind(AssertUnwindSafe(|| self.client_inner(frame))) {
            Ok(a) => a,
            Err(_) => {
                tracing::error!("intercept panicked on a client frame; forwarding it unchanged");
                ClientAction::Forward
            }
        }
    }

    fn client_inner(&self, frame: &[u8]) -> ClientAction {
        inject_fault("client");
        let Some(env) = Envelope::parse(frame) else {
            return ClientAction::Forward;
        };
        let Some(method) = env.method.as_deref() else {
            return ClientAction::Forward;
        };
        if method == "notifications/cancelled" {
            if let Some(id) = env
                .params
                .and_then(|p| serde_json::from_str::<Value>(p.get()).ok())
                .and_then(|p| p.get("requestId").map(Value::to_string))
            {
                self.state().pending.remove(&id);
            }
            return ClientAction::Forward;
        }
        if !env.is_request() {
            return ClientAction::Forward;
        }
        let id_key = env.id_key().expect("request has id");
        let params: Value = env
            .params
            .and_then(|p| serde_json::from_str(p.get()).ok())
            .unwrap_or(Value::Null);
        match method {
            "tools/call" => {
                let name = params
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                let mut st = self.state();
                if name == EXPAND_TOOL && self.active() && !st.server_has_expand {
                    return ClientAction::Expand {
                        id_raw: env.id.expect("request has id").get().to_string(),
                        expand_id: params
                            .get("arguments")
                            .and_then(|a| a.get("id"))
                            .and_then(Value::as_str)
                            .map(|s| s.trim().trim_matches('"').to_string()),
                        query: params
                            .get("arguments")
                            .and_then(|a| a.get("query"))
                            .and_then(Value::as_str)
                            .map(str::trim)
                            .filter(|q| !q.is_empty())
                            .map(str::to_string),
                        terminator: rpc::terminator(frame),
                    };
                }
                insert_pending(&mut st, id_key, Pending::ToolCall { name });
            }
            "tools/list" => {
                let first_page = params.get("cursor").is_none_or(Value::is_null);
                insert_pending(&mut self.state(), id_key, Pending::ToolsList { first_page });
            }
            "initialize" => {
                let client = params
                    .get("clientInfo")
                    .and_then(|c| c.get("name"))
                    .and_then(Value::as_str)
                    .unwrap_or("");
                self.state().visible_chars = visible_chars(client);
            }
            _ => {}
        }
        ClientAction::Forward
    }

    /// Build the reply to a `meiose_expand` call. Blocking (may read the
    /// database); call it from `spawn_blocking`.
    pub fn expand_reply(
        &self,
        id_raw: &str,
        expand_id: Option<&str>,
        query: Option<&str>,
        terminator: &[u8],
    ) -> Vec<u8> {
        let mut out = match (expand_id, &self.store) {
            (None, _) => rpc::tool_text_response(
                id_raw,
                "meiose_expand needs an \"id\" argument: the id shown in the \
                 [meiose: … meiose_expand(\"<id>\")] note of the result you want in full.",
                true,
            ),
            (Some(eid), Some(store)) => match store.get_output(eid, &self.cfg.server_key) {
                Some(raw) => {
                    let found = query.map(|q| search::search(&content_text(&raw), q));
                    store.record_call(CallRecord {
                        server: self.cfg.server.clone(),
                        tool: EXPAND_TOOL.into(),
                        kind: "expand",
                        // Lets stats and the eval tell partial from full expands.
                        handler: query.map(|_| "query"),
                        text_before: String::new(),
                        text_after: found.clone().unwrap_or_else(|| content_text(&raw)),
                        trim_us: 0,
                        expand_id: Some(eid.to_string()),
                    });
                    match found {
                        Some(text) => rpc::tool_text_response(id_raw, &text, false),
                        None => rpc::response_with_raw_result(id_raw, &raw),
                    }
                }
                None => rpc::tool_text_response(
                    id_raw,
                    &format!(
                        "meiose has no stored output for id \"{eid}\" from this server. Outputs are kept for {} \
                         (MEIOSE_KEEP_HOURS), for this session only with --no-store, and only while \
                         disk space allows. Re-run the original tool call to get a fresh, complete result.",
                        crate::store::retention_text(crate::store::keep_hours())
                    ),
                    true,
                ),
            },
            (Some(_), None) => rpc::tool_text_response(
                id_raw,
                "meiose cannot expand right now: its store is unavailable. \
                 Re-run the original tool call to get a fresh result.",
                true,
            ),
        };
        out.extend_from_slice(terminator);
        out
    }

    // ---------------- server → agent ----------------

    /// Returns the frame to send to the agent: the original bytes unless this
    /// is a response meiose intentionally changes.
    pub fn on_server_frame(&self, frame: Vec<u8>) -> Vec<u8> {
        match catch_unwind(AssertUnwindSafe(|| self.server_inner(&frame))) {
            Ok(Some(changed)) => changed,
            Ok(None) => frame,
            Err(_) => {
                tracing::error!("intercept panicked on a server frame; forwarding it unchanged");
                frame
            }
        }
    }

    fn server_inner(&self, frame: &[u8]) -> Option<Vec<u8>> {
        inject_fault("server");
        let env = Envelope::parse(frame)?;
        if !env.is_response() {
            return None;
        }
        let key = env.id_key()?;
        let pending = self.state().pending.remove(&key)?;
        let result_raw = env.result?.get();
        let id_raw = env.id?.get();
        match pending {
            Pending::ToolsList { first_page } => {
                if !(self.active() && first_page) {
                    return None;
                }
                self.inject_expand_tool(id_raw, result_raw, rpc::terminator(frame))
            }
            Pending::ToolCall { name } => {
                self.trim_tool_result(&name, id_raw, result_raw, rpc::terminator(frame))
            }
        }
    }

    fn inject_expand_tool(&self, id_raw: &str, result_raw: &str, term: &[u8]) -> Option<Vec<u8>> {
        let mut result: Value = serde_json::from_str(result_raw).ok()?;
        let tools = result.get_mut("tools")?.as_array_mut()?;
        if tools
            .iter()
            .any(|t| t.get("name").and_then(Value::as_str) == Some(EXPAND_TOOL))
        {
            tracing::warn!(
                "the MCP server already has a tool named {EXPAND_TOOL}; trimming is disabled for this session"
            );
            self.state().server_has_expand = true;
            return None;
        }
        tools.push(expand_tool_definition());
        self.state().expand_advertised = true;
        let mut out = rpc::response_with_raw_result(id_raw, &result.to_string());
        out.extend_from_slice(term);
        Some(out)
    }

    fn trim_tool_result(
        &self,
        tool: &str,
        id_raw: &str,
        result_raw: &str,
        term: &[u8],
    ) -> Option<Vec<u8>> {
        let started = Instant::now();
        let mut result: Value = serde_json::from_str(result_raw).ok()?;
        let complete = result
            .get("resultType")
            .is_none_or(|t| t.as_str() == Some("complete"));
        let text_before = texts_of(&result);
        let record =
            |handler: Option<&'static str>, after: String, eid: Option<String>, us: u64| {
                if let Some(store) = &self.store {
                    store.record_call(CallRecord {
                        server: self.cfg.server.clone(),
                        tool: tool.to_string(),
                        kind: "call",
                        handler,
                        text_before: text_before.clone(),
                        text_after: after,
                        trim_us: us,
                        expand_id: eid,
                    });
                }
            };

        let (advertised, server_has_expand) = {
            let st = self.state();
            (st.expand_advertised, st.server_has_expand)
        };
        if !complete || !self.active() || !advertised || server_has_expand {
            record(None, text_before.clone(), None, 0);
            return None;
        }

        let Some(expand_id) = self.new_expand_id() else {
            tracing::warn!("no randomness for an expand id; sending this output untrimmed");
            record(None, text_before.clone(), None, 0);
            return None;
        };
        let cx = TrimContext {
            expand_id: &expand_id,
            mode: self.cfg.mode,
        };
        let mut used: Option<&'static str> = None;
        if let Some(items) = result.get_mut("content").and_then(Value::as_array_mut) {
            for item in items.iter_mut() {
                if item.get("type").and_then(Value::as_str) != Some("text") {
                    continue;
                }
                let Some(text) = item.get("text").and_then(Value::as_str) else {
                    continue;
                };
                if let Some((handler, new_text)) = self.trim_one(text, &cx) {
                    item["text"] = Value::String(new_text);
                    used = Some(handler);
                }
            }
        }
        let Some(handler) = used else {
            record(
                None,
                text_before.clone(),
                None,
                started.elapsed().as_micros() as u64,
            );
            return None;
        };

        let new_result = result.to_string();
        // Store the original before the trimmed output can reach the agent.
        let store = self.store.as_ref().expect("active() checked");
        if !store.put_output(
            &expand_id,
            &self.cfg.server_key,
            &self.cfg.server,
            tool,
            result_raw,
            &new_result,
        ) {
            // Never send a trimmed output whose original we could not keep.
            return None;
        }
        let us = started.elapsed().as_micros() as u64;
        record(
            Some(handler),
            texts_of(&result),
            Some(expand_id.clone()),
            us,
        );
        tracing::debug!(tool, handler, us, expand_id, "trimmed tool result");

        let mut out = rpc::response_with_raw_result(id_raw, &new_result);
        out.extend_from_slice(term);
        Some(out)
    }

    /// Trim one text item; for Playwright snapshots also try a diff against
    /// the previous snapshot of the same page.
    fn trim_one(&self, text: &str, cx: &TrimContext) -> Option<(&'static str, String)> {
        let trimmed = handlers::trim_text(&self.handlers, text, cx);
        let normal = trimmed.as_ref().map_or(text, |(_, t)| t.text.as_str());
        // Apply the budget before diffing, so a diff (and the snapshot kept
        // for the next one) is always relative to what the agent was sent.
        let fitted = self.fit_budget(text, normal, cx);
        // What the agent sees if we do not send a diff.
        let (handler, view): (Option<&'static str>, &str) = match (&fitted, &trimmed) {
            (Some((h, t)), _) => (Some(*h), t.as_str()),
            (None, Some((h, t))) => (Some(*h), t.text.as_str()),
            (None, None) => (None, text),
        };
        if let Some(url) = diff::page_url(view)
            && let Some(body) = diff::body(view)
        {
            let original = diff::body(text).unwrap_or(body);
            let (prev, visible) = {
                let mut st = self.state();
                (st.last_snapshot.take(), st.visible_chars)
            };
            let mut sent: Option<String> = None;
            if self.cfg.diff
                && let Some(prev) = &prev
                && prev.url == url
                && prev.when.elapsed() < DIFF_MAX_AGE
                // A base the client never showed its model is no base.
                && visible.is_none_or(|limit| prev.sent_len <= limit)
            {
                let page_unchanged = prev.original_body == original;
                sent = catch_unwind(AssertUnwindSafe(|| {
                    diff::render(&prev.view_body, view, cx.expand_id, page_unchanged)
                }))
                .ok()
                .flatten();
            }
            // Whatever is sent, the agent's picture of the page is now this view.
            self.state().last_snapshot = Some(LastSnapshot {
                url: url.to_string(),
                view_body: body.to_string(),
                original_body: original.to_string(),
                sent_len: sent.as_ref().map_or(view.len(), String::len),
                when: Instant::now(),
            });
            if let Some(d) = sent {
                return Some(("playwright-diff", d));
            }
        }
        handler.map(|h| (h, view.to_string()))
    }

    /// Shrink a snapshot that is over `--budget`: aggressive trimming first,
    /// then an outline. `None` when no budget applies.
    fn fit_budget(
        &self,
        text: &str,
        view: &str,
        cx: &TrimContext,
    ) -> Option<(&'static str, String)> {
        let budget = self.cfg.budget_bytes;
        if budget == 0 || view.len() <= budget || playwright::find_block(text).is_none() {
            return None;
        }
        let hard = TrimContext {
            expand_id: cx.expand_id,
            mode: Mode::Aggressive,
        };
        let aggressive =
            playwright::trim_snapshot(text, &hard).map_or_else(|| text.to_string(), |t| t.text);
        if aggressive.len() <= budget {
            return Some(("playwright-budget", aggressive));
        }
        // Leave room for the header and the note.
        let room = budget.saturating_sub(1024);
        playwright::outline(&aggressive, cx.expand_id, room).map(|o| ("playwright-outline", o))
    }
}

/// Debug builds only: `MEIOSE_TEST_PANIC=client|server` makes that side of
/// the intercept panic, so tests can prove the fail-open path end to end.
#[inline]
fn inject_fault(side: &str) {
    #[cfg(debug_assertions)]
    if std::env::var("MEIOSE_TEST_PANIC").is_ok_and(|v| v == side) {
        panic!("injected test fault ({side})");
    }
    #[cfg(not(debug_assertions))]
    let _ = side;
}

fn insert_pending(st: &mut State, key: String, p: Pending) {
    if st.pending.len() >= MAX_PENDING {
        tracing::warn!("{MAX_PENDING} requests never got responses; forgetting them");
        st.pending.clear();
    }
    st.pending.insert(key, p);
}

pub fn expand_tool_definition() -> Value {
    serde_json::json!({
        "name": EXPAND_TOOL,
        "description": "Return the complete, untrimmed result of an earlier tool call that meiose \
            shortened to save context. Use it when a result ends with a note like \
            [meiose: … call meiose_expand(\"<id>\") …] and you need something that was left out: \
            refs of non-interactive elements, shortened text, omitted rows or items, or the full \
            page behind a snapshot diff. Pass query to get only the matching lines with their \
            context, which is much smaller; prefer it unless you need everything.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "id": {
                    "type": "string",
                    "description": "The id from the meiose note, e.g. \"k3x9q2abm7tw4\"."
                },
                "query": {
                    "type": "string",
                    "description": "Optional text to find, case-insensitive. Separate alternatives with |."
                }
            },
            "required": ["id"],
            "additionalProperties": false
        }
    })
}

const ID_ALPHABET: &[u8] = b"abcdefghijkmnpqrstuvwxyz23456789";
/// Symbols per expand id: 13 × 5 bits = 65 bits.
pub const ID_LEN: usize = 13;

fn new_expand_id() -> Option<String> {
    let mut b = [0u8; ID_LEN];
    getrandom::fill(&mut b).ok()?;
    Some(
        b.iter()
            .map(|x| ID_ALPHABET[usize::from(x & 31)] as char)
            .collect(),
    )
}

/// All text content of a tool result, joined (for token stats).
fn texts_of(result: &Value) -> String {
    result
        .get("content")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|i| i.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default()
}

fn content_text(result_raw: &str) -> String {
    serde_json::from_str::<Value>(result_raw)
        .map(|v| texts_of(&v))
        .unwrap_or_default()
}
