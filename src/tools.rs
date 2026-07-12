//! The four MCP tools this plugin provides, on top of [`crate::mcp_client`].
//!
//! Deliberately a passthrough surface: NPM registers its own `npm_*` tool set
//! per API-key scope (44 tools on a full-access key), and mirroring those
//! statically would bloat every session's tool list and drift when NPM
//! changes. Instead sessions discover the live catalog with `npm_list_tools`
//! and invoke through `npm_call`.

use crate::config;
use crate::mcp_client;

/// `npm_status` — configuration + connectivity report. Never a hard error:
/// diagnostics belong in the payload.
pub fn status_tool(_args: serde_json::Value) -> Result<serde_json::Value, String> {
    let cfg = match config::load() {
        Ok(c) => c,
        Err(e) => {
            return Ok(serde_json::json!({ "configured": false, "error": e }));
        }
    };
    let mut out = serde_json::json!({
        "configured": true,
        "base_url": cfg.base_url,
        "endpoint": cfg.endpoint(),
        "api_key": config::mask_key(&cfg.api_key),
    });
    match mcp_client::initialize_fresh(&cfg) {
        Ok((session, init)) => {
            out["connected"] = true.into();
            out["protocol_version"] = session.protocol_version.clone().into();
            if let Some(v) = init.get("serverInfo") {
                out["server"] = v.clone();
            }
            if let Some(v) = init.get("instructions") {
                out["instructions"] = v.clone();
            }
            match mcp_client::list_tools(&cfg) {
                Ok(tools) => {
                    out["tool_count"] = tools.len().into();
                    out["tools"] = tools
                        .iter()
                        .filter_map(|t| t.get("name").and_then(|n| n.as_str()))
                        .collect::<Vec<_>>()
                        .into();
                }
                Err(e) => out["tools_error"] = e.into(),
            }
        }
        Err(e) => {
            out["connected"] = false.into();
            out["error"] = e.into();
        }
    }
    Ok(out)
}

/// `npm_list_tools` — the live remote catalog. Compact (name, description,
/// hints) by default; full input schemas for the tools named in `names`.
pub fn list_tools_tool(args: serde_json::Value) -> Result<serde_json::Value, String> {
    let cfg = config::load()?;
    let tools = mcp_client::list_tools(&cfg)?;
    let names: Vec<String> = args
        .get("names")
        .and_then(|n| n.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();

    if !names.is_empty() {
        let mut found = Vec::new();
        let mut missing = Vec::new();
        for name in &names {
            match tools
                .iter()
                .find(|t| t.get("name").and_then(|n| n.as_str()) == Some(name))
            {
                Some(t) => found.push(t.clone()),
                None => missing.push(name.clone()),
            }
        }
        let mut out = serde_json::json!({ "tools": found });
        if !missing.is_empty() {
            out["missing"] = serde_json::json!({
                "names": missing,
                "note": "not registered for this API key — its scopes may exclude the resource",
            });
        }
        return Ok(out);
    }

    let compact: Vec<serde_json::Value> = tools
        .iter()
        .map(|t| {
            let mut e = serde_json::json!({
                "name": t.get("name").cloned().unwrap_or_default(),
                "description": t.get("description").cloned().unwrap_or_default(),
            });
            if let Some(a) = t.get("annotations") {
                if let Some(v) = a.get("readOnlyHint").and_then(|v| v.as_bool()) {
                    e["read_only"] = v.into();
                }
                if let Some(v) = a.get("destructiveHint").and_then(|v| v.as_bool()) {
                    e["destructive"] = v.into();
                }
            }
            e
        })
        .collect();
    Ok(serde_json::json!({
        "count": compact.len(),
        "tools": compact,
        "note": "pass names:[\"npm_…\"] to get a tool's full input schema before npm_call",
    }))
}

/// `npm_call` — proxy one `tools/call` to the NPM MCP server and unwrap its
/// text-content result.
pub fn call_tool_tool(args: serde_json::Value) -> Result<serde_json::Value, String> {
    let name = args
        .get("name")
        .and_then(|n| n.as_str())
        .map(str::trim)
        .filter(|n| !n.is_empty())
        .ok_or_else(|| "'name' is required — an NPM tool name like npm_list_proxy_hosts (discover them with npm_list_tools)".to_string())?;
    let arguments = args
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| serde_json::json!({}));
    if !arguments.is_object() {
        return Err("'arguments' must be an object matching the tool's input schema".to_string());
    }
    let cfg = config::load()?;
    let result = mcp_client::call_tool(&cfg, name, arguments)?;
    unwrap_tool_result(name, &result)
}

