//! A minimal MCP client speaking Streamable HTTP against the Nginx Proxy
//! Manager endpoint (`POST <base_url>/api/mcp`), built on the
//! `peckboard_http_request` host function.
//!
//! The NPM server (the `@modelcontextprotocol/sdk` `StreamableHTTPServerTransport`)
//! is strict about the dance:
//!
//! 1. `initialize` — no session header; the response carries a fresh
//!    `Mcp-Session-Id` header the server requires on everything after.
//! 2. `notifications/initialized` — acknowledged with `202 Accepted`.
//! 3. `tools/list` / `tools/call` — with the session header. There is no
//!    stateless mode; an unknown/expired session id gets a 400/404 JSON-RPC
//!    error, which this client answers by re-initialising once and retrying.
//! 4. `DELETE` on the endpoint tears the session down (done in `shutdown`).
//!
//! Every POST advertises `Accept: application/json, text/event-stream` (the
//! SDK 406es unless both are present) and the reply may be framed either as
//! plain JSON or as an SSE stream with the response message in a `data:`
//! line — [`parse_sse_message`] handles the latter.
//!
//! The WASM instance lives across tool invocations, so the negotiated session
//! is cached in a static and reused until the configuration changes, the
//! server forgets it, or the plugin shuts down.

use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::config::Config;
use crate::host::{HostFn, call_host};

/// MCP protocol revision requested at `initialize` (the server negotiates
/// down/up from here and returns the version both sides speak).
const PROTOCOL_VERSION: &str = "2025-03-26";

const HANDSHAKE_TIMEOUT_SECS: u64 = 15;
const LIST_TIMEOUT_SECS: u64 = 30;
/// `tools/call` may drive slow NPM work (a Let's Encrypt issuance under
/// `npm_create_certificate` takes tens of seconds).
const CALL_TIMEOUT_SECS: u64 = 90;

/// An established MCP session, valid for one (base_url, api_key) pair.
#[derive(Clone, Debug)]
pub struct McpSession {
    pub id: String,
    pub protocol_version: String,
    fingerprint: String,
}

static SESSION: Mutex<Option<McpSession>> = Mutex::new(None);
static NEXT_ID: AtomicU64 = AtomicU64::new(1);

fn next_id() -> u64 {
    NEXT_ID.fetch_add(1, Ordering::Relaxed)
}

/// Drop the cached session (configuration changed or server forgot it).
pub fn invalidate_session() {
    if let Ok(mut s) = SESSION.lock() {
        *s = None;
    }
}

/// What one HTTP exchange with the endpoint produced.
struct HttpReply {
    status: u16,
    content_type: String,
    session_header: Option<String>,
    body: String,
}

/// One JSON-RPC exchange, decoded.
#[derive(Debug)]
enum Reply {
    /// A full JSON-RPC message (has `result` or `error`).
    Message(serde_json::Value),
    /// `202 Accepted` / empty 2xx — the fate of notifications.
    Accepted,
    /// The server no longer knows our `Mcp-Session-Id`.
    Expired,
}

/// POST `payload` (or DELETE when `payload` is `None`) to the MCP endpoint.
fn http_exchange(
    cfg: &Config,
    session: Option<&McpSession>,
    payload: Option<&serde_json::Value>,
    timeout_secs: u64,
) -> Result<HttpReply, String> {
    let mut headers = serde_json::json!({
        "authorization": format!("Bearer {}", cfg.api_key),
        "accept": "application/json, text/event-stream",
    });
    if payload.is_some() {
        headers["content-type"] = "application/json".into();
    }
    if let Some(s) = session {
        headers["mcp-session-id"] = s.id.clone().into();
        headers["mcp-protocol-version"] = s.protocol_version.clone().into();
    }
    let mut req = serde_json::json!({
        "url": cfg.endpoint(),
        "method": if payload.is_some() { "POST" } else { "DELETE" },
        "headers": headers,
        "timeout_secs": timeout_secs,
    });
    if let Some(p) = payload {
        req["body"] = p.to_string().into();
    }
    let out = call_host(HostFn::HttpRequest, &req)?;
    let status = out.get("status").and_then(|v| v.as_u64()).unwrap_or(0) as u16;
    let header = |name: &str| {
        out.get("headers")
            .and_then(|h| h.get(name))
            .and_then(|v| v.as_str())
            .map(str::to_string)
    };
    Ok(HttpReply {
        status,
        content_type: header("content-type").unwrap_or_default(),
        session_header: header("mcp-session-id"),
        body: out
            .get("body")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string(),
    })
}

