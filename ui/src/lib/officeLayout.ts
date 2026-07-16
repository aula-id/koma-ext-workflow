/**
 * Pixel virtual-office layout — PURE, testable helpers (no React, no DOM).
 *
 * The office view (../views/OfficeMap.tsx) is a projection of the board snapshot onto a
 * fixed pool of 10 worker personas (mirrors office-core `persona.rs` WORKER_PERSONAS, same
 * order). These functions decide the room LAYOUT from `maxWorkers` and derive per-persona
 * DESK STATE from the project's tasks. Everything here is a pure function of its inputs so it
 * can be unit-tested without rendering (officeLayout.test.ts).
 */

/** The 10 worker personas, in stable order. MUST match office-core `WORKER_PERSONAS`. */
export const PERSONA_ORDER = [
  'nova', 'mika', 'tetsuo', 'bob', 'yuki', 'dax', 'ines', 'koji', 'vera', 'pip',
] as const;

export type TierId = 'cozy' | 'bullpen' | 'two-rows' | 'open-floor';
export type PersonaStatus = 'working' | 'review-debate' | 'parked' | 'idle';

/** Desk-cell geometry in the office's virtual coordinate space (the component scales it). */
export const DESK = { cellW: 168, cellH: 196, padX: 40, padY: 32, gapX: 26, gapY: 42 } as const;

const PREFIX = 'office-worker-';

/** Strip the `office-worker-` prefix to a short persona name; a value that is already short
 * (or empty) passes through unchanged. Defensive: the snapshot ships short names, but a full
 * binding id must normalize the same way. */
export function stripPersona(persona: string | undefined | null): string {
  if (!persona) return '';
  return persona.startsWith(PREFIX) ? persona.slice(PREFIX.length) : persona;
}

function clampInt(n: unknown, min: number, max: number, fallback: number): number {
  const v = typeof n === 'number' && Number.isFinite(n) ? Math.floor(n) : fallback;
  return Math.max(min, Math.min(max, v));
}

/** The office self-caps concurrent workers at 4 (office-core MAX_PROJECT_WORKERS); the panel
 * config clamps `maxWorkers` to 1..4. Missing config falls back to the kernel default (2). */
export function clampMaxWorkers(maxWorkers: unknown): number {
  return clampInt(maxWorkers, 1, 4, 2);
}

/**
 * Layout template for a worker count: 1-2 cozy, 3-4 bullpen, 5-6 two-rows, 7-10 open-floor.
 * Clamps out-of-range input (0/NaN -> 1 -> cozy; 11+ -> 10 -> open-floor) so it is total.
 */
export function tierFor(maxWorkers: number): TierId {
  const n = clampInt(maxWorkers, 1, 10, 1);
  if (n <= 2) return 'cozy';
  if (n <= 4) return 'bullpen';
  if (n <= 6) return 'two-rows';
  return 'open-floor';
}

/** Desks a tier draws: cozy 2, bullpen 4, two-rows 6, open-floor 10. */
export function deskCountFor(tier: TierId): number {
  switch (tier) {
    case 'cozy': return 2;
    case 'bullpen': return 4;
    case 'two-rows': return 6;
    case 'open-floor': return 10;
  }
}

/** Grid columns a tier lays desks out in. */
export function colsFor(tier: TierId): number {
  switch (tier) {
    case 'cozy': return 2;
    case 'bullpen': return 2;
    case 'two-rows': return 3;
    case 'open-floor': return 5;
  }
}

export interface Station {
  index: number;
  x: number;
  y: number;
}

/** Lay `count` desks out in a `cols`-wide grid, in the virtual office coordinate space. */
export function layoutDesks(count: number, cols: number): Station[] {
  const c = Math.max(1, Math.floor(cols));
  const out: Station[] = [];
  for (let i = 0; i < count; i++) {
    const row = Math.floor(i / c);
    const col = i % c;
    out.push({
      index: i,
      x: DESK.padX + col * (DESK.cellW + DESK.gapX),
      y: DESK.padY + row * (DESK.cellH + DESK.gapY),
    });
  }
  return out;
}

/** Desk coordinates for a tier's base arrangement. The component grows past this (via
 * `layoutDesks`) only when more personas are occupied than the tier's base count, so an
 * occupied desk is never dropped when the tier shrinks. */
export function stationsFor(tier: TierId): Station[] {
  return layoutDesks(deskCountFor(tier), colsFor(tier));
}

