//! Persona pool tests (persona.rs): deterministic id-hashed worker assignment.

use crate::persona::*;

#[test]
fn worker_persona_is_deterministic() {
    // The SAME task id always yields the SAME persona — this is what makes a respawn or a
    // review bounce of one task reuse its worker instead of reshuffling mid-project.
    for id in ["notif/t7", "a", "some/long/hierarchical/task-slug", ""] {
        let first = worker_persona(id);
        for _ in 0..50 {
            assert_eq!(worker_persona(id), first, "persona must be stable for {id:?}");
        }
        // The full spawn id is exactly the prefix + the short name.
        assert_eq!(worker_agent_id(id), format!("office-worker-{first}"));
    }
}

#[test]
fn worker_persona_is_always_in_the_pool() {
    for i in 0..500 {
        let id = format!("proj/epic/story/task-{i}");
        assert!(
            WORKER_PERSONAS.contains(&worker_persona(&id)),
            "unknown persona for {id}"
        );
    }
}

#[test]
fn worker_persona_reaches_all_ten() {
    // Distribution: a modest spread of distinct ids must exercise every one of the 10 personas,
    // so no persona is unreachable. This is deterministic (FNV-1a), not probabilistic.
    use std::collections::HashSet;
    let seen: HashSet<&str> = (0..200).map(|i| worker_persona(&format!("task-{i}"))).collect();
    assert_eq!(seen.len(), WORKER_PERSONAS.len(), "every persona must be reachable");
}

#[test]
fn short_worker_name_strips_only_worker_personas() {
    assert_eq!(short_worker_name("office-worker-tetsuo"), Some("tetsuo"));
    assert_eq!(short_worker_name(&worker_agent_id("x")), Some(worker_persona("x")));
    // Non-worker personas (reviewer / empty) are not worker ids.
    assert_eq!(short_worker_name("office-reviewer"), None);
    assert_eq!(short_worker_name(""), None);
}
