//! Blocked-by DAG logic: acyclicity validation, ready-set computation, and
//! halt (line-stuck) detection. All pure over `&[Task]` / `&Project`.

use crate::domain::{Project, Task, TaskId, TaskState};
use std::collections::HashMap;

/// A cycle was found in the blocked-by graph. `nodes` holds the task ids that
/// remain unresolved after Kahn's algorithm (i.e. the tasks participating in, or
/// downstream of, the cycle), sorted for determinism.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cycle {
    pub nodes: Vec<TaskId>,
}

/// Why the production line is stuck. `parked_blockers` are the Parked tasks that
/// every unfinished task is transitively blocked by, sorted for determinism.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StuckReason {
    pub parked_blockers: Vec<TaskId>,
}

fn is_done(state: &TaskState) -> bool {
    matches!(state, TaskState::Done { .. })
}

fn is_parked(state: &TaskState) -> bool {
    matches!(state, TaskState::Parked { .. })
}

/// A task counts as "running" (the line is actively working it) when a worker or
/// reviewer is or will be attached: OnProgress or Review. Review with no binding
/// yet still counts — the reviewer is spawned on the next tick.
fn is_running(state: &TaskState) -> bool {
    matches!(
        state,
        TaskState::OnProgress { .. } | TaskState::Review { .. }
    )
}

/// Validate that the blocked-by graph is acyclic (Kahn's algorithm). Edges only
/// count when the referenced blocker is present in `tasks`; dangling references
/// are ignored for cycle purposes.
pub fn validate_acyclic(tasks: &[Task]) -> Result<(), Cycle> {
    let n = tasks.len();
    let index: HashMap<&TaskId, usize> = tasks.iter().enumerate().map(|(i, t)| (&t.id, i)).collect();

    // in_degree[i] = number of present prerequisites of task i.
    // dependents[j] = tasks that depend on task j.
    let mut in_degree = vec![0usize; n];
    let mut dependents: Vec<Vec<usize>> = vec![Vec::new(); n];

    for (i, task) in tasks.iter().enumerate() {
        for dep in &task.blocked_by {
            if let Some(&j) = index.get(dep) {
                in_degree[i] += 1;
                dependents[j].push(i);
            }
        }
    }

    let mut queue: Vec<usize> = (0..n).filter(|&i| in_degree[i] == 0).collect();
    let mut resolved = 0usize;

    while let Some(i) = queue.pop() {
        resolved += 1;
        for &dependent in &dependents[i] {
            in_degree[dependent] -= 1;
            if in_degree[dependent] == 0 {
                queue.push(dependent);
            }
        }
    }

    if resolved == n {
        Ok(())
    } else {
        let mut nodes: Vec<TaskId> = (0..n)
            .filter(|&i| in_degree[i] > 0)
            .map(|i| tasks[i].id.clone())
            .collect();
        nodes.sort();
        Err(Cycle { nodes })
    }
}

/// Ready set: `Todo` tasks whose every `blocked_by` prerequisite is `Done`,
/// sorted `(priority desc, id asc)` — fully deterministic. A blocker missing from
/// `tasks` is treated as NOT done (the task cannot be ready on broken data).
pub fn ready_set(tasks: &[Task]) -> Vec<TaskId> {
    let by_id: HashMap<&TaskId, &Task> = tasks.iter().map(|t| (&t.id, t)).collect();

    let mut ready: Vec<&Task> = tasks
        .iter()
        .filter(|t| matches!(t.state, TaskState::Todo))
        .filter(|t| {
            t.blocked_by
                .iter()
                .all(|dep| by_id.get(dep).map(|b| is_done(&b.state)).unwrap_or(false))
        })
        .collect();

    ready.sort_by(|a, b| {
        b.priority
            .cmp(&a.priority) // priority desc
            .then_with(|| a.id.cmp(&b.id)) // id asc
    });

    ready.into_iter().map(|t| t.id.clone()).collect()
}

/// Detect whether the production line has stuck itself: unfinished tasks exist,
/// zero running agents, zero ready tasks, and every unfinished task is
/// transitively blocked by a Parked task. Returns the parked culprits, or `None`
/// if the line can still move.
pub fn line_is_stuck(project: &Project) -> Option<StuckReason> {
    let tasks = &project.tasks;

    let unfinished: Vec<&Task> = tasks.iter().filter(|t| !is_done(&t.state)).collect();
    if unfinished.is_empty() {
        return None;
    }

    // Any active agent means the line is still moving.
    if tasks.iter().any(|t| is_running(&t.state)) {
        return None;
    }

    // Any ready task means the line can dispatch.
    if !ready_set(tasks).is_empty() {
        return None;
    }

    // A task is "poisoned" if it is Parked or (transitively) depends on a poisoned
    // task. Compute via memoized DFS, cycle-guarded (graph is validated acyclic
    // elsewhere, but never loop on bad data).
    let by_id: HashMap<&TaskId, &Task> = tasks.iter().map(|t| (&t.id, t)).collect();
    let mut poisoned: HashMap<&TaskId, bool> = HashMap::new();
    let mut visiting: std::collections::HashSet<&TaskId> = std::collections::HashSet::new();

    fn poison<'a>(
        id: &'a TaskId,
        by_id: &HashMap<&'a TaskId, &'a Task>,
        poisoned: &mut HashMap<&'a TaskId, bool>,
        visiting: &mut std::collections::HashSet<&'a TaskId>,
    ) -> bool {
        if let Some(&p) = poisoned.get(id) {
            return p;
        }
        let task = match by_id.get(id) {
            Some(t) => *t,
            None => return false, // dangling reference: not a known parked blocker
        };
        if !visiting.insert(id) {
            // cycle: treat as not-poisoned on the back-edge to terminate
            return false;
        }
        let result = if is_parked(&task.state) {
            true
        } else {
            task.blocked_by
                .iter()
                .any(|dep| poison(dep, by_id, poisoned, visiting))
        };
        visiting.remove(id);
        poisoned.insert(id, result);
        result
    }

    let all_poisoned = unfinished
        .iter()
        .all(|t| poison(&t.id, &by_id, &mut poisoned, &mut visiting));

    if !all_poisoned {
        return None;
    }

    let mut parked_blockers: Vec<TaskId> = tasks
        .iter()
        .filter(|t| is_parked(&t.state))
        .map(|t| t.id.clone())
        .collect();
    parked_blockers.sort();

    Some(StuckReason { parked_blockers })
}