// ---------------------------------------------------------------------------
// Presence derivation
// ---------------------------------------------------------------------------

export interface OfficeTask {
  id: string;
  title?: string;
  state: string;
  priority?: number;
  persona?: string;
}

/** One line of a sprint-review ceremony transcript (feature: sprints), mirroring office-core
 * digest.rs's wire shape `{ speaker, text }` (the domain `SprintLine { speaker, line }` renamed
 * on the wire — `text`, not `line`). `speaker` is either a `PERSONA_ORDER` name (a worker who
 * worked the sprint's tasks) or one of the fixed ceremony roles `'reviewer'` | `'researcher'` |
 * `'office'` (the PM's closing line). */
export interface OfficeSprintLine {
  speaker: string;
  text: string;
}

/** One sprint of a project-track plan (feature: sprints), mirroring office-core digest.rs's
 * `sprints[]` wire entry. `status` is the wire string exactly as office-core's
 * `sprint_status_str` emits it — note `'inreview'`, not `'inReview'` (unlike `activeSprint`
 * below, which DOES use camelCase for its own `inReview` boolean). `transcript` is present only
 * while `status === 'inreview'`. */
export interface OfficeSprint {
  index: number;
  goal: string;
  status: 'pending' | 'active' | 'inreview' | 'done';
  total: number;
  done: number;
  tasks: string[];
  transcript?: OfficeSprintLine[];
}

/** Pointer to the project's CURRENT sprint (feature: sprints), mirroring office-core digest.rs's
 * `activeSprint` wire object. Present only when a sprint is `Active` or `InReview`. */
export interface OfficeActiveSprint {
  index: number;
  count: number;
  goal: string;
  total: number;
  done: number;
  inReview: boolean;
}

export interface OfficeProject {
  tasks?: OfficeTask[];
  config?: { maxWorkers?: number | null } | null;
  research?: unknown;
  researchActive?: boolean;
  audit?: unknown;
  auditActive?: boolean;
  /** Full sprint list + the current-sprint pointer (feature: sprints). Both absent on a
   * pre-sprints/no-sprint-track snapshot (back-compat: classic office renders unchanged). */
  sprints?: OfficeSprint[];
  activeSprint?: OfficeActiveSprint | null;
}

export interface PersonaPresence {
  persona: string;
  status: PersonaStatus;
  taskId?: string;
  taskTitle?: string;
  /** May blink a live code screen: a `working` persona within the blink capacity. */
  monitorActive: boolean;
  /** Occupied (working) but beyond the blink capacity -> seated, dark monitor, "waiting for a chair". */
  waitingForChair: boolean;
}

// The task states the office draws a seated persona for, most-active first. Idle personas
// (no such task) fall through to 'idle'.
const OCCUPIED_STATE: Record<string, PersonaStatus> = {
  onprogress: 'working',
  review: 'review-debate',
  parked: 'parked',
};
const STATE_RANK: Record<string, number> = { onprogress: 0, review: 1, parked: 2 };

/**
 * Per-persona desk state for the whole 10-slot pool, derived from the project's tasks.
 * A persona is `working` (a task onprogress), `review-debate` (a task in review — the reviewer
 * stands at the desk), `parked` (a task parked mid-work), or `idle` (no active task). When two
 * tasks hash to one persona the more-active one wins (onprogress > review > parked, then
 * priority). Blink capacity is `min(maxWorkers, 4)`: `working` personas beyond it are
 * "waiting for a chair" (dark monitor) rather than blinking.
 */
export function presenceFor(project: OfficeProject | null | undefined): PersonaPresence[] {
  const tasks = project?.tasks ?? [];
  const maxWorkers = clampMaxWorkers(project?.config?.maxWorkers);

  // persona -> the winning occupied task for it.
  const byPersona = new Map<string, OfficeTask>();
  for (const t of tasks) {
    if (!(t.state in OCCUPIED_STATE)) continue;
    const name = stripPersona(t.persona);
    if (!name) continue;
    const cur = byPersona.get(name);
    if (!cur || betterTask(t, cur)) byPersona.set(name, t);
  }

  // Working personas ordered by priority desc, id asc -> the first `maxWorkers` get live monitors.
  const workingOrdered = [...byPersona.entries()]
    .filter(([, t]) => t.state === 'onprogress')
    .sort((a, b) => taskOrder(a[1], b[1]))
    .map(([name]) => name);
  const blinking = new Set(workingOrdered.slice(0, maxWorkers));

  return PERSONA_ORDER.map((persona) => {
    const t = byPersona.get(persona);
    if (!t) {
      return { persona, status: 'idle' as PersonaStatus, monitorActive: false, waitingForChair: false };
    }
    const status = OCCUPIED_STATE[t.state];
    const isWorking = status === 'working';
    return {
      persona,
      status,
      taskId: t.id,
      taskTitle: t.title,
      monitorActive: isWorking && blinking.has(persona),
      waitingForChair: isWorking && !blinking.has(persona),
    };
  });
}