/// Decode an exchange into a [`Reply`]. Pure, so the framings are testable.
fn decode_reply(
    status: u16,
    content_type: &str,
    body: &str,
    want_id: u64,
) -> Result<Reply, String> {
    if status == 401 || status == 403 {
        return Err(format!(
            "NPM rejected the API key ({status}). Check it exists, is not expired, and has the \
             scopes you need (npm_configure to update it)."
        ));
    }
    // The SDK answers a stale/unknown session with 400/404 before any body
    // framing; re-initialising is the fix, not an error.
    if (status == 400 || status == 404) && body.to_ascii_lowercase().contains("session") {
        return Ok(Reply::Expired);
    }
    if status == 202 || (status >= 200 && status < 300 && body.trim().is_empty()) {
        return Ok(Reply::Accepted);
    }
    let message = if content_type.contains("text/event-stream") {
        parse_sse_message(body, want_id)
            .ok_or_else(|| "SSE stream held no JSON-RPC response message".to_string())?
    } else {
        serde_json::from_str::<serde_json::Value>(body).map_err(|e| {
            format!(
                "endpoint returned HTTP {status} with a non-JSON-RPC body ({e}): {}",
                snippet(body)
            )
        })?
    };
    if status >= 400 {
        // JSON-RPC-shaped HTTP error (e.g. the router's own 4xx envelopes).
        if let Some(err) = message.get("error") {
            return Err(rpc_error_text(err));
        }
        return Err(format!("endpoint returned HTTP {status}: {}", snippet(body)));
    }
    Ok(Reply::Message(message))
}

/// Pull the JSON-RPC response for `want_id` out of an SSE-framed body:
/// events are separated by blank lines, each carrying the message across one
/// or more `data:` lines. Falls back to the last complete message when ids
/// don't line up (the server only streams responses to our own request here).
pub fn parse_sse_message(body: &str, want_id: u64) -> Option<serde_json::Value> {
    let mut fallback = None;
    for event in body.replace("\r\n", "\n").split("\n\n") {
        let data: Vec<&str> = event
            .lines()
            .filter_map(|l| l.strip_prefix("data:"))
            .map(|l| l.strip_prefix(' ').unwrap_or(l))
            .collect();
        if data.is_empty() {
            continue;
        }
        let Ok(msg) = serde_json::from_str::<serde_json::Value>(&data.join("\n")) else {
            continue;
        };
        if msg.get("result").is_none() && msg.get("error").is_none() {
            continue; // a notification or ping, not a response
        }
        if msg.get("id") == Some(&serde_json::Value::from(want_id)) {
            return Some(msg);
        }
        fallback = Some(msg);
    }
    fallback
}

fn snippet(body: &str) -> String {
    let trimmed = body.trim();
    if trimmed.chars().count() > 300 {
        let head: String = trimmed.chars().take(300).collect();
        format!("{head}…")
    } else {
        trimmed.to_string()
    }
}

fn rpc_error_text(err: &serde_json::Value) -> String {
    let code = err.get("code").and_then(|c| c.as_i64()).unwrap_or(0);
    let message = err
        .get("message")
        .and_then(|m| m.as_str())
        .unwrap_or("unknown error");
    match err.get("data") {
        Some(d) if !d.is_null() => format!("NPM MCP error {code}: {message} ({d})"),
        _ => format!("NPM MCP error {code}: {message}"),
    }
}

/// Send one JSON-RPC request on an existing session.
fn request_on(
    cfg: &Config,
    session: &McpSession,
    method: &str,
    params: serde_json::Value,
    timeout_secs: u64,
) -> Result<Reply, String> {
    let id = next_id();
    let payload = serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": method,
        "params": params,
    });
    let reply = http_exchange(cfg, Some(session), Some(&payload), timeout_secs)?;
    decode_reply(reply.status, &reply.content_type, &reply.body, id)
}

