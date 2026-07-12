//! Peckboard nginx-manager plugin (WASM / Extism).
//!
//! Bridges the built-in MCP server of an [Nginx Proxy Manager] instance into
//! Peckboard sessions as MCP tools, via the `mcp.tool.invoke` hook:
//!
//! - **npm_status** — configuration + connectivity diagnostics.
//! - **npm_list_tools** — the live `npm_*` tool catalog the configured API
//!   key's scopes expose (full input schemas on demand).
//! - **npm_call** — proxy one tool invocation (`tools/call`) to NPM.
//! - **npm_configure** — set `base_url` / `api_key`, verify the handshake.
//!
//! The remote endpoint speaks MCP Streamable HTTP (`POST <base>/api/mcp`,
//! Bearer `npm_…` API key); `src/mcp_client.rs` owns that dance (initialize,
//! `Mcp-Session-Id`, SSE-or-JSON response framing, expiry recovery). Outbound
//! HTTP happens through the `peckboard_http_request` host function — gated on
//! the `http_request` permission, which exists so plugins can reach
//! self-hosted services on private networks; the operator grants it at
//! install.
//!
//! [Nginx Proxy Manager]: https://github.com/firestar/nginx-proxy-manager
//!
//! ## Plugin interface
//!
//! Core expects four exports (`peckboard/src/plugin/manager.rs`):
//! - `manifest` — declares the hook handled and the MCP tools provided.
//! - `init` — called once on load with the plugin's config block; seeds
//!   `base_url` / `api_key` plugin settings from it when present.
//! - `handle` — called per hook with `{ "hook", "payload" }`; returns a Verdict.
//! - `shutdown` — tears down the cached MCP session (best-effort `DELETE`).

#![cfg_attr(not(target_arch = "wasm32"), allow(dead_code, unused_imports))]

mod config;
mod host;
mod manifest;
mod mcp_client;
mod tools;

use serde::Deserialize;

#[cfg(target_arch = "wasm32")]
mod entry {
    use super::*;
    use extism_pdk::*;

    #[plugin_fn]
    pub fn manifest() -> FnResult<String> {
        Ok(crate::manifest::manifest_json())
    }

    #[plugin_fn]
    pub fn init(config: String) -> FnResult<String> {
        Ok(crate::init_impl(&config))
    }

    #[plugin_fn]
    pub fn shutdown() -> FnResult<String> {
        crate::mcp_client::shutdown_session(crate::config::load().ok().as_ref());
        Ok(serde_json::json!({ "ok": true }).to_string())
    }

    #[plugin_fn]
    pub fn handle(input: String) -> FnResult<String> {
        let call: HookCall = serde_json::from_str(&input)?;
        match call.hook.as_str() {
            "mcp.tool.invoke" => Ok(handle_invoke(call.payload)),
            _ => Ok(skip()),
        }
    }
}

/// Seed `base_url` / `api_key` settings from the operator's config block
/// (`plugins.nginx-manager.config` in Peckboard's config.json). The file wins
/// over values set earlier via `npm_configure` — it is re-applied on every
/// load, so operators can rotate keys by editing one place.
fn init_impl(config: &str) -> String {
    let mut seeded = Vec::new();
    if let Ok(serde_json::Value::Object(map)) = serde_json::from_str::<serde_json::Value>(config) {
        for key in ["base_url", "api_key"] {
            if let Some(v) = map.get(key).and_then(|v| v.as_str())
                && !v.trim().is_empty()
                && config::set_setting(key, v.trim()).is_ok()
            {
                seeded.push(key);
            }
        }
        if !seeded.is_empty() {
            mcp_client::invalidate_session();
        }
    }
    serde_json::json!({ "ok": true, "seeded": seeded }).to_string()
}

/// The `{ "hook", "payload" }` envelope core passes to `handle`.
#[derive(Debug, Deserialize)]
struct HookCall {
    hook: String,
    #[serde(default)]
    payload: serde_json::Value,
}

/// Dispatch an `mcp.tool.invoke` to the right tool. A tool's `Err` becomes a
/// `Verdict::Cancel` (surfaced to the worker as an MCP tool error); an unknown
/// tool is also a Cancel. Success is a `Verdict::Allow` carrying the value.
fn handle_invoke(payload: serde_json::Value) -> String {
    let tool = payload
        .get("tool")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let args = payload
        .get("arguments")
        .cloned()
        .unwrap_or(serde_json::Value::Null);

    let result: Result<serde_json::Value, String> = match tool.as_str() {
        "npm_status" => tools::status_tool(args),
        "npm_list_tools" => tools::list_tools_tool(args),
        "npm_call" => tools::call_tool_tool(args),
        "npm_configure" => tools::configure_tool(args),
        other => return cancel(&format!("nginx-manager does not provide tool '{other}'")),
    };

    match result {
        Ok(value) => allow(value),
        Err(reason) => cancel(&reason),
    }
}

// ── Verdict helpers (mirror core's `Verdict` enum) ────────────────────

fn allow(value: serde_json::Value) -> String {
    serde_json::json!({ "verdict": "allow", "payload": value }).to_string()
}

fn cancel(reason: &str) -> String {
    serde_json::json!({ "verdict": "cancel", "reason": reason }).to_string()
}

fn skip() -> String {
    serde_json::json!({ "verdict": "skip" }).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_tool_is_cancelled() {
        let out = handle_invoke(serde_json::json!({ "tool": "npm_frobnicate", "arguments": {} }));
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["verdict"], "cancel");
        assert!(
            v["reason"].as_str().unwrap().contains("npm_frobnicate"),
            "{out}"
        );
    }

    #[test]
    fn verdict_helpers_shape() {
        let a: serde_json::Value =
            serde_json::from_str(&allow(serde_json::json!({ "x": 1 }))).unwrap();
        assert_eq!(a["verdict"], "allow");
        assert_eq!(a["payload"]["x"], 1);
        let c: serde_json::Value = serde_json::from_str(&cancel("nope")).unwrap();
        assert_eq!(c["verdict"], "cancel");
        assert_eq!(c["reason"], "nope");
        let s: serde_json::Value = serde_json::from_str(&skip()).unwrap();
        assert_eq!(s["verdict"], "skip");
    }
}