function betterTask(a: OfficeTask, b: OfficeTask): boolean {
  const ra = STATE_RANK[a.state] ?? 9;
  const rb = STATE_RANK[b.state] ?? 9;
  if (ra !== rb) return ra < rb;
  return taskOrder(a, b) < 0;
}

function taskOrder(a: OfficeTask, b: OfficeTask): number {
  return (b.priority ?? 0) - (a.priority ?? 0) || a.id.localeCompare(b.id);
}

/** Count of personas with an occupied desk (working / review-debate / parked). */
export function occupiedCount(presence: PersonaPresence[]): number {
  return presence.filter((p) => p.status !== 'idle').length;
}

/** Whether the project-level researcher is in flight (fixed-staff reading animation). */
export function isResearchLive(project: OfficeProject | null | undefined): boolean {
  return Boolean(project?.researchActive || project?.research);
}

/** Whether the project-level clean-build auditor is in flight (fixed-staff judging animation). */
export function isAuditLive(project: OfficeProject | null | undefined): boolean {
  return Boolean(project?.auditActive || project?.audit);
}

// ---------------------------------------------------------------------------
// Live office activity (6.2d)
// ---------------------------------------------------------------------------

/** Formats an elapsed duration since `sinceMs` as "Ns" under a minute, else "m:ss". */
export function formatElapsed(nowMs: number, sinceMs: number): string {
  const totalSeconds = Math.max(0, Math.floor((nowMs - sinceMs) / 1000));
  if (totalSeconds < 60) return `${totalSeconds}s`;
  const minutes = Math.floor(totalSeconds / 60);
  const seconds = totalSeconds % 60;
  return `${minutes}:${String(seconds).padStart(2, '0')}`;
}

const NON_DRAFTING_LABELS = new Set(['researching the stack', 'auditing the delivery']);

/** The prefix the driver stamps on the "waiting on you — N assumptions" activity label
 * (office-daemon driver.rs `office_activity`), when the drafting pipeline is stopped on the
 * safeguard's pending assumptions. */
const WAITING_ON_USER_PREFIX = 'waiting on you';

/** Whether an officeActivity label is the safeguard's "waiting on you — N assumptions" state
 * (the pipeline is stopped pending the user's approval), as opposed to live work. Prefix-matched
 * because the label carries a variable assumption count. */
export function isWaitingOnUserActivity(label: string | undefined | null): boolean {
  return Boolean(label) && (label as string).startsWith(WAITING_ON_USER_PREFIX);
}

/** Whether an officeActivity label is a "drafting family" activity (drafting/fact-checking/
 * breaking-down/replying/summarizing) as opposed to research/audit (which have their own
 * dedicated staff animations elsewhere in the office map) or the waiting-on-user state (which is
 * not live work at all). */
export function isDraftingFamilyActivity(label: string | undefined | null): boolean {
  if (!label) return false;
  if (isWaitingOnUserActivity(label)) return false;
  return !NON_DRAFTING_LABELS.has(label);
}

// ---------------------------------------------------------------------------
// Sprint-review meeting room (feature: sprints)
// ---------------------------------------------------------------------------

/** Whether the project's current sprint is in its review ceremony — the office map swaps the
 * desk grid for the meeting-room scene while this is true. */
export function isSprintReview(project: OfficeProject | null | undefined): boolean {
  return Boolean(project?.activeSprint?.inReview);
}

/** The sprint object under review (carrying the ceremony transcript), or `null` when no sprint
 * is currently in review (including the legacy no-sprint flow, where `sprints` is absent). */
export function reviewedSprint(project: OfficeProject | null | undefined): OfficeSprint | null {
  const active = project?.activeSprint;
  if (!active || !active.inReview) return null;
  const sprints = project?.sprints ?? [];
  return sprints[active.index] ?? null;
}

