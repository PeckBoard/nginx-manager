//! The plugin manifest: identity, the single `mcp.tool.invoke` hook, the MCP
//! tools this plugin provides (with their input schemas), the operator
//! settings (`base_url`, `api_key`) Peckboard's Settings UI renders, and the
//! host permissions those tools require (`provide_mcp_tools` + `http_request`
//! for the outbound calls to the NPM instance, which may live on the LAN).

/// Build the manifest JSON string returned by the `manifest` export.
pub fn manifest_json() -> String {
    let manifest = serde_json::json!({
        "description": env!("CARGO_PKG_DESCRIPTION"),
        "version": env!("CARGO_PKG_VERSION"),
        "repository": env!("CARGO_PKG_REPOSITORY"),

        "hooks": ["mcp.tool.invoke"],

        // tools/call against NPM can legitimately take tens of seconds (a
        // Let's Encrypt issuance under npm_create_certificate); the default
        // 2 s Extism call budget counts host-side HTTP time, so raise it.
        "call_timeout_secs": 120,

        // Operator-editable connection settings, rendered by Peckboard's
        // Settings UI (Plugins → nginx-manager → Settings). They live in the
        // same plugin-settings rows `config::load` reads, so a value saved in
        // the form is exactly what the next tool call uses — and unlike
        // npm_configure, the key never passes through a chat transcript.
        // Keys deliberately match the config.json seeding block.
        "settings": [
            {
                "key": "base_url",
                "title": "Nginx Proxy Manager URL",
                "description": "Root URL of the NPM admin interface, e.g. \"http://192.168.1.10:81\" — /api/mcp is appended automatically (a full …/api/mcp URL is also accepted).",
                "required": true,
                "type": "url",
                "placeholder": "http://192.168.1.10:81"
            },
            {
                "key": "api_key",
                "title": "API key",
                "description": "NPM API key used as the Bearer token (\"npm_…\"), created in the NPM UI under API Keys — its scopes bound everything npm_call can do.",
                "required": true,
                "type": "string",
                "secret": true,
                "placeholder": "npm_…"
            }
        ],
        "mcp_tools": [
            {
                "name": "npm_status",
                "title": "Nginx Proxy Manager connection status",
                "description": "Check the Nginx Proxy Manager connection: whether base_url/api_key are configured, and if so handshake with the NPM MCP endpoint and report server info, negotiated protocol version, the server's usage instructions, and which npm_* tools the configured API key's scopes expose. Start here when NPM tools misbehave.",
                "input_schema": {
                    "type": "object",
                    "properties": {},
                    "additionalProperties": false
                }
            },
            {
                "name": "npm_list_tools",
                "title": "List Nginx Proxy Manager tools",
                "description": "List the management tools the configured NPM API key can use — proxy hosts, redirection hosts, 404 hosts, TCP/UDP streams, certificates, access lists, tags, users, settings, audit log, reports. Returns a compact name + description catalog (with read-only/destructive hints); pass names:[…] to fetch the full JSON input schemas of specific tools before invoking them with npm_call.",
                "input_schema": {
                    "type": "object",
                    "properties": {
                        "names": {
                            "type": "array",
                            "items": { "type": "string" },
                            "description": "Return full definitions (including input schema) for just these tool names, e.g. [\"npm_create_proxy_host\"]. Omit for the compact catalog of every available tool."
                        }
                    },
                    "additionalProperties": false
                }
            },
            {
                "name": "npm_call",
                "title": "Call a Nginx Proxy Manager tool",
                "description": "Invoke one Nginx Proxy Manager tool by name (e.g. npm_list_proxy_hosts, npm_create_proxy_host, npm_renew_certificate) with arguments matching its input schema — discover names with npm_list_tools and schemas via its names:[…] parameter. The call is proxied to the configured NPM instance's MCP server and bounded by the API key's scopes. npm_delete_* tools take effect immediately; be sure before deleting.",
                "input_schema": {
                    "type": "object",
                    "properties": {
                        "name": {
                            "type": "string",
                            "description": "The NPM tool to invoke, e.g. \"npm_list_proxy_hosts\"."
                        },
                        "arguments": {
                            "type": "object",
                            "description": "Arguments per that tool's input schema (npm_list_tools with names:[…] shows it). Omit for tools that take none."
                        }
                    },
                    "required": ["name"],
                    "additionalProperties": false
                }
            },
            {
                "name": "npm_configure",
                "title": "Configure the Nginx Proxy Manager connection",
                "description": "Set which Nginx Proxy Manager this Peckboard talks to: base_url of the NPM admin interface (e.g. \"http://192.168.1.10:81\") and api_key (an NPM API key, \"npm_…\", created in the NPM UI under API Keys — its scopes bound everything npm_call can do). Verifies the connection by default and reports the server info. Note the key passes through the chat transcript; operators who prefer not to can instead set both in Settings → Plugins → nginx-manager, or in Peckboard's config.json under plugins.nginx-manager.config.",
                "input_schema": {
                    "type": "object",
                    "properties": {
                        "base_url": {
                            "type": "string",
                            "description": "Root URL of the NPM instance, e.g. \"http://192.168.1.10:81\" — /api/mcp is appended automatically (a full …/api/mcp URL is also accepted)."
                        },
                        "api_key": {
                            "type": "string",
                            "description": "NPM API key used as the Bearer token, e.g. \"npm_…\"."
                        },
                        "verify": {
                            "type": "boolean",
                            "description": "Handshake with the endpoint after saving (default true)."
                        }
                    },
                    "additionalProperties": false
                }
            }
        ],

        "permissions": [
            "provide_mcp_tools",
            "http_request"
        ],
    });
    manifest.to_string()
}

#[cfg(test)]
mod tests {
    #[test]
    fn manifest_is_valid_json_with_required_fields() {
        let m: serde_json::Value = serde_json::from_str(&super::manifest_json()).unwrap();
        assert!(!m["description"].as_str().unwrap().is_empty());
        assert!(!m["version"].as_str().unwrap().is_empty());
        assert_eq!(m["hooks"], serde_json::json!(["mcp.tool.invoke"]));
        assert_eq!(m["mcp_tools"].as_array().unwrap().len(), 4);
        // Settings the operator edits in Peckboard's UI: URL + secret token,
        // keyed exactly as config.rs reads them.
        let settings = m["settings"].as_array().unwrap();
        assert_eq!(settings.len(), 2);
        assert_eq!(settings[0]["key"], "base_url");
        assert_eq!(settings[0]["type"], "url");
        assert_eq!(settings[0]["required"], true);
        assert_eq!(settings[1]["key"], "api_key");
        assert_eq!(settings[1]["type"], "string");
        assert_eq!(settings[1]["secret"], true);
        assert_eq!(settings[1]["required"], true);
        let perms = m["permissions"].as_array().unwrap();
        assert!(perms.contains(&serde_json::json!("provide_mcp_tools")));
        assert!(perms.contains(&serde_json::json!("http_request")));
        // Tool-name rule enforced by core: [a-z0-9_], ≤64 chars.
        for t in m["mcp_tools"].as_array().unwrap() {
            let name = t["name"].as_str().unwrap();
            assert!(
                name.chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
                    && name.len() <= 64,
                "bad tool name {name}"
            );
        }
    }
}
