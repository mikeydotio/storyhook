//! The JSON-RPC 2.0 envelope MCP's stdio transport carries, and the small
//! slice of the Model Context Protocol (2025-11-25) this server speaks:
//! `initialize`, `notifications/initialized`, `tools/list`, `tools/call`,
//! `ping`.
//!
//! This is framing only — parsing one line of JSON into a request, and
//! serializing an answer back into one line of JSON. It knows nothing about
//! stories, `Invocation`, or the daemon; [`super::tools`] and [`super::serve`]
//! own that. Keeping the split is what makes the framing testable against
//! nothing but byte buffers, and what stops a future protocol revision from
//! touching anything that talks to storyhook's own wire.

use serde_json::{Value, json};

/// A parsed line of input: either a request expecting an answer, or a
/// notification that gets none.
///
/// [`Self::id`] is `None` for a notification per JSON-RPC 2.0 — a request
/// object with no `id` member. `method`/`params` are read leniently, before
/// any strict deserialization, so a malformed request can still echo the
/// `id` it carried (if any) in its error reply — the same reason a
/// hand-rolled parser reads a byte at a time rather than rejecting the whole
/// document on the first symptom.
pub struct Incoming {
    pub id: Option<Value>,
    pub method: String,
    pub params: Value,
}

/// What went wrong turning one line of input into an [`Incoming`], with the
/// `id` recovered if the line was at least a JSON object carrying one.
pub struct FramingError {
    pub id: Value,
    pub code: i32,
    pub message: String,
}

/// JSON-RPC's own error codes for the framing layer. A tool-call failure
/// (a bad `--project`, a daemon that would not answer) is never one of
/// these — it is a successful RPC whose *result* says `isError: true`. See
/// [`super::tools::call_result`].
pub const PARSE_ERROR: i32 = -32700;
pub const INVALID_REQUEST: i32 = -32600;
pub const METHOD_NOT_FOUND: i32 = -32601;
pub const INVALID_PARAMS: i32 = -32602;

/// Parses one line of input into an [`Incoming`] or a [`FramingError`].
///
/// MCP's stdio transport does not batch — one JSON value per line — so a
/// top-level array is an invalid request rather than something this reads
/// item-by-item.
pub fn parse_line(line: &str) -> Result<Incoming, FramingError> {
    let value: Value = serde_json::from_str(line).map_err(|e| FramingError {
        id: Value::Null,
        code: PARSE_ERROR,
        message: format!("parse error: {e}"),
    })?;

    let Value::Object(map) = &value else {
        return Err(FramingError {
            id: Value::Null,
            code: INVALID_REQUEST,
            message: "a request must be a JSON object".to_string(),
        });
    };

    // Recovered defensively, ahead of every other check, so a request that
    // fails a later check still gets its `id` echoed back rather than
    // `null` — the whole reason this is two passes instead of one strict
    // `Deserialize`.
    let id = map.get("id").cloned();
    let error_id = id.clone().unwrap_or(Value::Null);

    match map.get("jsonrpc").and_then(Value::as_str) {
        Some("2.0") => {}
        _ => {
            return Err(FramingError {
                id: error_id,
                code: INVALID_REQUEST,
                message: "\"jsonrpc\" must be \"2.0\"".to_string(),
            });
        }
    }

    let Some(method) = map.get("method").and_then(Value::as_str) else {
        return Err(FramingError {
            id: error_id,
            code: INVALID_REQUEST,
            message: "a request must name a \"method\"".to_string(),
        });
    };

    let params = map.get("params").cloned().unwrap_or(Value::Null);

    Ok(Incoming {
        id,
        method: method.to_string(),
        params,
    })
}

/// A successful JSON-RPC reply carrying `result`.
pub fn ok_reply(id: Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

/// A JSON-RPC error reply. `id` is `Value::Null` when the failing request's
/// own id could not be recovered — the only case JSON-RPC 2.0 permits that.
pub fn error_reply(id: Value, code: i32, message: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": code, "message": message },
    })
}

impl From<FramingError> for Value {
    fn from(error: FramingError) -> Self {
        error_reply(error.id, error.code, &error.message)
    }
}