/** "sprint i/N — goal" badge text (1-indexed for display), or `null` when the project carries no
 * sprint pointer at all (pre-sprints / no-sprint-track snapshot — back-compat: no badge). */
export function sprintBadgeText(project: OfficeProject | null | undefined): string | null {
  const active = project?.activeSprint;
  if (!active) return null;
  return `sprint ${active.index + 1}/${active.count} — ${active.goal}`;
}

/** Personas who worked a sprint's tasks (feature: sprints meeting room), in stable
 * `PERSONA_ORDER` — the "workers around the table" seats. Matched via the project's live
 * task -> persona binding (the same field `presenceFor` reads), not the transcript text, so a
 * worker is seated at the table even before the ceremony has spoken their line. */
export function sprintAttendees(
  project: OfficeProject | null | undefined,
  sprint: OfficeSprint | null | undefined,
): string[] {
  if (!sprint) return [];
  const taskIds = new Set(sprint.tasks);
  const tasks = project?.tasks ?? [];
  const personas = new Set<string>();
  for (const t of tasks) {
    if (!taskIds.has(t.id)) continue;
    const name = stripPersona(t.persona);
    if (name) personas.add(name);
  }
  return PERSONA_ORDER.filter((p) => personas.has(p));
}

/** Meeting-table seat geometry (feature: sprints), in the office's virtual coordinate space —
 * mirrors `DESK`'s role for the desk grid. The PM and reviewer anchor the table's head; workers
 * ring the remaining seats in a `cols`-wide grid below. */
export const TABLE = { padX: 40, padY: 32, seatW: 140, seatH: 150, cols: 3 } as const;

export interface MeetingSeat {
  role: 'pm' | 'reviewer' | 'worker';
  persona?: string;
  x: number;
  y: number;
}

/**
 * Seat layout for the sprint-review meeting room: the PM at the head (top-left of the table),
 * the reviewer beside them (top-right), and one seat per attendee worker persona ringed around
 * the table's remaining rows. Deterministic (attendees is already `PERSONA_ORDER`-stable) — the
 * researcher keeps their existing bookshelf-lane "corner observer spot" rather than getting a
 * table seat, so it is not listed here.
 */
export function meetingSeatsFor(attendees: string[]): MeetingSeat[] {
  const seats: MeetingSeat[] = [
    { role: 'pm', x: TABLE.padX + TABLE.seatW, y: TABLE.padY },
    { role: 'reviewer', x: TABLE.padX + TABLE.seatW * 2, y: TABLE.padY },
  ];
  attendees.forEach((persona, i) => {
    const col = i % TABLE.cols;
    const row = Math.floor(i / TABLE.cols);
    seats.push({
      role: 'worker',
      persona,
      x: TABLE.padX + col * TABLE.seatW,
      y: TABLE.padY + TABLE.seatH * (row + 1),
    });
  });
  return seats;
}

// ---------------------------------------------------------------------------
// Ambient idle life (feature: ambient-idle-life)
// ---------------------------------------------------------------------------

/** Idle personas (no active desk), minus any currently seated at the meeting table (a sprint
 * review's attendees) — the pool the ambient idle-life system animates. Passing the review's
 * `sprintAttendees()` result as `excluded` is what keeps idle sprites off the meeting table: a
 * sprint's tasks are typically `Done` by the time its review starts, which reads as `idle` in
 * `presenceFor` — without this exclusion those same personas would double-book as both meeting
 * attendees AND wandering idle sprites. */
export function idlePersonasFor(presence: PersonaPresence[], excluded: readonly string[] = []): string[] {
  const excludeSet = new Set(excluded);
  return presence.filter((p) => p.status === 'idle' && !excludeSet.has(p.persona)).map((p) => p.persona);
}

export type IdleActivity = 'wander' | 'gossip' | 'cooler';

export interface IdleAssignment {
  persona: string;
  activity: IdleActivity;
  /** Gossip only: the paired persona (deterministically paired within the same decision
   * window). */
  partner?: string;
  /** 0..1 progress through the current decision window (an interpolant for wander position). */
  t: number;
  /** Wander only: normalized 0..1 waypoint fractions along the ambient lane to interpolate
   * between (start -> end) as `t` advances this window. */
  waypointA?: number;
  waypointB?: number;
  /** An occasional, hardcoded flavor line for a gossip/cooler bubble (no model calls) — index and
   * presence chosen deterministically per decision window, `undefined` most windows so the
   * bubble doesn't spam every tick. */
  bubble?: string;
}

