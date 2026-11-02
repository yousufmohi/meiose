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
use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hasher};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::diff;
use crate::handlers::{self, Handler, Mode, TrimContext};
use crate::rpc::{self, Envelope};
use crate::store::{CallRecord, StoreHandle};

pub const EXPAND_TOOL: &str = "meiose_expand";
/// Only diff against a snapshot the agent saw this recently.
const DIFF_MAX_AGE: Duration = Duration::from_secs(10 * 60);
/// Requests whose responses never come (cancelled, crashed) must not leak.
const MAX_PENDING: usize = 10_000;

pub struct Config {
    pub mode: Mode,
    /// False with `--passthrough`: nothing is modified (stats still recorded).
    pub trim: bool,
    pub diff: bool,
    /// Short label for stats, e.g. "npx-playwright-mcp".
    pub server: String,
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
    /// (page url, trimmed yaml body the agent saw, when)
    last_snapshot: Option<(String, String, Instant)>,
}

pub enum ClientAction {
    /// Forward the frame to the server unchanged.
    Forward,
    /// Answer `meiose_expand` locally; build the reply with [`Interceptor::expand_reply`].
    Expand {
        id_raw: String,
        expand_id: Option<String>,
        terminator: &'static [u8],
    },
}

pub struct Interceptor {
    cfg: Config,
    handlers: Vec<Box<dyn Handler>>,
    store: Option<StoreHandle>,
    state: Mutex<State>,
    id_seed: u64,
    id_counter: AtomicU64,
}

impl Interceptor {
    pub fn new(cfg: Config, store: Option<StoreHandle>) -> Interceptor {
        let mut h = RandomState::new().build_hasher();
        h.write_u128(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0),
        );
        h.write_u32(std::process::id());
        Interceptor {
            cfg,
            handlers: handlers::all(),
            store,
            state: Mutex::default(),
            id_seed: h.finish(),
            id_counter: AtomicU64::new(0),
        }
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn active(&self) -> bool {
        self.cfg.trim && self.store.is_some()
    }

    /// Short, unguessable-enough id like `k3x9q2ab`.
    fn new_expand_id(&self) -> String {
        let n = self.id_counter.fetch_add(1, Ordering::Relaxed);
        let mut x = self.id_seed ^ n.wrapping_mul(0x9E37_79B9_7F4A_7C15);
        x ^= x >> 31;
        x = x.wrapping_mul(0xBF58_476D_1CE4_E5B9);
        x ^= x >> 29;
        const ALPHABET: &[u8] = b"abcdefghijkmnpqrstuvwxyz23456789";
        (0..8)
            .map(|i| ALPHABET[((x >> (i * 5)) & 31) as usize] as char)
            .collect()
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
                        terminator: rpc::terminator(frame),
                    };
                }
                insert_pending(&mut st, id_key, Pending::ToolCall { name });
            }
            "tools/list" => {
                let first_page = params.get("cursor").is_none_or(Value::is_null);
                insert_pending(&mut self.state(), id_key, Pending::ToolsList { first_page });
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
        terminator: &[u8],
    ) -> Vec<u8> {
        let mut out = match (expand_id, &self.store) {
            (None, _) => rpc::tool_text_response(
                id_raw,
                "meiose_expand needs an \"id\" argument: the id shown in the \
                 [meiose: … meiose_expand(\"<id>\")] note of the result you want in full.",
                true,
            ),
            (Some(eid), Some(store)) => match store.get_output(eid) {
                Some(raw) => {
                    store.record_call(CallRecord {
                        server: self.cfg.server.clone(),
                        tool: EXPAND_TOOL.into(),
                        kind: "expand",
                        handler: None,
                        text_before: String::new(),
                        text_after: content_text(&raw),
                        trim_us: 0,
                        expand_id: Some(eid.to_string()),
                    });
                    rpc::response_with_raw_result(id_raw, &raw)
                }
                None => rpc::tool_text_response(
                    id_raw,
                    &format!(
                        "meiose has no stored output for id \"{eid}\". Outputs are kept for 7 days \
                         (and only while disk space allows). Re-run the original tool call to get a \
                         fresh, complete result."
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

        let expand_id = self.new_expand_id();
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
        store.put_output(&expand_id, &self.cfg.server, tool, result_raw, &new_result);
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
        // What the agent sees if we do not send a diff.
        let view = trimmed.as_ref().map_or(text, |(_, t)| t.text.as_str());
        if let Some(url) = diff::page_url(view)
            && let Some(body) = diff::body(view)
        {
            let mut st = self.state();
            let prev = st.last_snapshot.take();
            st.last_snapshot = Some((url.to_string(), body.to_string(), Instant::now()));
            drop(st);
            if self.cfg.diff
                && let Some((prev_url, prev_body, when)) = prev
                && prev_url == url
                && when.elapsed() < DIFF_MAX_AGE
                && let Some(d) = catch_unwind(AssertUnwindSafe(|| {
                    diff::render(&prev_body, view, cx.expand_id)
                }))
                .ok()
                .flatten()
            {
                return Some(("playwright-diff", d));
            }
        }
        trimmed.map(|(h, t)| (h, t.text))
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
            page behind a snapshot diff.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "id": {
                    "type": "string",
                    "description": "The id from the meiose note, e.g. \"k3x9q2ab\"."
                }
            },
            "required": ["id"],
            "additionalProperties": false
        }
    })
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
