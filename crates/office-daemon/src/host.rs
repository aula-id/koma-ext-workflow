//! `Host` trait: the seam between the driver (W7+) and the live `Koma` handle, so
//! driver/kernel-adjacent code can be tested against a scripted `FakeHost` instead of
//! a real socket (ARCHITECTURE.md 5.1).
//!
//! `Koma::call` errors come back as `{ "error": "..." }` values, never `Err`
//! (sdk.rs, recon gotcha) — [`is_grant_denied`] / [`is_timeout`] are the two generic
//! string-match classes the driver treats uniformly; everything else is verb-specific
//! and handled at the call site.

use koma_extension::Koma;
use serde_json::Value;
use std::collections::{HashMap, VecDeque};

/// Everything the driver needs from a live koma connection: ext->koma `Call`
/// (blocking, replies as a `Value` — never `Err`, see module docs),
/// `panel_push` (write-only), and `notify` (write-only, fire-and-forget).
pub trait Host {
    fn call(&mut self, method: &str, params: Value) -> Value;
    fn panel_push(&mut self, panel_id: &str, payload: Value);
    fn notify(&mut self, name: &str, params: Value);
}

/// Production `Host`: a thin wrapper over a real (or demo) `Koma` handle.
pub struct KomaHost {
    koma: Koma,
}

impl KomaHost {
    pub fn new(koma: Koma) -> Self {
        KomaHost { koma }
    }
}

impl Host for KomaHost {
    fn call(&mut self, method: &str, params: Value) -> Value {
        self.koma.call(method, params)
    }

    fn panel_push(&mut self, panel_id: &str, payload: Value) {
        self.koma.panel_push(panel_id, payload);
    }

    fn notify(&mut self, name: &str, params: Value) {
        self.koma.notify(name, params);
    }
}

/// Scripted `Host` for driver/handler tests (W7+). Replies to [`Host::call`] are
/// consumed front-to-back per method name via [`FakeHost::script`]; a call with no
/// remaining scripted reply returns a canned `FakeHost: no script for '<method>'`
/// error so a missing script entry fails a test loudly instead of returning `null`
/// silently. Every call/push/notify is recorded so a test can assert exactly what
/// the code under test did without a live host.
#[derive(Default)]
pub struct FakeHost {
    scripts: HashMap<String, VecDeque<Value>>,
    pub calls: Vec<(String, Value)>,
    pub panel_pushes: Vec<(String, Value)>,
    pub notifies: Vec<(String, Value)>,
}

impl FakeHost {
    pub fn new() -> Self {
        FakeHost::default()
    }

    /// Queue one scripted reply for `method`. Multiple calls for the same script
    /// multiple replies in FIFO order (first `script` call = first reply consumed).
    pub fn script(&mut self, method: &str, reply: Value) -> &mut Self {
        self.scripts
            .entry(method.to_string())
            .or_default()
            .push_back(reply);
        self
    }
}

impl Host for FakeHost {
    fn call(&mut self, method: &str, params: Value) -> Value {
        self.calls.push((method.to_string(), params));
        match self.scripts.get_mut(method).and_then(VecDeque::pop_front) {
            Some(v) => v,
            None => serde_json::json!({ "error": format!("FakeHost: no script for '{method}'") }),
        }
    }

    fn panel_push(&mut self, panel_id: &str, payload: Value) {
        self.panel_pushes.push((panel_id.to_string(), payload));
    }

    fn notify(&mut self, name: &str, params: Value) {
        self.notifies.push((name.to_string(), params));
    }
}

/// `true` when `v` is a `Koma::call` error value carrying the `"grant denied:"`
/// prefix (broker.rs:416-419) — a fatal misconfiguration the driver surfaces in the
/// panel and stops dispatch for, never retries.
pub fn is_grant_denied(v: &Value) -> bool {
    error_str(v)
        .map(|s| s.starts_with("grant denied:"))
        .unwrap_or(false)
}

/// `true` when `v` is a `Koma::call` error value carrying the exact
/// `"koma call: timed out"` string the SDK's host-mode `Call` path returns when the
/// 120s bound (sdk.rs:560) elapses with no reply — a transient the driver retries
/// with backoff.
pub fn is_timeout(v: &Value) -> bool {
    error_str(v)
        .map(|s| s.starts_with("koma call: timed out"))
        .unwrap_or(false)
}

fn error_str(v: &Value) -> Option<&str> {
    v.get("error").and_then(|e| e.as_str())
}

#[cfg(test)]
#[path = "host_test.rs"]
mod host_test;
