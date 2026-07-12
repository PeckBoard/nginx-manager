# Peckboard nginx-manager Plugin

Manage an [Nginx Proxy Manager](https://github.com/firestar/nginx-proxy-manager)
instance from Peckboard sessions. NPM ships a built-in MCP server
(`POST <host>:81/api/mcp`, Streamable HTTP, Bearer `npm_…` API keys); this
plugin bridges it into every Peckboard session as MCP tools.

## Tools

| Tool | What it does |
| --- | --- |
| `npm_status` | Configuration + connectivity diagnostics: handshake, server info, negotiated protocol version, and the tool catalog your API key's scopes expose. |
| `npm_list_tools` | The live `npm_*` catalog (proxy hosts, redirection hosts, 404 hosts, streams, certificates, access lists, tags, users, audit log, reports). Compact by default; `names:[…]` returns full input schemas. |
| `npm_call` | Proxy one tool invocation (`tools/call`) to the NPM MCP server, e.g. `{"name":"npm_create_proxy_host","arguments":{…}}`. |
| `npm_configure` | Set `base_url` / `api_key` and verify the connection. |

Deliberately a passthrough surface rather than a static mirror of NPM's ~44
tools: the catalog is discovered live, so it always matches your NPM version
and your API key's scopes, and it doesn't bloat every session's tool list.

## Setup

1. In the NPM admin UI create an **API key** (scopes bound what the tools can
   do — e.g. `proxy_hosts:manage`, `certificates:view`).
2. Install this plugin from the Peckboard plugin registry and approve it. It
   asks for two permissions:
   - `provide_mcp_tools` — contribute the four tools above;
   - `http_request` — outbound HTTP **including private/LAN targets** (that is
     how it reaches a self-hosted NPM; approve accordingly).
3. Configure, either
   - from any session: `npm_configure` with
     `{"base_url":"http://192.168.1.10:81","api_key":"npm_…"}` (the key passes
     through the chat transcript), or
   - secret-free, in Peckboard's `config.json` (re-applied on every start,
     wins over `npm_configure`):

     ```json
     {
       "plugins": {
         "nginx-manager": {
           "config": {
             "base_url": "http://192.168.1.10:81",
             "api_key": "npm_xxxxxxxxxxxxxxxxxxxx"
           }
         }
       }
     }
     ```

4. `npm_status` should report `connected: true` and the tool catalog.

## How It Talks to NPM

`src/mcp_client.rs` implements the Streamable HTTP client: `initialize`
(capturing the `Mcp-Session-Id` header) → `notifications/initialized` →
`tools/list` / `tools/call`, parsing both plain-JSON and SSE-framed responses.
The session is cached across invocations and transparently re-established when
NPM forgets it (it keeps sessions in memory; a restart drops them). Slow calls
are expected — a Let's Encrypt issuance via `npm_create_certificate` takes tens
of seconds — so the manifest declares `call_timeout_secs: 120`.

Outbound HTTP goes through Peckboard's `peckboard_http_request` host function
(the WASM sandbox has no network); Peckboard ≥ the version introducing that
host function is required (`min_peckboard` in the registry entry).

## Build

```bash
./build.sh
# → target/wasm32-unknown-unknown/release/peckboard_nginx_manager_plugin.wasm
```

Copy the artifact to `<dataDir>/plugins/nginx-manager.wasm` (the file stem is
the plugin id) and restart Peckboard, or install via the registry.

## Tests

- `cargo test` — host-target unit tests (SSE parsing, response decoding,
  endpoint normalisation, verdict shapes; no network, no wasm).
- Peckboard core's `tests/nginx_manager_plugin.rs` drives the built wasm
  end-to-end against a mock NPM MCP endpoint on loopback (handshake, SSE and
  JSON framings, session-expiry recovery, slow calls). Build the wasm first or
  the test skips.
