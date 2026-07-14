//! Durable, atomic, versioned project store (ARCHITECTURE.md 4).
//!
//! Layout under the state root (`${WORKFLOW_HOME:-~/.koma-workflow}`):
//!
//! ```text
//! <root>/
//!   README.md
//!   DO-NOT-ENTER.md
//!   registry.json                    { schema, projects: [ { project_id, name, phase, state_dir } ] }
//!   projects/
//!     <slug>/
//!       state.json                   { schema, project }
//!       prd.md
//!       journal.ndjson
//!       lease.json                   (see lease.rs)
//! ```
//!
//! Guarantees:
//! - Every `state.json`/`registry.json` write is `tmp -> fsync -> rename -> dir fsync`.
//! - Cross-file order: create writes `state.json` BEFORE the registry row; archive removes
//!   the registry row BEFORE deleting the state dir. A crash mid-pair leaves at most a state
//!   dir with no registry row, which `load_all` adopts (never a dangling registry row).
//! - `load_all` is self-healing: adopt orphan states, drop+quarantine corrupt/missing ones.
//! - Every file carries `"schema": "workflow/1"`; loaders refuse a newer major and run an
//!   ordered migration table (empty for v1).

use office_core::{Project, ProjectPhase};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashSet;
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

/// Schema string stamped into every file this crate writes.
pub const SCHEMA: &str = "workflow/1";
/// Current schema major. Anything with a higher major is refused on load.
pub const SCHEMA_MAJOR: u32 = 1;

const README: &str = "# Workflow state root\n\n\
This directory is the durable state for the Workflow koma extension (id aula.workflow).\n\
It lives OUTSIDE ~/.koma/extensions so that reinstalling or upgrading the extension never\n\
deletes your project boards.\n\n\
Layout:\n\
  registry.json          index of known projects\n\
  projects/<slug>/\n\
    state.json           full project state (schema workflow/1)\n\
    prd.md               human-readable PRD mirror\n\
    journal.ndjson       append-only audit log\n\
    lease.json           dispatch ownership lease\n\n\
Managed automatically. See DO-NOT-ENTER.md before editing by hand.\n";

const DO_NOT_ENTER: &str = "# Do not edit by hand\n\n\
This is the Workflow extension's working state (aula.workflow). The daemon reads and\n\
rewrites these files atomically. Hand-editing while koma is running can corrupt a board\n\
or lose work. To remove everything, delete this whole directory while koma is stopped.\n";

// ---------------------------------------------------------------------------
// Default root
// ---------------------------------------------------------------------------

/// Default state root: `$WORKFLOW_HOME` if set and non-empty, else `~/.koma-workflow`.
pub fn root() -> PathBuf {
    if let Ok(v) = std::env::var("WORKFLOW_HOME") {
        if !v.is_empty() {
            return PathBuf::from(v);
        }
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    PathBuf::from(home).join(".koma-workflow")
}

// ---------------------------------------------------------------------------
// On-disk envelopes
// ---------------------------------------------------------------------------

#[derive(Serialize, Deserialize)]
struct StateFile {
    schema: String,
    project: Project,
}

/// One row of `registry.json`.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct RegistryRow {
    pub project_id: String,
    pub name: String,
    pub phase: String,
    /// Relative path from the root, e.g. `projects/<slug>`.
    pub state_dir: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct RegistryFile {
    schema: String,
    #[serde(default)]
    projects: Vec<RegistryRow>,
}

impl RegistryFile {
    fn empty() -> Self {
        RegistryFile {
            schema: SCHEMA.to_string(),
            projects: Vec::new(),
        }
    }
}

/// Outcome of a self-healing `load_all`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LoadResult {
    /// Projects that loaded cleanly (adopted or already registered).
    pub projects: Vec<Project>,
    /// Slugs of valid `state.json`s that had no registry row and were adopted.
    pub adopted: Vec<String>,
    /// Slugs whose `state.json` was corrupt/unreadable and got moved to `.quarantine`.
    pub quarantined: Vec<String>,
    /// Registry slugs whose state dir was missing entirely; the row was dropped.
    pub dropped: Vec<String>,
}

