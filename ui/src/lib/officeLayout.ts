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

export interface OfficeProject {
  tasks?: OfficeTask[];
  config?: { maxWorkers?: number | null } | null;
  research?: unknown;
  researchActive?: boolean;
  audit?: unknown;
  auditActive?: boolean;
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