/** ~8s per idle decision window at the office's 200ms tick (`useTick(200, ...)`). */
const IDLE_WINDOW_TICKS = 40;

const GOSSIP_LINES = ['...', 'no way, really?', 'true story', 'huh, neat', 'five more minutes'];
const COOLER_LINES = ['...', 'ahh, refreshing', 'needed that', 'back to it'];

/** A tiny deterministic string hash -> a stable 0..1 pseudo-random float. Same key always yields
 * the same value, so idle movement is reproducible across renders and in tests — the office view
 * avoids `Math.random` in its render/animation paths (every frame derives from the single `tick`
 * clock; see `useTick`), and this keeps the idle-life system consistent with that.
 *
 * FNV-1a followed by a Murmur3-style finalizer: a plain rolling hash (`h = h*31 + c`) has almost
 * no avalanche on a short varying suffix — e.g. keys differing only in a trailing window-index
 * digit (`...|0` vs `...|1`) come out ONE APART, not spread across the range — which made every
 * idle decision window pick the same activity. The finalizer's xor/multiply/shift mixing fixes
 * that: adjacent keys land on well-spread, uncorrelated seeds. */
function hashSeed(key: string): number {
  let h = 0x811c9dc5; // FNV offset basis
  for (let i = 0; i < key.length; i++) {
    h ^= key.charCodeAt(i);
    h = Math.imul(h, 0x01000193); // FNV prime
  }
  h ^= h >>> 16;
  h = Math.imul(h, 0x85ebca6b);
  h ^= h >>> 13;
  h = Math.imul(h, 0xc2b2ae35);
  h ^= h >>> 16;
  return (h >>> 0) / 0xffffffff;
}

/**
 * Ambient idle-life assignment for the tick (feature: ambient-idle-life): each idle persona (no
 * active desk, no meeting-table seat) gets a deterministic activity for the current decision
 * window — wander the floor between two seeded waypoints, gossip-pair at the empty commons table
 * with another idle persona, or visit the water cooler. Pure function of
 * `(idlePersonas, tick)` — no `Math.random` — so it is reproducible and test-friendly.
 * `windowIndex` (derived from `tick`) reshuffles activities/pairs/lines every
 * `IDLE_WINDOW_TICKS`; positions/bubbles hold steady within a window so they don't flicker.
 */
export function idleAssignmentsFor(idlePersonas: readonly string[], tick: number): IdleAssignment[] {
  const windowIndex = Math.floor(tick / IDLE_WINDOW_TICKS);
  const t = (tick % IDLE_WINDOW_TICKS) / IDLE_WINDOW_TICKS;
  const sorted = [...idlePersonas].sort();
  const paired = new Map<string, IdleAssignment>();

  // Deterministically pair adjacent idle personas (stable sort order) for gossip, gated by a
  // per-pair-per-window seed so a pairing isn't permanent.
  for (let i = 0; i + 1 < sorted.length; i += 2) {
    const a = sorted[i];
    const b = sorted[i + 1];
    if (hashSeed(`gossip|${a}|${b}|${windowIndex}`) >= 0.5) continue;
    const bubble =
      hashSeed(`gossip-show|${a}|${b}|${windowIndex}`) < 0.4
        ? GOSSIP_LINES[Math.floor(hashSeed(`gossip-line|${a}|${b}|${windowIndex}`) * GOSSIP_LINES.length)]
        : undefined;
    paired.set(a, { persona: a, activity: 'gossip', partner: b, t, bubble });
    paired.set(b, { persona: b, activity: 'gossip', partner: a, t, bubble });
  }

  return idlePersonas.map((persona) => {
    const pair = paired.get(persona);
    if (pair) return pair;
    if (hashSeed(`kind|${persona}|${windowIndex}`) < 0.3) {
      const bubble =
        hashSeed(`cooler-show|${persona}|${windowIndex}`) < 0.4
          ? COOLER_LINES[Math.floor(hashSeed(`cooler-line|${persona}|${windowIndex}`) * COOLER_LINES.length)]
          : undefined;
      return { persona, activity: 'cooler', t, bubble };
    }
    return {
      persona,
      activity: 'wander',
      t,
      waypointA: hashSeed(`wp-a|${persona}|${windowIndex}`),
      waypointB: hashSeed(`wp-b|${persona}|${windowIndex}`),
    };
  });
}
