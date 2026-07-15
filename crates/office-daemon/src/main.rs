//! office-daemon: the Workflow extension's shipped binary (`kind: "daemon"`,
//! manifest.json). This wave (BUILD_WAVES.md W6) wires the SDK glue only — the
//! `Extension` impl, the `Host` trait/`FakeHost`, and `on_invoke`/`on_event` routing
//! onto an `mpsc` channel. No kernel/store wiring yet (W7).
//!
//! # Threading model
//!
//! Same fleet-board-daemon pattern the SDK docs prescribe (sdk.rs's DEADLOCK RULE):
//! `on_invoke`/`on_event` run ON the host's single duplex-serve-loop thread and must
//! never call `Koma::call` (it would block that thread waiting on a reply only that
//! same thread could ever read off the socket). `DaemonDemo::driver` is a bare
//! `fn(&mut Koma)` — a function pointer, so it cannot capture anything from `main()`
//! — so both the channel's sending half and (once the driver thread has a live
//! handle) a write-only `Koma` clone are parked in `OnceLock`s:
//!
//! - `CMD_TX` / `CMD_RX`: `on_invoke`/`on_event` (via `handlers.rs`) push a parsed
//!   `Input` and return immediately; the driver thread (`driver_entry`, this wave a
//!   no-op drain — W7 replaces the body with the real kernel tick loop) owns the
//!   receiving half.
//! - `KOMA_WRITE`: a `try_clone`'d handle a future wave can use for handler-safe
//!   fire-and-forget calls (`panel_push`/`notify` only — never `call`, per the
//!   DEADLOCK RULE); populated as the first thing `driver_entry` does once it has a
//!   live `Koma`.

mod driver;
mod git;
mod handlers;
mod host;
mod inbox;

use driver::Driver;
use handlers::Input;
use host::KomaHost;
use koma_extension::{run_daemon, DaemonDemo, Extension, ExtensionManifest, Koma};
use office_store::Store;
use serde_json::Value;
use std::sync::mpsc;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

/// Sending half of the driver's `mpsc` channel; set once in `main()` before
/// `run_daemon` starts serving. `on_invoke`/`on_event` read this to enqueue.
static CMD_TX: OnceLock<mpsc::Sender<Input>> = OnceLock::new();

/// Receiving half, claimed once by `driver_entry`. Parked in a `Mutex` (not just a
/// bare `Receiver`) so the `OnceLock` stays `Sync`; there is only ever one reader.
static CMD_RX: OnceLock<Mutex<mpsc::Receiver<Input>>> = OnceLock::new();

/// A write-only `Koma` clone for future handler-safe `panel_push`/`notify` calls
/// (never `call` — DEADLOCK RULE). Populated by `driver_entry` as soon as it has a
/// live handle; unused by this wave's handlers, wired for W7+.
static KOMA_WRITE: OnceLock<Mutex<Koma>> = OnceLock::new();

struct Office;

impl Extension for Office {
    fn manifest(&self) -> ExtensionManifest {
        serde_json::from_str(include_str!("../../../manifest.json")).expect("manifest.json is valid")
    }

    /// koma->ext `Invoke` (contributes side: `tool.call` for the seven `workflow_*`
    /// tools, `panel.msg` for the board panel). Delegates straight to `handlers.rs`
    /// — no `Koma` handle is touched here (DEADLOCK RULE).
    fn on_invoke(&mut self, method: &str, params: Value) -> Value {
        let tx = CMD_TX
            .get()
            .expect("main() sets CMD_TX before run_daemon starts serving");
        handlers::on_invoke(method, params, tx)
    }

    /// koma->ext `Event` (contributes side: `subagent.done`/`agent.turn_end`/
    /// `session.foreground_change` from `contributes.events`, plus the private
    /// `agents.done` notify armed by `agents.spawn { notify: true }`). Fire-and-
    /// forget; no reply.
    fn on_event(&mut self, name: &str, params: Value) {
        if let Some(tx) = CMD_TX.get() {
            handlers::on_event(name, params, tx);
        }
    }
}

