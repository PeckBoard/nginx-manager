//! Plugin configuration: where the target Nginx Proxy Manager lives and how
//! to authenticate against it.
//!
//! The two values (`base_url`, `api_key`) are stored in the plugin's own
//! settings namespace (`peckboard_get/set_plugin_setting`). They get there in
//! one of three ways:
//!
//! - the operator edits them in Peckboard's Settings UI (Plugins →
//!   nginx-manager → Settings) — the manifest declares both fields, and the
//!   form writes the same settings rows read here;
//! - the operator puts them in Peckboard's `config.json` under
//!   `plugins.nginx-manager.config` — the plugin's `init` export seeds the
//!   settings from that block on every (re)start, so the file wins; or
//! - a session calls the `npm_configure` tool (convenient, but the key then
//!   passes through the chat transcript).

use crate::host::{HostFn, call_host};

/// A ready-to-use target: non-empty `base_url` and `api_key`.
#[derive(Clone, Debug, PartialEq)]
pub struct Config {
    pub base_url: String,
    pub api_key: String,
}

impl Config {
    /// The MCP endpoint URL. `base_url` is normally the NPM root
    /// (`http://host:81`) and `/api/mcp` is appended; a value that already
    /// ends in `/api/mcp` is used as-is so pasting the full endpoint also
    /// works.
    pub fn endpoint(&self) -> String {
        endpoint_from(&self.base_url)
    }

    /// Identity of this configuration for session caching: a cached MCP
    /// session is only reused while the target and credential are unchanged.
    pub fn fingerprint(&self) -> String {
        format!("{}\n{}", self.base_url, self.api_key)
    }
}

pub fn endpoint_from(base_url: &str) -> String {
    let trimmed = base_url.trim().trim_end_matches('/');
    if trimmed.ends_with("/api/mcp") {
        trimmed.to_string()
    } else {
        format!("{trimmed}/api/mcp")
    }
}

/// `api_key` shortened for echoing back to a session: enough to recognise,
/// never enough to reuse.
pub fn mask_key(key: &str) -> String {
    let chars: Vec<char> = key.chars().collect();
    if chars.len() <= 8 {
        return "****".to_string();
    }
    let head: String = chars[..6].iter().collect();
    format!("{head}…({} chars)", chars.len())
}

/// Load the configuration from plugin settings; a helpful error when either
/// half is missing.
pub fn load() -> Result<Config, String> {
    let base_url = get_setting("base_url")?.unwrap_or_default();
    let api_key = get_setting("api_key")?.unwrap_or_default();
    if base_url.trim().is_empty() || api_key.trim().is_empty() {
        return Err(
            "nginx-manager is not configured. Set the Nginx Proxy Manager URL and API key in \
             Settings → Plugins → nginx-manager, or set base_url (e.g. \"http://192.168.1.10:81\") \
             and api_key (an NPM API key, \"npm_…\") with the npm_configure tool, or put them in \
             Peckboard's config.json under plugins.nginx-manager.config and restart."
                .to_string(),
        );
    }
    Ok(Config {
        base_url: base_url.trim().to_string(),
        api_key: api_key.trim().to_string(),
    })
}

pub fn get_setting(key: &str) -> Result<Option<String>, String> {
    let out = call_host(HostFn::GetPluginSetting, &serde_json::json!({ "key": key }))?;
    Ok(match out.get("value") {
        None | Some(serde_json::Value::Null) => None,
        Some(serde_json::Value::String(s)) => Some(s.clone()),
        // Tolerate a non-string value (e.g. hand-edited): keep its JSON text.
        Some(v) => Some(v.to_string()),
    })
}

pub fn set_setting(key: &str, value: &str) -> Result<(), String> {
    call_host(
        HostFn::SetPluginSetting,
        &serde_json::json!({ "key": key, "value": value }),
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoint_normalisation() {
        assert_eq!(
            endpoint_from("http://npm.local:81"),
            "http://npm.local:81/api/mcp"
        );
        assert_eq!(
            endpoint_from("http://npm.local:81/"),
            "http://npm.local:81/api/mcp"
        );
        assert_eq!(
            endpoint_from("http://npm.local:81/api/mcp"),
            "http://npm.local:81/api/mcp"
        );
        assert_eq!(
            endpoint_from("  https://proxy.example.com/api/mcp/  "),
            "https://proxy.example.com/api/mcp"
        );
    }

    #[test]
    fn key_masking_never_leaks() {
        assert_eq!(mask_key("npm_abc"), "****");
        let masked = mask_key("npm_0123456789abcdef");
        assert!(masked.starts_with("npm_01"), "{masked}");
        assert!(!masked.contains("abcdef"), "{masked}");
    }
}