/// Full handshake: `initialize` (capturing the session header and negotiated
/// protocol version) then `notifications/initialized`. Returns the new
/// session and the `initialize` result (serverInfo, capabilities,
/// instructions).
pub fn initialize_fresh(cfg: &Config) -> Result<(McpSession, serde_json::Value), String> {
    let id = next_id();
    let payload = serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "initialize",
        "params": {
            "protocolVersion": PROTOCOL_VERSION,
            "capabilities": {},
            "clientInfo": {
                "name": "peckboard-nginx-manager",
                "version": env!("CARGO_PKG_VERSION"),
            },
        },
    });
    let reply = http_exchange(cfg, None, Some(&payload), HANDSHAKE_TIMEOUT_SECS)?;
    let session_id = reply.session_header.clone();
    let message = match decode_reply(reply.status, &reply.content_type, &reply.body, id)? {
        Reply::Message(m) => m,
        Reply::Accepted => return Err("initialize was accepted but returned no result".into()),
        Reply::Expired => return Err("initialize rejected as an expired session".into()),
    };
    if let Some(err) = message.get("error") {
        return Err(rpc_error_text(err));
    }
    let result = message
        .get("result")
        .cloned()
        .ok_or_else(|| "initialize response had no result".to_string())?;
    let session_id = session_id.ok_or_else(|| {
        "server sent no Mcp-Session-Id header on initialize — not a Streamable HTTP MCP endpoint?"
            .to_string()
    })?;
    let session = McpSession {
        id: session_id,
        protocol_version: result
            .get("protocolVersion")
            .and_then(|v| v.as_str())
            .unwrap_or(PROTOCOL_VERSION)
            .to_string(),
        fingerprint: cfg.fingerprint(),
    };
    // The SDK expects the initialized notification before serving requests.
    let note = serde_json::json!({ "jsonrpc": "2.0", "method": "notifications/initialized" });
    let reply = http_exchange(cfg, Some(&session), Some(&note), HANDSHAKE_TIMEOUT_SECS)?;
    match decode_reply(reply.status, &reply.content_type, &reply.body, 0)? {
        Reply::Accepted | Reply::Message(_) => {}
        Reply::Expired => return Err("session expired during the initialize handshake".into()),
    }
    if let Ok(mut slot) = SESSION.lock() {
        *slot = Some(session.clone());
    }
    Ok((session, result))
}

/// The cached session for `cfg`, creating one if missing or stale.
fn ensure_session(cfg: &Config) -> Result<McpSession, String> {
    if let Ok(slot) = SESSION.lock()
        && let Some(s) = slot.as_ref()
        && s.fingerprint == cfg.fingerprint()
    {
        return Ok(s.clone());
    }
    initialize_fresh(cfg).map(|(s, _)| s)
}

/// One request with automatic re-initialise-and-retry when the server has
/// forgotten our session (NPM keeps sessions in memory; a restart drops them).
fn request(
    cfg: &Config,
    method: &str,
    params: serde_json::Value,
    timeout_secs: u64,
) -> Result<serde_json::Value, String> {
    let session = ensure_session(cfg)?;
    let reply = request_on(cfg, &session, method, params.clone(), timeout_secs)?;
    let message = match reply {
        Reply::Message(m) => m,
        Reply::Accepted => return Err(format!("{method} was accepted but returned no result")),
        Reply::Expired => {
            invalidate_session();
            let session = ensure_session(cfg)?;
            match request_on(cfg, &session, method, params, timeout_secs)? {
                Reply::Message(m) => m,
                Reply::Accepted => {
                    return Err(format!("{method} was accepted but returned no result"));
                }
                Reply::Expired => {
                    return Err("session expired again right after re-initialising".into());
                }
            }
        }
    };
    if let Some(err) = message.get("error") {
        return Err(rpc_error_text(err));
    }
    message
        .get("result")
        .cloned()
        .ok_or_else(|| format!("{method} response had no result"))
}

