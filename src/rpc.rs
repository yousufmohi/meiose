//! Just enough JSON-RPC to route messages, without re-serializing them.
//!
//! [`Envelope`] borrows raw slices of the frame (`id`, `params`, `result`) so
//! classifying a 5 MB response costs one validating scan and no copies of
//! the payload. Frames we do not modify are forwarded as their original bytes.

use serde::Deserialize;
use serde_json::value::RawValue;

#[derive(Deserialize)]
pub struct Envelope<'a> {
    #[serde(borrow, default)]
    pub id: Option<&'a RawValue>,
    #[serde(default)]
    pub method: Option<std::borrow::Cow<'a, str>>,
    #[serde(borrow, default)]
    pub params: Option<&'a RawValue>,
    #[serde(borrow, default)]
    pub result: Option<&'a RawValue>,
    #[serde(borrow, default)]
    pub error: Option<&'a RawValue>,
}

impl<'a> Envelope<'a> {
    /// `None` for anything that is not a single JSON-RPC object (batches,
    /// garbage, blank lines): such frames are forwarded untouched.
    pub fn parse(frame: &'a [u8]) -> Option<Envelope<'a>> {
        let trimmed = trim_ascii_ws(frame);
        if trimmed.first() != Some(&b'{') {
            return None;
        }
        serde_json::from_slice(trimmed).ok()
    }

    pub fn is_request(&self) -> bool {
        self.method.is_some() && self.id.is_some()
    }

    pub fn is_response(&self) -> bool {
        self.method.is_none()
            && self.id.is_some()
            && (self.result.is_some() || self.error.is_some())
    }

    /// The id as compact JSON text: `1`, `"abc"`. Used as a map key, so
    /// `1` and `"1"` stay distinct, as JSON-RPC requires.
    pub fn id_key(&self) -> Option<String> {
        self.id.map(|r| compact(r.get()))
    }
}

fn trim_ascii_ws(b: &[u8]) -> &[u8] {
    let start = b
        .iter()
        .position(|c| !c.is_ascii_whitespace())
        .unwrap_or(b.len());
    let end = b
        .iter()
        .rposition(|c| !c.is_ascii_whitespace())
        .map_or(start, |e| e + 1);
    &b[start..end]
}

/// Remove insignificant whitespace from a small JSON value (an id).
fn compact(raw: &str) -> String {
    match serde_json::from_str::<serde_json::Value>(raw) {
        Ok(v) => v.to_string(),
        Err(_) => raw.trim().to_string(),
    }
}

/// The line terminator a frame ended with, so replies we build match it.
pub fn terminator(frame: &[u8]) -> &'static [u8] {
    if frame.ends_with(b"\r\n") {
        b"\r\n"
    } else {
        b"\n"
    }
}

/// `{"jsonrpc":"2.0","id":<id>,"result":<result>}` with `result` spliced in
/// verbatim, so an expanded output is byte-for-byte what the server sent.
pub fn response_with_raw_result(id_raw: &str, result_raw: &str) -> Vec<u8> {
    let mut v = Vec::with_capacity(result_raw.len() + id_raw.len() + 40);
    v.extend_from_slice(b"{\"jsonrpc\":\"2.0\",\"id\":");
    v.extend_from_slice(id_raw.as_bytes());
    v.extend_from_slice(b",\"result\":");
    v.extend_from_slice(result_raw.as_bytes());
    v.push(b'}');
    v
}

/// A tool result carrying one text item; `is_error` marks a tool-level error
/// the model can read and recover from.
pub fn tool_text_response(id_raw: &str, text: &str, is_error: bool) -> Vec<u8> {
    let result = serde_json::json!({
        "content": [{ "type": "text", "text": text }],
        "isError": is_error,
    });
    response_with_raw_result(id_raw, &result.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_requests_responses_and_notifications() {
        let req = br#"{"jsonrpc":"2.0","id":7,"method":"tools/call","params":{"name":"x"}}"#;
        let e = Envelope::parse(req).unwrap();
        assert!(e.is_request() && !e.is_response());
        assert_eq!(e.id_key().as_deref(), Some("7"));

        let resp = br#"{"jsonrpc":"2.0","id":"a b","result":{}}"#;
        let e = Envelope::parse(resp).unwrap();
        assert!(e.is_response());
        assert_eq!(e.id_key().as_deref(), Some("\"a b\""));

        let note = br#"{"jsonrpc":"2.0","method":"notifications/progress"}"#;
        let e = Envelope::parse(note).unwrap();
        assert!(!e.is_request() && !e.is_response());
    }

    #[test]
    fn numeric_and_string_ids_stay_distinct() {
        let a = Envelope::parse(br#"{"id":1,"result":{}}"#)
            .unwrap()
            .id_key();
        let b = Envelope::parse(br#"{"id":"1","result":{}}"#)
            .unwrap()
            .id_key();
        assert_ne!(a, b);
    }

    #[test]
    fn non_objects_are_not_parsed() {
        for f in [&b"[1,2]"[..], b"", b"garbage", b"{\"id\":", b"   \n"] {
            assert!(
                Envelope::parse(f).is_none(),
                "{:?}",
                String::from_utf8_lossy(f)
            );
        }
    }

    #[test]
    fn raw_result_is_spliced_verbatim() {
        let original = r#"{"content":[{"type":"text","text":"aé  b"}],  "isError":false}"#;
        let out = response_with_raw_result("3", original);
        let s = String::from_utf8(out).unwrap();
        assert!(s.contains(original));
        let v: serde_json::Value = serde_json::from_str(&s).unwrap();
        assert_eq!(v["id"], 3);
    }

    #[test]
    fn tolerates_crlf_terminated_frames() {
        let e = Envelope::parse(b"{\"id\":1,\"result\":{}}\r\n").unwrap();
        assert!(e.is_response());
        assert_eq!(terminator(b"{}\r\n"), b"\r\n");
        assert_eq!(terminator(b"{}\n"), b"\n");
    }
}