// ---------------------------------------------------------------------------
// Store
// ---------------------------------------------------------------------------

/// A handle to a Workflow state root. Cheap to clone-by-path; all methods take `&self`
/// and are safe to share across threads (mutations serialize through an advisory flock).
#[derive(Clone, Debug)]
pub struct Store {
    root: PathBuf,
}

impl Store {
    /// Open (creating if absent) the state root, writing `README.md`/`DO-NOT-ENTER.md`
    /// and an empty `registry.json` on first init. Existing files are never clobbered.
    pub fn open(root: impl Into<PathBuf>) -> io::Result<Store> {
        let root = root.into();
        fs::create_dir_all(root.join("projects"))?;
        let readme = root.join("README.md");
        if !readme.exists() {
            fs::write(&readme, README)?;
        }
        let dne = root.join("DO-NOT-ENTER.md");
        if !dne.exists() {
            fs::write(&dne, DO_NOT_ENTER)?;
        }
        let store = Store { root };
        if !store.registry_path().exists() {
            store.write_registry_file(&RegistryFile::empty())?;
        }
        Ok(store)
    }

    /// Open the default root (`root()`).
    pub fn open_default() -> io::Result<Store> {
        Store::open(root())
    }

    pub fn root_dir(&self) -> &Path {
        &self.root
    }

    // -- paths ---------------------------------------------------------------

    fn projects_dir(&self) -> PathBuf {
        self.root.join("projects")
    }

    pub fn state_dir(&self, slug: &str) -> PathBuf {
        self.projects_dir().join(slug)
    }

    pub fn state_path(&self, slug: &str) -> PathBuf {
        self.state_dir(slug).join("state.json")
    }

    fn registry_path(&self) -> PathBuf {
        self.root.join("registry.json")
    }

    pub fn prd_path(&self, slug: &str) -> PathBuf {
        self.state_dir(slug).join("prd.md")
    }

    pub fn journal_path(&self, slug: &str) -> PathBuf {
        self.state_dir(slug).join("journal.ndjson")
    }

    /// Path to a project's `lease.json` (see lease.rs).
    pub fn lease_path(&self, slug: &str) -> PathBuf {
        self.state_dir(slug).join("lease.json")
    }

    /// Path to a project's `state.json.lock` — the advisory flock every state.json writer
    /// (holder persists AND non-holder comment adds) contends on (4.4).
    fn lock_path(&self, slug: &str) -> PathBuf {
        self.state_dir(slug).join("state.json.lock")
    }

    // -- state.json ----------------------------------------------------------

    /// Atomically write ONLY `state.json` for a project. Does not touch the registry.
    /// Exposed so the driver (and tests) can model the create/archive ordering precisely.
    ///
    /// Takes the SAME advisory `flock` on `state.json.lock` that `with_state_lock` uses, so
    /// the lease holder's persists contend with a non-holder's flock'd comment adds (4.4).
    /// Without this the lock is one-sided and enforces nothing: a holder rename could land
    /// inside a non-holder's locked window and silently clobber a committed update.
    pub fn put_state(&self, p: &Project) -> io::Result<()> {
        let slug = p.id.0.as_str();
        fs::create_dir_all(self.state_dir(slug))?;
        let lock = File::create(self.lock_path(slug))?;
        lock.lock()?; // symmetric advisory flock; released when `lock` drops (end of fn)
        self.put_state_inner(p)
    }

    /// Serialize + atomically write `state.json` WITHOUT taking the advisory flock. Callers
    /// that ALREADY hold `state.json.lock` (`with_state_lock`) use this to avoid re-locking
    /// the same path from the same process, which would self-deadlock.
    fn put_state_inner(&self, p: &Project) -> io::Result<()> {
        let slug = p.id.0.as_str();
        fs::create_dir_all(self.state_dir(slug))?;
        let sf = StateFile {
            schema: SCHEMA.to_string(),
            project: p.clone(),
        };
        let bytes = serde_json::to_vec_pretty(&sf).map_err(to_io)?;
        atomic_write(&self.state_path(slug), &bytes)
    }