/// `tools/list`, following pagination cursors until exhausted. Returns the
/// flat tool array.
pub fn list_tools(cfg: &Config) -> Result<Vec<serde_json::Value>, String> {
    let mut tools = Vec::new();
    let mut cursor: Option<String> = None;
    loop {
        let params = match &cursor {
            Some(c) => serde_json::json!({ "cursor": c }),
            None => serde_json::json!({}),
        };
        let result = request(cfg, "tools/list", params, LIST_TIMEOUT_SECS)?;
        if let Some(page) = result.get("tools").and_then(|t| t.as_array()) {
            tools.extend(page.iter().cloned());
        }
        match result.get("nextCursor").and_then(|c| c.as_str()) {
            Some(c) if !c.is_empty() => cursor = Some(c.to_string()),
            _ => return Ok(tools),
        }
    }
}

/// `tools/call` for one remote tool. Returns the raw MCP result
/// (`{"content": [...], "isError"?}`); unwrapping is the caller's business.
pub fn call_tool(
    cfg: &Config,
    name: &str,
    arguments: serde_json::Value,
) -> Result<serde_json::Value, String> {
    request(
        cfg,
        "tools/call",
        serde_json::json!({ "name": name, "arguments": arguments }),
        CALL_TIMEOUT_SECS,
    )
}

/// Best-effort `DELETE` of the cached session (plugin shutdown). NPM keeps
/// sessions in memory until told otherwise, so be a polite client.
pub fn shutdown_session(cfg: Option<&Config>) {
    let session = match SESSION.lock() {
        Ok(mut slot) => slot.take(),
        Err(_) => None,
    };
    if let (Some(cfg), Some(session)) = (cfg, session) {
        let _ = http_exchange(cfg, Some(&session), None, HANDSHAKE_TIMEOUT_SECS);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sse_single_event_matching_id() {
        let body = "event: message\ndata: {\"jsonrpc\":\"2.0\",\"id\":7,\"result\":{\"ok\":true}}\n\n";
        let msg = parse_sse_message(body, 7).unwrap();
        assert_eq!(msg["result"]["ok"], true);
    }

    #[test]
    fn sse_multiline_data_and_crlf() {
        let body = "data: {\"jsonrpc\":\"2.0\",\r\ndata: \"id\":3,\"result\":{\"n\":1}}\r\n\r\n";
        let msg = parse_sse_message(body, 3).unwrap();
        assert_eq!(msg["result"]["n"], 1);
    }

    #[test]
    fn sse_skips_notifications_and_falls_back() {
        let body = concat!(
            "data: {\"jsonrpc\":\"2.0\",\"method\":\"notifications/progress\"}\n\n",
            "data: {\"jsonrpc\":\"2.0\",\"id\":99,\"result\":{\"v\":2}}\n\n",
        );
        // id doesn't match, but the only response message is still returned.
        let msg = parse_sse_message(body, 1).unwrap();
        assert_eq!(msg["result"]["v"], 2);
    }

    #[test]
    fn decode_plain_json_reply() {
        let r = decode_reply(
            200,
            "application/json",
            "{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{}}",
            1,
        );
        assert!(matches!(r, Ok(Reply::Message(_))));
    }

    #[test]
    fn decode_expired_session_variants() {
        let r = decode_reply(
            404,
            "application/json",
            "{\"jsonrpc\":\"2.0\",\"error\":{\"code\":-32001,\"message\":\"Unknown or expired MCP session\"},\"id\":null}",
            1,
        );
        assert!(matches!(r, Ok(Reply::Expired)));
        let r = decode_reply(
            400,
            "application/json",
            "{\"jsonrpc\":\"2.0\",\"error\":{\"code\":-32000,\"message\":\"Bad Request: No valid session ID provided\"},\"id\":null}",
            1,
        );
        assert!(matches!(r, Ok(Reply::Expired)));
    }

    #[test]
    fn decode_auth_failure_is_actionable() {
        let e = decode_reply(401, "application/json", "{}", 1).unwrap_err();
        assert!(e.contains("API key"), "{e}");
    }

    #[test]
    fn decode_accepted_notification() {
        assert!(matches!(
            decode_reply(202, "", "", 0),
            Ok(Reply::Accepted)
        ));
    }

    #[test]
    fn rpc_error_text_includes_code_and_data() {
        let e = rpc_error_text(&serde_json::json!({
            "code": -32602, "message": "bad params", "data": {"field": "port"}
        }));
        assert!(e.contains("-32602") && e.contains("bad params") && e.contains("port"), "{e}");
    }
}