/// Runs on its own thread with a live `Koma` handle (host mode) or a demo stub
/// (demo mode) — see `koma_extension::sdk::run_daemon`. Host mode runs the full W7
/// tick loop (kernel + store + effect execution + reconciliation); demo mode has no
/// live socket, so it only drains whatever `main()`'s scripted invoke queued and
/// returns (blocking forever there would hang `cargo run`).
fn driver_entry(koma: &mut Koma) {
    let _ = KOMA_WRITE.set(Mutex::new(koma.try_clone()));
    let _ = driver::CACHE.set(Mutex::new(driver::SnapshotCache::default()));

    if std::env::var_os("KOMA_EXT_SOCKET").is_none() {
        // Demo mode: bounded drain, no store side effects, exit cleanly.
        if let Some(rx) = CMD_RX.get() {
            if let Ok(rx) = rx.lock() {
                for _input in rx.try_iter() {}
            }
        }
        return;
    }

    run_host_driver(koma);
}

/// Host-mode driver: open the durable store, spawn the dedicated lease-heartbeat thread
/// (own `try_clone`'d clock, no host calls — 4.4/5.1), bootstrap (bind session, lease,
/// reconcile), then loop on `recv_timeout(1s)` feeding `Tick`/inputs into the kernel.
fn run_host_driver(koma: &mut Koma) {
    let store = match Store::open_default() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("workflow-driver: cannot open state root: {e}");
            return;
        }
    };
    let instance = driver::mint_instance();
    let pid = std::process::id();

    // Dedicated heartbeat thread: refreshes our leases every 10s so a slow host call on
    // the driver can never age a lease past the 60s steal window (5.6).
    {
        let hb_store = store.clone();
        let hb_instance = instance.clone();
        std::thread::spawn(move || loop {
            std::thread::sleep(Duration::from_secs(10));
            driver::heartbeat_owned(&hb_store, &hb_instance, driver::now_ms());
        });
    }

    let host = KomaHost::new(koma.try_clone());
    let mut d = match Driver::load(store, host, instance, pid) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("workflow-driver: load failed: {e}");
            return;
        }
    };

    // Install the off-loop invoke pool (5.1): worker threads own `try_clone`'d handles and
    // post results back onto CMD_TX as `InvokeDone`, so a multi-minute PRD flow never freezes the
    // tick loop, reconcile, the heartbeat, or the 100ms panel-read oneshots.
    if let Some(tx) = CMD_TX.get() {
        d.set_invoker(Box::new(driver::ThreadInvoker::new(koma.try_clone(), tx.clone())));
    }

    d.bootstrap(driver::now_ms());

    let rx = CMD_RX
        .get()
        .expect("main() sets CMD_RX before run_daemon starts serving")
        .lock()
        .expect("cmd channel mutex poisoned");

    loop {
        match rx.recv_timeout(Duration::from_secs(1)) {
            Ok(input) => d.handle(input, driver::now_ms()),
            Err(mpsc::RecvTimeoutError::Timeout) => d.on_tick(driver::now_ms()),
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
}

fn main() {
    let (cmd_tx, cmd_rx) = mpsc::channel();
    CMD_TX
        .set(cmd_tx)
        .unwrap_or_else(|_| unreachable!("CMD_TX is only ever set here"));
    CMD_RX
        .set(Mutex::new(cmd_rx))
        .unwrap_or_else(|_| unreachable!("CMD_RX is only ever set here"));

    run_daemon(
        Office,
        DaemonDemo {
            // Simulates the model calling the `workflow_projects` contributed tool —
            // koma relays it as a "tool.call" invoke with exactly this shape (see
            // echo-tool-daemon / event-watcher-daemon for the reference pattern).
            invoke: Some((
                "tool.call".to_string(),
                serde_json::json!({ "name": "workflow_projects", "args": {} }),
            )),
            driver: Some(driver_entry),
        },
    );
}
