//! Worker persona pool + deterministic assignment (ARCHITECTURE.md 5.2 / manifest
//! `contributes.sub_agents`).
//!
//! The office fields a POOL of 10 named worker sub-agents (`office-worker-<name>`),
//! all sharing a byte-identical work-protocol prompt CORE plus a personality flavor.
//! Which persona a task is worked by is a *stable* function of the task id — a
//! dependency-free FNV-1a hash mod 10 — so a respawn or a review bounce of the SAME
//! task always draws the SAME persona (no reshuffle mid-project), and the choice is
//! reconstructible after a store reload without persisting any assignment table.

/// The 10 worker personas, in stable assignment order. The FNV-1a index maps into
/// this; the UI office view mirrors the same order (ui/src/lib/officeLayout.ts).
pub const WORKER_PERSONAS: [&str; 10] = [
    "nova", "mika", "tetsuo", "bob", "yuki", "dax", "ines", "koji", "vera", "pip",
];

/// The `office-worker-` prefix every worker sub-agent id (and worker binding persona)
/// carries. The office view strips it to the short persona name.
pub const WORKER_PREFIX: &str = "office-worker-";

/// FNV-1a (64-bit) over the task id bytes. Deterministic and dependency-free, so the
/// same task id always yields the same hash across processes, respawns, and reloads.
fn fnv1a_64(s: &str) -> u64 {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut h = OFFSET;
    for b in s.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(PRIME);
    }
    h
}

/// The short persona name (e.g. `nova`) a task id is assigned to.
pub fn worker_persona(task_id: &str) -> &'static str {
    WORKER_PERSONAS[(fnv1a_64(task_id) % WORKER_PERSONAS.len() as u64) as usize]
}

/// The full worker sub-agent id (e.g. `office-worker-nova`) for a task id. Stamped on
/// the worker `AgentBinding.persona` and carried as the `Effect::Spawn.agent`.
pub fn worker_agent_id(task_id: &str) -> String {
    format!("{}{}", WORKER_PREFIX, worker_persona(task_id))
}

/// Strip the `office-worker-` prefix from a binding persona to its short name; returns
/// `None` for a non-worker persona (e.g. `office-reviewer`) or an empty string.
pub fn short_worker_name(persona: &str) -> Option<&str> {
    persona.strip_prefix(WORKER_PREFIX)
}
