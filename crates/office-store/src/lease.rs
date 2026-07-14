//! Cross-instance dispatch lease (ARCHITECTURE.md 4.4).
//!
//! Every session daemon runs its own office-daemon instance against the same state root,
//! so exactly one instance may dispatch for a project at a time. Ownership is a `lease.json`
//! file per project:
//!
//! ```json
//! { "schema": "workflow/1", "instance": "<uuid4>", "session": "<uuid|null>",
//!   "pid": 1234, "heartbeat_ms": 1720000000000 }
//! ```
//!
//! - Acquire when: no lease, OR the lease is stale (heartbeat older than 60s), OR the lease
//!   belongs to our bound session (rebind after a koma restart minted a new instance uuid).
//! - Otherwise the project is READ-ONLY for us (the panel still shows it; comments still go
//!   through `Store::with_state_lock`).
//! - Every read-modify-write of the lease holds an advisory `flock` on `lease.json.lock`, so
//!   two racing daemons cannot both decide the lease is free.

use crate::store::{atomic_write, SCHEMA};
use serde::{Deserialize, Serialize};
use std::fs::{self, File};
use std::io;
use std::path::Path;

/// A lease older than this (wall-clock, ms) is stale and may be stolen.
pub const STALE_MS: u64 = 60_000;
/// Recommended heartbeat cadence for the holder (ms). Well under `STALE_MS`.
pub const HEARTBEAT_MS: u64 = 10_000;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Lease {
    pub schema: String,
    pub instance: String,
    pub session: Option<String>,
    pub pid: u32,
    pub heartbeat_ms: u64,
}

impl Lease {
    fn new(instance: &str, session: Option<&str>, pid: u32, now_ms: u64) -> Lease {
        Lease {
            schema: SCHEMA.to_string(),
            instance: instance.to_string(),
            session: session.map(|s| s.to_string()),
            pid,
            heartbeat_ms: now_ms,
        }
    }
}

/// Whether a lease is stale as of `now_ms`.
pub fn is_stale(lease: &Lease, now_ms: u64) -> bool {
    now_ms.saturating_sub(lease.heartbeat_ms) > STALE_MS
}

/// Read the current lease, returning `None` if absent or corrupt.
pub fn read(path: &Path) -> io::Result<Option<Lease>> {
    match fs::read(path) {
        Ok(bytes) => Ok(serde_json::from_slice::<Lease>(&bytes).ok()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// Attempt to acquire the lease. Returns `Some(lease)` if we now hold it, or `None` if a
/// live, foreign instance holds it (read-only mode). Serialized by `flock`.
pub fn acquire(
    path: &Path,
    instance: &str,
    session: Option<&str>,
    pid: u32,
    now_ms: u64,
) -> io::Result<Option<Lease>> {
    with_lock(path, || {
        let existing = read(path)?;
        let can_take = match &existing {
            None => true,
            Some(l) => {
                l.instance == instance                                       // already ours
                    || is_stale(l, now_ms)                                   // stale steal
                    || (l.session.is_some() && l.session.as_deref() == session) // same-session rebind
            }
        };
        if !can_take {
            return Ok(None);
        }
        let lease = Lease::new(instance, session, pid, now_ms);
        write_lease(path, &lease)?;
        Ok(Some(lease))
    })
}

/// Refresh the heartbeat. Returns the updated lease. Errors with `PermissionDenied` if a
/// different live instance has taken the lease out from under us.
pub fn heartbeat(path: &Path, lease: &Lease, now_ms: u64) -> io::Result<Lease> {
    with_lock(path, || {
        if let Some(l) = read(path)? {
            if l.instance != lease.instance && !is_stale(&l, now_ms) {
                return Err(io::Error::new(io::ErrorKind::PermissionDenied, "lease taken by another instance"));
            }
        }
        let mut updated = lease.clone();
        updated.heartbeat_ms = now_ms;
        write_lease(path, &updated)?;
        Ok(updated)
    })
}

/// Release the lease if (and only if) we still hold it. No-op otherwise.
pub fn release(path: &Path, instance: &str) -> io::Result<()> {
    with_lock(path, || {
        if let Some(l) = read(path)? {
            if l.instance == instance {
                match fs::remove_file(path) {
                    Ok(()) => {}
                    Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                    Err(e) => return Err(e),
                }
            }
        }
        Ok(())
    })
}

fn write_lease(path: &Path, lease: &Lease) -> io::Result<()> {
    let bytes = serde_json::to_vec_pretty(lease)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
    atomic_write(path, &bytes)
}

/// Hold an advisory `flock` on `<lease-dir>/lease.json.lock` for the duration of `f`.
fn with_lock<F, R>(lease_path: &Path, f: F) -> io::Result<R>
where
    F: FnOnce() -> io::Result<R>,
{
    let parent = lease_path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "lease path has no parent"))?;
    fs::create_dir_all(parent)?;
    let lock = File::create(parent.join("lease.json.lock"))?;
    lock.lock()?; // exclusive advisory flock; released on drop
    f()
}