    /// Load and schema-check a single project's `state.json`, running migrations.
    pub fn load_project(&self, slug: &str) -> io::Result<Project> {
        let bytes = fs::read(self.state_path(slug))?;
        parse_state(&bytes)
    }

    // -- registry ------------------------------------------------------------

    fn read_registry_file(&self) -> io::Result<RegistryFile> {
        match fs::read(self.registry_path()) {
            // A corrupt registry is treated as empty: state dirs are the source of truth
            // and `load_all` rebuilds the index from them (self-healing).
            Ok(bytes) => Ok(serde_json::from_slice::<RegistryFile>(&bytes).unwrap_or_else(|_| RegistryFile::empty())),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(RegistryFile::empty()),
            Err(e) => Err(e),
        }
    }

    fn write_registry_file(&self, rf: &RegistryFile) -> io::Result<()> {
        let bytes = serde_json::to_vec_pretty(rf).map_err(to_io)?;
        atomic_write(&self.registry_path(), &bytes)
    }

    /// The current registry rows.
    pub fn registry(&self) -> io::Result<Vec<RegistryRow>> {
        Ok(self.read_registry_file()?.projects)
    }

    /// Insert or update the registry row for a project.
    pub fn upsert_registry_row(&self, p: &Project) -> io::Result<()> {
        let mut rf = self.read_registry_file()?;
        let row = row_for(p);
        if let Some(existing) = rf.projects.iter_mut().find(|r| r.project_id == row.project_id) {
            *existing = row;
        } else {
            rf.projects.push(row);
        }
        self.write_registry_file(&rf)
    }

    /// Remove a project's registry row (no-op if absent).
    pub fn remove_registry_row(&self, slug: &str) -> io::Result<()> {
        let mut rf = self.read_registry_file()?;
        rf.projects.retain(|r| r.project_id != slug);
        self.write_registry_file(&rf)
    }

    // -- create / save / archive --------------------------------------------

    /// Persist a project: write `state.json` FIRST, then upsert the registry row.
    /// State-first ordering means the registry never points at a project whose state
    /// did not land (ARCHITECTURE.md 4.2). Covers both create and update.
    pub fn save_project(&self, p: &Project) -> io::Result<()> {
        self.put_state(p)?;
        self.upsert_registry_row(p)?;
        Ok(())
    }

    /// Alias for `save_project` at creation time (same state-first ordering).
    pub fn create_project(&self, p: &Project) -> io::Result<()> {
        self.save_project(p)
    }

    /// Archive a project: remove the registry row FIRST, then delete the state dir.
    /// A crash between the two leaves an orphan state dir, which `load_all` re-adopts.
    pub fn archive_project(&self, slug: &str) -> io::Result<()> {
        self.remove_registry_row(slug)?;
        let dir = self.state_dir(slug);
        if dir.exists() {
            fs::remove_dir_all(&dir)?;
        }
        Ok(())
    }

    // -- self-healing bulk load ---------------------------------------------