/// Flatten the MCP `{"content":[{"type":"text",…}],"isError"?}` result the
/// NPM server produces: errors become tool errors, JSON text comes back as
/// JSON, anything else as `{"text": …}`.
fn unwrap_tool_result(name: &str, result: &serde_json::Value) -> Result<serde_json::Value, String> {
    let texts: Vec<&str> = result
        .get("content")
        .and_then(|c| c.as_array())
        .map(|items| {
            items
                .iter()
                .filter(|i| i.get("type").and_then(|t| t.as_str()) == Some("text"))
                .filter_map(|i| i.get("text").and_then(|t| t.as_str()))
                .collect()
        })
        .unwrap_or_default();
    let joined = texts.join("\n");
    if result.get("isError").and_then(|e| e.as_bool()) == Some(true) {
        return Err(if joined.is_empty() {
            format!("{name} failed with no error detail")
        } else {
            joined
        });
    }
    if joined.is_empty() {
        return Ok(serde_json::json!({ "ok": true }));
    }
    Ok(serde_json::from_str(&joined).unwrap_or(serde_json::json!({ "text": joined })))
}

/// `npm_configure` — store `base_url` / `api_key`, then (by default) verify
/// by handshaking with the endpoint.
pub fn configure_tool(args: serde_json::Value) -> Result<serde_json::Value, String> {
    let base_url = args
        .get("base_url")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let api_key = args
        .get("api_key")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let verify = args.get("verify").and_then(|v| v.as_bool()).unwrap_or(true);
    if base_url.is_none() && api_key.is_none() {
        return Err(
            "nothing to configure — pass base_url and/or api_key (or use npm_status to inspect \
             the current state)"
                .to_string(),
        );
    }
    if let Some(url) = base_url {
        if !url.starts_with("http://") && !url.starts_with("https://") {
            return Err("base_url must start with http:// or https://".to_string());
        }
        config::set_setting("base_url", url)?;
    }
    if let Some(key) = api_key {
        config::set_setting("api_key", key)?;
    }
    mcp_client::invalidate_session();

    let mut out = serde_json::json!({ "saved": true });
    if let Some(url) = base_url {
        out["base_url"] = url.into();
    }
    if let Some(key) = api_key {
        out["api_key"] = config::mask_key(key).into();
    }
    if verify {
        match config::load()
            .and_then(|cfg| mcp_client::initialize_fresh(&cfg).map(|(_, init)| (cfg, init)))
        {
            Ok((cfg, init)) => {
                out["verified"] = true.into();
                if let Some(v) = init.get("serverInfo") {
                    out["server"] = v.clone();
                }
                if let Ok(tools) = mcp_client::list_tools(&cfg) {
                    out["tool_count"] = tools.len().into();
                }
            }
            Err(e) => {
                out["verified"] = false.into();
                out["verify_error"] = e.into();
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unwrap_parses_json_text_content() {
        let r = unwrap_tool_result(
            "npm_list_proxy_hosts",
            &serde_json::json!({
                "content": [{"type": "text", "text": "[{\"id\":1,\"domain_names\":[\"a.example.com\"]}]"}]
            }),
        )
        .unwrap();
        assert_eq!(r[0]["id"], 1);
    }

    #[test]
    fn unwrap_wraps_plain_text() {
        let r = unwrap_tool_result(
            "npm_get_setting",
            &serde_json::json!({ "content": [{"type": "text", "text": "not json"}] }),
        )
        .unwrap();
        assert_eq!(r["text"], "not json");
    }

    #[test]
    fn unwrap_surfaces_is_error_as_err() {
        let e = unwrap_tool_result(
            "npm_delete_proxy_host",
            &serde_json::json!({
                "isError": true,
                "content": [{"type": "text", "text": "Error: NPM API error: Not Found"}]
            }),
        )
        .unwrap_err();
        assert!(e.contains("Not Found"), "{e}");
    }

    #[test]
    fn unwrap_empty_content_is_ok() {
        let r = unwrap_tool_result("npm_x", &serde_json::json!({ "content": [] })).unwrap();
        assert_eq!(r["ok"], true);
    }
}
