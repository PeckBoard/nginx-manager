//! FFI layer: the Peckboard core host functions this plugin calls, as thin
//! JSON wrappers.
//!
//! Every host function is JSON-string-in / JSON-string-out and returns an
//! `{"error": "..."}` envelope instead of trapping; [`call_host`] turns that
//! envelope into an `Err(String)` so tool code can use `?`.
//!
//! The FFI exists only on `wasm32` (the Extism host imports are unavailable on
//! the host target used for `cargo test`), so host builds get an
//! `unimplemented!()` stub the tests never reach.

/// Which host function a [`call_host`] targets. `HttpRequest` is gated
/// host-side on the `http_request` permission this plugin declares (see
/// `manifest.rs`); the plugin-settings pair is ungated (a plugin may always
/// read/write its own namespace).
pub enum HostFn {
    HttpRequest,
    GetPluginSetting,
    SetPluginSetting,
}

#[cfg(target_arch = "wasm32")]
mod imp {
    use super::HostFn;
    use extism_pdk::*;

    #[host_fn]
    extern "ExtismHost" {
        fn peckboard_http_request(input: String) -> String;
        fn peckboard_get_plugin_setting(input: String) -> String;
        fn peckboard_set_plugin_setting(input: String) -> String;
    }

    /// Invoke a host function with a JSON value, parse its JSON reply, and
    /// surface an `{"error": ...}` envelope (or a trap) as `Err(String)`.
    pub fn call_host(
        which: HostFn,
        input: &serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        let s = input.to_string();
        let out = unsafe {
            match which {
                HostFn::HttpRequest => peckboard_http_request(s),
                HostFn::GetPluginSetting => peckboard_get_plugin_setting(s),
                HostFn::SetPluginSetting => peckboard_set_plugin_setting(s),
            }
        }
        .map_err(|e| e.to_string())?;
        parse_envelope(&out)
    }

    fn parse_envelope(out: &str) -> Result<serde_json::Value, String> {
        let v: serde_json::Value =
            serde_json::from_str(out).map_err(|e| format!("host returned invalid json: {e}"))?;
        if let Some(err) = v.get("error").and_then(|e| e.as_str()) {
            return Err(err.to_string());
        }
        Ok(v)
    }
}

// Host-target stub so the crate links for `cargo test` (no host imports exist
// off-wasm; no test calls a host-backed function).
#[cfg(not(target_arch = "wasm32"))]
mod imp {
    use super::HostFn;

    pub fn call_host(
        _which: HostFn,
        _input: &serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        unimplemented!("host calls are only available on wasm32")
    }
}

pub use imp::call_host;