    /// Load every project, reconciling the registry against what is on disk:
    /// - a valid `state.json` with no registry row is ADOPTED (row rebuilt),
    /// - a registry row whose `state.json` is corrupt is DROPPED and the dir QUARANTINED,
    /// - a registry row whose state dir is missing entirely is DROPPED.
    /// The healed registry is written back before returning.
    pub fn load_all(&self) -> io::Result<LoadResult> {
        let reg = self.read_registry_file()?;
        let reg_slugs: HashSet<String> = reg.projects.iter().map(|r| r.project_id.clone()).collect();

        let mut result = LoadResult::default();
        let mut seen_on_disk: HashSet<String> = HashSet::new();

        let pdir = self.projects_dir();
        if pdir.exists() {
            for entry in fs::read_dir(&pdir)? {
                let entry = entry?;
                if !entry.file_type()?.is_dir() {
                    continue;
                }
                let name = entry.file_name().to_string_lossy().to_string();
                if name == ".quarantine" {
                    continue;
                }
                let state_path = entry.path().join("state.json");
                if !state_path.exists() {
                    // A project dir with no state.json (e.g. only a stray lease/journal) is
                    // not a project; leave it alone.
                    continue;
                }
                seen_on_disk.insert(name.clone());
                match fs::read(&state_path).ok().and_then(|b| parse_state(&b).ok()) {
                    Some(project) => {
                        let slug = project.id.0.clone();
                        if !reg_slugs.contains(&slug) {
                            result.adopted.push(slug);
                        }
                        result.projects.push(project);
                    }
                    None => {
                        // Corrupt / unreadable / newer-schema state: quarantine the dir.
                        self.quarantine(&name)?;
                        result.quarantined.push(name);
                    }
                }
            }
        }

        // Registry rows whose state dir never showed up on disk are stale -> dropped.
        for row in &reg.projects {
            let slug = &row.project_id;
            if !seen_on_disk.contains(slug) {
                result.dropped.push(slug.clone());
            }
        }

        // Write back the healed registry, rebuilt from the projects that survived.
        let mut healed = RegistryFile::empty();
        healed.projects = result.projects.iter().map(row_for).collect();
        self.write_registry_file(&healed)?;

        Ok(result)
    }

    fn quarantine(&self, name: &str) -> io::Result<()> {
        let qdir = self.projects_dir().join(".quarantine");
        fs::create_dir_all(&qdir)?;
        let src = self.projects_dir().join(name);
        let mut dst = qdir.join(name);
        let mut n = 1u32;
        while dst.exists() {
            dst = qdir.join(format!("{}-{}", name, n));
            n += 1;
        }
        fs::rename(&src, &dst)
    }

    // -- prd mirror ----------------------------------------------------------

    pub fn write_prd(&self, slug: &str, markdown: &str) -> io::Result<()> {
        fs::create_dir_all(self.state_dir(slug))?;
        atomic_write(&self.prd_path(slug), markdown.as_bytes())
    }

    pub fn read_prd(&self, slug: &str) -> io::Result<Option<String>> {
        match fs::read_to_string(self.prd_path(slug)) {
            Ok(s) => Ok(Some(s)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    // -- journal -------------------------------------------------------------

    /// Append one JSON event as a line to `journal.ndjson`.
    pub fn append_journal(&self, slug: &str, event: &Value) -> io::Result<()> {
        fs::create_dir_all(self.state_dir(slug))?;
        let mut f = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.journal_path(slug))?;
        let mut line = serde_json::to_vec(event).map_err(to_io)?;
        line.push(b'\n');
        f.write_all(&line)?;
        f.flush()
    }

    /// Read the journal, skipping any malformed line (torn tail tolerance).
    pub fn read_journal(&self, slug: &str) -> io::Result<Vec<Value>> {
        let data = match fs::read_to_string(self.journal_path(slug)) {
            Ok(d) => d,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e),
        };
        let mut out = Vec::new();
        for line in data.lines() {
            if line.trim().is_empty() {
                continue;
            }
            if let Ok(v) = serde_json::from_str::<Value>(line) {
                out.push(v);
            }
            // A malformed line (e.g. a torn final write) is skipped.
        }
        Ok(out)
    }

    // -- flock'd load-mutate-store ------------------------------------------

    /// Run `f` against a freshly-loaded project and persist the result, holding an
    /// advisory `flock` on `state.json.lock` for the whole load-mutate-store window.
    /// This is the ONLY safe way for a non-lease-holder to mutate state (comment adds),
    /// and it serializes concurrent writers so they cannot lose updates or torn-write.
    pub fn with_state_lock<F, R>(&self, slug: &str, f: F) -> io::Result<R>
    where
        F: FnOnce(&mut Project) -> R,
    {
        fs::create_dir_all(self.state_dir(slug))?;
        let lock = File::create(self.lock_path(slug))?;
        lock.lock()?; // exclusive advisory flock; released when `lock` drops
        let bytes = fs::read(self.state_path(slug))?;
        let mut project = parse_state(&bytes)?;
        let r = f(&mut project);
        // Already holding the flock — use the un-locked writer to avoid a self-deadlock.
        self.put_state_inner(&project)?;
        Ok(r)
    }
}

// ---------------------------------------------------------------------------
// Free helpers
// ---------------------------------------------------------------------------

fn row_for(p: &Project) -> RegistryRow {
    RegistryRow {
        project_id: p.id.0.clone(),
        name: p.name.clone(),
        phase: phase_label(&p.phase),
        state_dir: format!("projects/{}", p.id.0),
    }
}

fn phase_label(phase: &ProjectPhase) -> String {
    match phase {
        ProjectPhase::Drafting => "drafting",
        ProjectPhase::Ready => "ready",
        ProjectPhase::Running => "running",
        ProjectPhase::Interrupted => "interrupted",
        ProjectPhase::Halted { .. } => "halted",
        ProjectPhase::Done { .. } => "done",
    }
    .to_string()
}

fn to_io<E: std::fmt::Display>(e: E) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, e.to_string())
}

/// `tmp -> fsync -> rename -> dir fsync` atomic file replace. A crash at any point leaves
/// the previous file intact (the rename is the only mutation of the live path).
pub(crate) fn atomic_write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "path has no parent"))?;
    fs::create_dir_all(parent)?;
    let fname = path.file_name().and_then(|n| n.to_str()).unwrap_or("out");
    let tmp = parent.join(format!(".{}.tmp.{}", fname, std::process::id()));
    {
        let mut f = File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    fs::rename(&tmp, path)?;
    // Best-effort dir fsync so a power loss (not just a process crash) keeps the rename.
    if let Ok(dir) = File::open(parent) {
        let _ = dir.sync_all();
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Schema parse + migration engine
// ---------------------------------------------------------------------------

/// A single schema migration: transforms a raw JSON value from major N to major N+1.
pub type Migration = fn(Value) -> Value;

/// Ordered migration table. Index i migrates major (i+1) -> (i+2). Empty for v1.
pub const MIGRATIONS: &[Migration] = &[];

/// Parse a `state.json` byte buffer into a `Project`, enforcing the schema contract:
/// refuse a newer major, run the migration chain for an older one.
fn parse_state(bytes: &[u8]) -> io::Result<Project> {
    let v: Value = serde_json::from_slice(bytes).map_err(to_io)?;
    let schema = v
        .get("schema")
        .and_then(|s| s.as_str())
        .ok_or_else(|| to_io("state.json missing schema"))?;
    let major = parse_major(schema)?;
    if major > SCHEMA_MAJOR {
        return Err(to_io(format!(
            "refusing state with schema major {} (this build supports up to {})",
            major, SCHEMA_MAJOR
        )));
    }
    let migrated = apply_migrations(v, major, SCHEMA_MAJOR, MIGRATIONS)?;
    let sf: StateFile = serde_json::from_value(migrated).map_err(to_io)?;
    Ok(sf.project)
}

/// Extract the leading numeric major from a `"workflow/<major>"` schema string.
fn parse_major(schema: &str) -> io::Result<u32> {
    let rest = schema
        .strip_prefix("workflow/")
        .ok_or_else(|| to_io(format!("unrecognized schema: {}", schema)))?;
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    digits
        .parse::<u32>()
        .map_err(|_| to_io(format!("unrecognized schema major: {}", schema)))
}

/// Run migrations `from_major -> to_major` in order using `table`.
/// A no-op when `from_major == to_major`. Errors if a required step is missing.
pub fn apply_migrations(
    mut v: Value,
    from_major: u32,
    to_major: u32,
    table: &[Migration],
) -> io::Result<Value> {
    let mut m = from_major;
    while m < to_major {
        let idx = (m - 1) as usize;
        let step = table
            .get(idx)
            .ok_or_else(|| to_io(format!("no migration registered for schema major {}", m)))?;
        v = step(v);
        m += 1;
    }
    Ok(v)
}
