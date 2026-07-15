/**
 * Mock panel-bridge harness (dev/screenshot tool only). Installed exclusively when the
 * URL carries `?mock=1` (see `main.tsx`) — this module is never imported otherwise, so
 * it tree-shakes out of every real build/host path.
 *
 * `installMockBridge()` stubs `window.KomaPanel` with an in-memory fake that answers
 * every op in docs/PANEL_PROTOCOL.md: synchronous reads (`hello`/`state`/`prd_get`)
 * resolve inline, mutating ops ack `{ok:true, accepted:true}` and apply the mutation to
 * the fake state before pushing a fresh full snapshot (mirrors "ack now, result arrives
 * as a push"). The seeded state is deliberately rich — three projects covering every
 * phase, column, receipt state, and dependency shape the UI renders — so every view is
 * populated and clickable without a real daemon.
 */

import type { Snapshot } from './bridge';

// ---------------------------------------------------------------------------
// Fake state
// ---------------------------------------------------------------------------

let seq = 0;
let projects: any[] = [];
let pushHandler: ((payload: Snapshot) => void) | null = null;

function nowMs(): number {
  return Date.now();
}

function clampInt(value: unknown, min: number, max: number, fallback: number): number {
  const n = typeof value === 'number' && Number.isFinite(value) ? Math.trunc(value) : fallback;
  return Math.max(min, Math.min(max, n));
}

function defaultConfig(overrides: Record<string, unknown> = {}): Record<string, unknown> {
  return {
    maxWorkers: 2,
    bounceBudget: 3,
    workerModel: undefined,
    reviewerModel: undefined,
    keepDesks: false,
    crdPassGrade: 98,
    assumptionCheck: true,
    ...overrides,
  };
}

function comment(
  id: number,
  author: 'user' | 'office' | 'system',
  text: string,
  createdMs: number,
  receipt: { state: 'pending' | 'delivered' | 'read'; atMs?: number },
) {
  return { id, author, text, createdMs, receipt };
}

function historyEvent(atMs: number, event: string) {
  return { atMs, event };
}

// ---------------------------------------------------------------------------
// Project 1 — Running: "Notifications Revamp". 15 tasks across all 5 columns,
// including 2 parked, a dependency web, and bounce history.
// ---------------------------------------------------------------------------

function buildRunningProject() {
  const T0 = nowMs() - 6 * 60 * 60 * 1000;

  const tasks = [
    {
      id: 'notif/t1', title: 'Design fanout event schema', column: 'backlog', state: 'backlog',
      priority: 5, blockedBy: [], bounces: 0,
      description: 'Define the event envelope every notification producer publishes onto the fanout bus.',
      acceptance: ['schema documented', 'versioned envelope with a `kind` discriminant'],
      comments: [], lastReport: null, lastReview: null,
      history: [historyEvent(T0, 'created')],
    },
    {
      id: 'notif/t2', title: 'Spike: preference storage backend', column: 'backlog', state: 'backlog',
      priority: 3, blockedBy: [], bounces: 0,
      description: 'Evaluate whether per-user notification preferences live in the existing settings table or a new one.',
      acceptance: ['recommendation written up with tradeoffs'],
      comments: [], lastReport: null, lastReview: null,
      history: [historyEvent(T0, 'created')],
    },
    {
      id: 'notif/t3', title: 'Inbox empty-state copy', column: 'backlog', state: 'backlog',
      priority: 1, blockedBy: [], bounces: 0,
      description: 'Write the empty-state copy and illustration brief for a fresh inbox.',
      acceptance: ['copy approved by design'],
      comments: [], lastReport: null, lastReview: null,
      history: [historyEvent(T0, 'created')],
    },
    {
      id: 'notif/t4', title: 'Implement fanout publisher', column: 'todo', state: 'todo',
      priority: 8, blockedBy: ['notif/t1'], bounces: 0,
      description: 'Publish domain events onto the fanout bus using the schema from t1.',
      acceptance: ['unit tests cover every event kind', 'publish latency under 50ms p99'],
      comments: [], lastReport: null, lastReview: null,
      history: [historyEvent(T0, 'created'), historyEvent(T0 + 30 * 60 * 1000, 'queued')],
    },
    {
      id: 'notif/t5', title: 'Preference read/write API', column: 'todo', state: 'todo',
      priority: 6, blockedBy: ['notif/t2'], bounces: 0,
      description: 'CRUD endpoints for per-channel notification preferences.',
      acceptance: ['GET/PUT endpoints', 'defaults applied when unset'],
      comments: [], lastReport: null, lastReview: null,
      history: [historyEvent(T0, 'created')],
    },
    {
      id: 'notif/t6', title: 'Inbox list virtualization', column: 'todo', state: 'todo',
      priority: 2, blockedBy: [], bounces: 0,
      description: 'Virtualize the inbox list so it stays smooth past a few thousand notifications.',
      acceptance: ['scrolls at 60fps with 5k rows in dev tools perf trace'],
      comments: [], lastReport: null, lastReview: null,
      history: [historyEvent(T0, 'created')],
    },
    {
      id: 'notif/core-notification-pipeline/fanout-channel-delivery/build-fanout-consumer-worker', title: 'Build fanout consumer worker', column: 'onprogress', state: 'onprogress',
      priority: 7, blockedBy: [], bounces: 1, agentId: 4021, persona: 'nova',
      description: 'Consume the fanout bus and route events to the delivery queue per channel.',
      acceptance: ['at-least-once delivery', 'dead-letter queue for poison events'],
      comments: [
        comment(1, 'user', 'Ping me before you touch the retry backoff constants.', T0 + 90 * 60 * 1000, { state: 'delivered', atMs: T0 + 91 * 60 * 1000 }),
      ],
      lastReport: null, lastReview: null,
      history: [
        historyEvent(T0, 'created'),
        historyEvent(T0 + 60 * 60 * 1000, 'dispatched'),
        historyEvent(T0 + 120 * 60 * 1000, 'bounced: missing dead-letter path'),
        historyEvent(T0 + 121 * 60 * 1000, 're-dispatched'),
      ],
    },
    {
      id: 'notif/t8', title: 'Wire push-notification channel adapter', column: 'onprogress', state: 'onprogress',
      priority: 4, blockedBy: [], bounces: 0, agentId: 4022, persona: 'mika',
      description: 'Adapt the delivery queue output to the mobile push provider SDK.',
      acceptance: ['handles provider 429s with backoff'],
      comments: [], lastReport: null, lastReview: null,
      history: [historyEvent(T0, 'created'), historyEvent(T0 + 45 * 60 * 1000, 'dispatched')],
    },
    {
      id: 'notif/t9', title: 'Email channel adapter', column: 'review', state: 'review',
      priority: 6, blockedBy: ['notif/core-notification-pipeline/fanout-channel-delivery/build-fanout-consumer-worker'], bounces: 2, persona: 'tetsuo',
      description: 'Adapt the delivery queue output to the transactional email provider.',
      acceptance: ['unsubscribe link injected', 'bounce webhook updates preference store'],
      comments: [
        comment(2, 'user', 'Please double check the unsubscribe link signing.', T0 + 130 * 60 * 1000, { state: 'read', atMs: T0 + 132 * 60 * 1000 }),
        comment(3, 'office', 'Noted — added an HMAC-signed token, see the report.', T0 + 133 * 60 * 1000, { state: 'delivered', atMs: T0 + 134 * 60 * 1000 }),
        comment(4, 'user', 'One more thing: also log the provider message id.', T0 + 150 * 60 * 1000, { state: 'pending' }),
      ],
      lastReport: 'Implemented the adapter with HMAC-signed unsubscribe links; bounce webhook wired to the preference store.',
      lastReview: 'FAIL: missing test coverage for the bounce webhook handler; please add before re-review.',
      history: [
        historyEvent(T0, 'created'),
        historyEvent(T0 + 100 * 60 * 1000, 'dispatched'),
        historyEvent(T0 + 130 * 60 * 1000, 'submitted for review'),
        historyEvent(T0 + 135 * 60 * 1000, 'bounced: review failed (attempt 1)'),
        historyEvent(T0 + 160 * 60 * 1000, 'submitted for review'),
        historyEvent(T0 + 165 * 60 * 1000, 'bounced: review failed (attempt 2)'),
        historyEvent(T0 + 190 * 60 * 1000, 'submitted for review'),
      ],
    },
    {
      id: 'notif/t10', title: 'Digest email batching job', column: 'review', state: 'review',
      priority: 3, blockedBy: [], bounces: 0, persona: 'yuki',
      description: 'Nightly job that batches low-priority notifications into a single digest email.',
      acceptance: ['idempotent re-run', 'respects the user\'s digest-frequency preference'],
      comments: [], lastReport: 'Batching job implemented and dry-run tested against staging data.', lastReview: null,
      history: [historyEvent(T0, 'created'), historyEvent(T0 + 170 * 60 * 1000, 'submitted for review')],
    },
    {
      id: 'notif/t11', title: 'Rate-limit shared SMS gateway credentials', column: 'review', state: 'parked',
      priority: 9, blockedBy: [], bounces: 3, persona: 'bob',
      description: 'The SMS gateway account needs a rate-limit bump before the adapter can go further — blocked on vendor support.',
      acceptance: ['vendor confirms new rate limit', 'adapter respects the new ceiling'],
      comments: [
        comment(5, 'system', 'Parked: worker reported blocked — waiting on vendor support ticket #48213.', T0 + 200 * 60 * 1000, { state: 'read', atMs: T0 + 200 * 60 * 1000 }),
      ],
      lastReport: 'Blocked: vendor rate limit (100/min) is too low for our peak traffic; opened support ticket #48213.',
      lastReview: null,
      history: [
        historyEvent(T0, 'created'),
        historyEvent(T0 + 195 * 60 * 1000, 'dispatched'),
        historyEvent(T0 + 200 * 60 * 1000, 'parked: worker blocked'),
      ],
    },
    {
      id: 'notif/t12', title: 'Slack channel adapter over budget', column: 'review', state: 'parked',
      priority: 4, blockedBy: [], bounces: 4,
      description: 'Slack adapter has bounced past the project bounce budget and needs a human look before continuing.',
      acceptance: ['adapter passes review', 'oauth token refresh handled'],
      comments: [], lastReport: null,
      lastReview: 'FAIL: oauth token refresh still not handled after 4 attempts — escalating.',
      history: [
        historyEvent(T0, 'created'),
        historyEvent(T0 + 100 * 60 * 1000, 'bounced: review failed (attempt 1)'),
        historyEvent(T0 + 130 * 60 * 1000, 'bounced: review failed (attempt 2)'),
        historyEvent(T0 + 160 * 60 * 1000, 'bounced: review failed (attempt 3)'),
        historyEvent(T0 + 190 * 60 * 1000, 'bounced: review failed (attempt 4)'),
        historyEvent(T0 + 191 * 60 * 1000, 'parked: bounce budget exceeded'),
      ],
    },
    {
      id: 'notif/t13', title: 'In-app toast component', column: 'done', state: 'done',
      priority: 5, blockedBy: [], bounces: 0,
      description: 'Toast component for real-time in-app notification delivery.',
      acceptance: ['auto-dismiss after 5s', 'stacks up to 3 toasts'],
      comments: [], lastReport: 'Shipped and merged.', lastReview: 'PASS: matches design spec.',
      history: [historyEvent(T0, 'created'), historyEvent(T0 + 40 * 60 * 1000, 'done')],
    },
    {
      id: 'notif/t14', title: 'Notification preference UI', column: 'done', state: 'done',
      priority: 5, blockedBy: [], bounces: 1,
      description: 'Settings screen for per-channel notification toggles.',
      acceptance: ['toggles persist immediately', 'matches design spec'],
      comments: [], lastReport: 'Shipped after one round of visual feedback.', lastReview: 'PASS.',
      history: [
        historyEvent(T0, 'created'),
        historyEvent(T0 + 50 * 60 * 1000, 'bounced: spacing did not match spec'),
        historyEvent(T0 + 70 * 60 * 1000, 'done'),
      ],
    },
    {
      id: 'notif/t15', title: 'Delivery latency dashboard', column: 'done', state: 'done',
      priority: 2, blockedBy: [], bounces: 0,
      description: 'Internal dashboard tracking p50/p95/p99 delivery latency per channel.',
      acceptance: ['dashboard live in the ops org'],
      comments: [], lastReport: 'Deployed.', lastReview: 'PASS.',
      history: [historyEvent(T0, 'created'), historyEvent(T0 + 20 * 60 * 1000, 'done')],
    },
  ];

  return {
    id: 'notif',
    name: 'Notifications Revamp',
    phase: { kind: 'running' },
    deliveryPath: '/home/dev/workspace/notifications/deliver',
    seq: 42,
    tasks,
    epics: [
      { id: 'notif/epic-core', title: 'Core notification pipeline', stories: ['notif/story-fanout', 'notif/story-prefs'] },
      { id: 'notif/epic-ui', title: 'Notification UI', stories: ['notif/story-inbox'] },
    ],
    stories: [
      { id: 'notif/story-fanout', title: 'Fanout & channel delivery', tasks: ['notif/t1', 'notif/t4', 'notif/core-notification-pipeline/fanout-channel-delivery/build-fanout-consumer-worker', 'notif/t9'] },
      { id: 'notif/story-prefs', title: 'Preferences', tasks: ['notif/t2', 'notif/t5', 'notif/t10'] },
      { id: 'notif/story-inbox', title: 'Inbox surface', tasks: ['notif/t3', 'notif/t6', 'notif/t8'] },
    ],
    prdMarkdown: '# Notifications Revamp\n\nUnify email/push/SMS/Slack delivery behind one fanout pipeline.',
    lastAuditGrade: 96,
    officeTranscript: [],
    officeSummary: '',
    outbox: [],
    config: defaultConfig({ maxWorkers: 4, bounceBudget: 3, workerModel: 'claude-sonnet', reviewerModel: 'claude-opus' }),
  };
}

// ---------------------------------------------------------------------------
// Project 2 — Drafting: "Loyalty Program Redesign". No tasks yet, rich PRD +
// office chat transcript + a folded summary + outbound notices in all states.
// ---------------------------------------------------------------------------

function buildDraftingProject() {
  const prdMarkdown = [
    '# Loyalty Program Redesign',
    '',
    'Replace the points-only loyalty program with tiered rewards.',
    '',
    '## Goals',
    '',
    '- Introduce **Bronze / Silver / Gold** tiers based on trailing-12-month spend',
    '- Let users redeem points for *either* discounts or partner perks',
    '- Keep the existing `points_ledger` table as the source of truth',
    '',
    '## Non-goals',
    '',
    '- Migrating the legacy punch-card program (tracked separately)',
    '',
    '## Tier thresholds',
    '',
    '| Tier | Trailing spend | Perk budget |',
    '|---|---|---|',
    '| Bronze | < 1000 | none |',
    '| Silver | 1000 - 4999 | 2% |',
    '| Gold | >= 5000 | 5% |',
    '',
    '---',
    '',
    '## Open questions',
    '',
    '- Do tier downgrades happen instantly or at renewal? See the [rewards RFC](https://example.com/rfc/rewards-tiers) for the current proposal.',
    '',
    '```',
    'tier(spend) =',
    '  spend >= 5000 -> gold',
    '  spend >= 1000 -> silver',
    '  else           -> bronze',
    '```',
  ].join('\n');

  const researchNotes = [
    '- **Fastify 4.x**: stable; prefer `@fastify/postgres` over a raw pool for connection lifecycle.',
    '- **PostgreSQL 16**: materialized views suit the nightly tier recompute; index the',
    '  `trailing_spend` column the tier function reads.',
    '- **BullMQ 5.x**: the maintained successor to `bull`; run it against a dedicated Redis 7 instance.',
    '- Pitfall: trailing-12-month windows drift — recompute from the ledger, do not mutate in place.',
  ].join('\n');

  return {
    id: 'loyalty',
    name: 'Loyalty Program Redesign',
    phase: { kind: 'drafting' },
    deliveryPath: null,
    seq: 7,
    tasks: [],
    epics: [],
    stories: [],
    prdMarkdown,
    // TRD not drafted yet — research already came back, and the office brain is now
    // mid-draft on the TRD (see `officeActivity` below). Doc-cards matrix: prd done,
    // research done, trd active, crd backlog.
    trdMarkdown: '',
    researchNotes,
    crdMarkdown: '',
    pendingAssumptions: [],
    // Live office-brain activity (6.2d): drafting the TRD now that research is in.
    officeActivity: { label: 'drafting the TRD', sinceMs: nowMs() - 90 * 1000 },
    officeTranscript: [
      { who: 'user', text: 'We want to redesign the loyalty program around tiers instead of a flat points balance.' },
      { who: 'office', text: 'Got it. Should tiers be based on trailing spend, lifetime spend, or something else?' },
      { who: 'user', text: 'Trailing 12 months, so people can drop a tier if they slow down.' },
      { who: 'office', text: 'Makes sense. Drafted a PRD with three tiers (Bronze/Silver/Gold) and left tier-downgrade timing as an open question — take a look at the doc.' },
      { who: 'user', text: 'Looks good. Let\'s also make sure the existing points_ledger table stays authoritative, no new ledger.' },
      { who: 'office', text: 'Added that as a non-negotiable in the PRD. Research on the stack is back — drafting the TRD now.' },
    ],
    officeSummary: 'User wants tiered loyalty (Bronze/Silver/Gold) based on trailing-12mo spend, redeemable for discounts or partner perks, keeping points_ledger authoritative; tier-downgrade timing still open.',
    outbox: [
      { id: 1, text: 'PRD drafted — take a look when you have a minute.', sent: true, paused: false },
      { id: 2, text: 'Still waiting on a decision for tier-downgrade timing before breakdown can start.', sent: false, paused: true },
      { id: 3, text: 'Reminder: this project has been in Drafting for 3 days.', sent: false, paused: false },
    ],
    config: defaultConfig(),
  };
}

// ---------------------------------------------------------------------------
// Project 2b — Drafting: "Bulk CSV Import Wizard". Doc-cards matrix continued:
// PRD authored but the safeguard flagged an ungrounded assumption, so the panel
// waits on the user (review/assumptions) with research/TRD/CRD still backlog.
// ---------------------------------------------------------------------------

function buildDraftingAssumptionsProject() {
  const prdMarkdown = [
    '# Bulk CSV Import Wizard',
    '',
    'Let admins bulk-import products via CSV instead of entering them one at a time.',
    '',
    '## Goals',
    '',
    '- Validate every row client-side before upload',
    '- Show a per-row error report so a bad row does not block the whole file',
    '',
    '## Open questions',
    '',
    '- Should a partially-valid file commit the good rows, or is it all-or-nothing?',
  ].join('\n');

  return {
    id: 'csv-import',
    name: 'Bulk CSV Import Wizard',
    phase: { kind: 'drafting' },
    deliveryPath: null,
    seq: 2,
    tasks: [],
    epics: [],
    stories: [],
    prdMarkdown,
    trdMarkdown: '',
    researchNotes: '',
    crdMarkdown: '',
    // Doc-cards matrix: prd review/assumptions (newest non-empty doc), research/trd/crd
    // still backlog behind it.
    pendingAssumptions: [
      'Assumed partial imports commit the valid rows and skip the invalid ones — the user never said.',
    ],
    // Waiting-on-user activity (6.2c feature 5): the driver stamps this when the pipeline is
    // stopped on pending assumptions. `sinceMs: 0` -> the UI hides the elapsed suffix.
    officeActivity: { label: 'waiting on you — 1 assumption', sinceMs: 0 },
    officeTranscript: [
      { who: 'user', text: 'Admins need to bulk-import products from a CSV instead of typing them in one at a time.' },
      { who: 'office', text: 'Drafted a PRD with client-side validation and a per-row error report. One open question I could not resolve: should a partially-valid file commit the good rows, or is it all-or-nothing? Flagged that as an assumption for now — take a look.' },
    ],
    officeSummary: 'Admin bulk CSV import with client-side validation and a per-row error report; partial-import commit behavior is an open/assumed question.',
    outbox: [],
    config: defaultConfig(),
  };
}

// ---------------------------------------------------------------------------
// Project 3 — Halted: "Legacy Auth Migration". A parked blocker chain: one
// parked task transitively blocks two dependents.
// ---------------------------------------------------------------------------

function buildHaltedProject() {
  const T0 = nowMs() - 3 * 24 * 60 * 60 * 1000;

  const tasks = [
    {
      id: 'legacy/t1', title: 'Migrate auth service off the legacy session store', column: 'review', state: 'parked',
      priority: 9, blockedBy: [], bounces: 2,
      description: 'The legacy session store does not support the new token format; migration is blocked on a decision from the security team.',
      acceptance: ['security sign-off recorded', 'new store passes the auth integration suite'],
      comments: [
        comment(1, 'system', 'Parked: worker reported blocked — awaiting security team sign-off on the new token format.', T0 + 60 * 60 * 1000, { state: 'read', atMs: T0 + 60 * 60 * 1000 }),
      ],
      lastReport: 'Blocked: cannot proceed without security sign-off on token format change.',
      lastReview: null,
      history: [
        historyEvent(T0, 'created'),
        historyEvent(T0 + 30 * 60 * 1000, 'dispatched'),
        historyEvent(T0 + 60 * 60 * 1000, 'parked: worker blocked'),
      ],
    },
    {
      id: 'legacy/t2', title: 'Migrate database schema for new session format', column: 'todo', state: 'todo',
      priority: 7, blockedBy: ['legacy/t1'], bounces: 0,
      description: 'Add the columns the new session format needs; depends on the token format being finalized.',
      acceptance: ['migration is reversible', 'zero-downtime rollout plan documented'],
      comments: [], lastReport: null, lastReview: null,
      history: [historyEvent(T0, 'created')],
    },
    {
      id: 'legacy/t3', title: 'Cut over API gateway to new session validation', column: 'backlog', state: 'backlog',
      priority: 6, blockedBy: ['legacy/t2'], bounces: 0,
      description: 'Switch the gateway to validate the new session format; depends on the schema migration.',
      acceptance: ['gateway validates both formats during rollout window'],
      comments: [], lastReport: null, lastReview: null,
      history: [historyEvent(T0, 'created')],
    },
    {
      id: 'legacy/t4', title: 'Delete dead feature-flag plumbing', column: 'backlog', state: 'backlog',
      priority: 1, blockedBy: [], bounces: 0,
      description: 'Unrelated cleanup task — not on the migration critical path.',
      acceptance: ['no references to the removed flags remain'],
      comments: [], lastReport: null, lastReview: null,
      history: [historyEvent(T0, 'created')],
    },
    {
      id: 'legacy/t5', title: 'Document legacy session store quirks', column: 'done', state: 'done',
      priority: 3, blockedBy: [], bounces: 0,
      description: 'Write up the legacy store\'s undocumented behavior for the migration team.',
      acceptance: ['doc reviewed by the on-call rotation'],
      comments: [], lastReport: 'Documented and reviewed.', lastReview: 'PASS.',
      history: [historyEvent(T0, 'created'), historyEvent(T0 + 10 * 60 * 1000, 'done')],
    },
  ];

  return {
    id: 'legacy',
    name: 'Legacy Auth Migration',
    phase: { kind: 'halted', reason: '"legacy/t1" is parked and blocks the rest of the migration chain' },
    deliveryPath: '/home/dev/workspace/legacy-auth/deliver',
    seq: 19,
    tasks,
    epics: [],
    stories: [],
    prdMarkdown: '# Legacy Auth Migration\n\nMove the auth service off the legacy session store onto the new token format.',
    officeTranscript: [],
    officeSummary: '',
    outbox: [],
    config: defaultConfig({ maxWorkers: 2, bounceBudget: 2, keepDesks: true }),
  };
}

function buildInitialProjects(): any[] {
  return [buildRunningProject(), buildDraftingProject(), buildDraftingAssumptionsProject(), buildHaltedProject()];
}

// ---------------------------------------------------------------------------
// Snapshot plumbing
// ---------------------------------------------------------------------------

function envelope(): Snapshot {
  seq += 1;
  // Deep-clone so callers mutating the returned snapshot (or the store's derived
  // projections) can never reach back into the mock's own source of truth.
  return { kind: 'snapshot', seq, truncated: false, projects: JSON.parse(JSON.stringify(projects)) };
}

function pushSnapshot(): void {
  if (pushHandler) pushHandler(envelope());
}

/** Schedule a push just after the ack resolves, mirroring the real protocol's
 * "ack immediately, mutation arrives as a subsequent push" (PANEL_PROTOCOL.md 1.2). */
function schedulePush(): void {
  queueMicrotask(pushSnapshot);
}

function findProject(id: string): any {
  return projects.find((p) => p.id === id);
}

function findTask(taskId: string): { project: any; task: any } | undefined {
  for (const project of projects) {
    const task = project.tasks.find((t: any) => t.id === taskId);
    if (task) return { project, task };
  }
  return undefined;
}

function nextCommentId(task: any): number {
  return task.comments.reduce((max: number, c: any) => Math.max(max, c.id), 0) + 1;
}

function ok(): any {
  return { ok: true, accepted: true };
}

function err(message: string): any {
  return { error: message };
}

// ---------------------------------------------------------------------------
// Op handling
// ---------------------------------------------------------------------------

const OFFICE_REPLIES = [
  'Got it — noted, and updated the PRD accordingly.',
  'Makes sense. I\'ll fold that into the plan.',
  'Good call. Anything else before we move to breakdown?',
  'Understood, tracking that as an open question in the doc.',
];

function officeReplyTo(message: string): string {
  const hash = Array.from(message).reduce((h, c) => (h * 31 + c.charCodeAt(0)) >>> 0, 7);
  return OFFICE_REPLIES[hash % OFFICE_REPLIES.length];
}

function handleOp(payload: any): any {
  const op = payload?.op;

  switch (op) {
    case 'hello':
    case 'state':
      return { ok: true, snapshot: envelope() };

    case 'prd_get': {
      const p = findProject(payload.project);
      if (!p) return err(`unknown project: ${payload.project}`);
      return { ok: true, prd: p.prdMarkdown ?? null };
    }

    case 'office_chat': {
      const p = findProject(payload.project);
      if (!p) return err(`unknown project: ${payload.project}`);
      const message = String(payload.message ?? '');
      p.officeTranscript = [...p.officeTranscript, { who: 'user', text: message }];
      setTimeout(() => {
        p.officeTranscript = [...p.officeTranscript, { who: 'office', text: officeReplyTo(message) }];
        pushSnapshot();
      }, 500);
      schedulePush();
      return ok();
    }

    case 'authorize': {
      const p = findProject(payload.project);
      if (!p) return err(`unknown project: ${payload.project}`);
      p.deliveryPath = payload.deliveryPath ?? p.deliveryPath;
      p.phase = { kind: 'running' };
      schedulePush();
      return ok();
    }

    case 'interrupt': {
      const p = findProject(payload.project);
      if (!p) return err(`unknown project: ${payload.project}`);
      p.phase = { kind: 'interrupted' };
      schedulePush();
      return ok();
    }

    case 'resume': {
      const p = findProject(payload.project);
      if (!p) return err(`unknown project: ${payload.project}`);
      p.phase = { kind: 'running' };
      schedulePush();
      return ok();
    }

    case 'card_move': {
      const found = findTask(payload.task);
      if (!found) return err(`unknown task: ${payload.task}`);
      const to = payload.to;
      found.task.column = to;
      found.task.state = to;
      found.task.history = [...found.task.history, historyEvent(nowMs(), `moved to ${to}`)];
      schedulePush();
      return ok();
    }

    case 'comment_add': {
      const found = findTask(payload.task);
      if (!found) return err(`unknown task: ${payload.task}`);
      const id = nextCommentId(found.task);
      found.task.comments = [
        ...found.task.comments,
        comment(id, 'user', String(payload.text ?? ''), nowMs(), { state: 'pending' }),
      ];
      schedulePush();
      return ok();
    }

    case 'unpark': {
      const found = findTask(payload.task);
      if (!found) return err(`unknown task: ${payload.task}`);
      found.task.state = 'todo';
      found.task.column = 'todo';
      found.task.history = [...found.task.history, historyEvent(nowMs(), 'unparked')];
      schedulePush();
      return ok();
    }

    case 'edit_task': {
      const found = findTask(payload.task);
      if (!found) return err(`unknown task: ${payload.task}`);
      const { op: _op, task: _task, ...patch } = payload;
      Object.assign(found.task, patch);
      schedulePush();
      return ok();
    }

    case 'edit_deps': {
      const found = findTask(payload.task);
      if (!found) return err(`unknown task: ${payload.task}`);
      if (Array.isArray(payload.blockedBy)) {
        found.task.blockedBy = payload.blockedBy;
      }
      schedulePush();
      return ok();
    }

    case 'config_set': {
      const p = findProject(payload.project);
      if (!p) return err(`unknown project: ${payload.project}`);
      const next = { ...p.config };
      if (payload.maxWorkers !== undefined) next.maxWorkers = clampInt(payload.maxWorkers, 1, 4, next.maxWorkers);
      if (payload.bounceBudget !== undefined) next.bounceBudget = payload.bounceBudget;
      if (payload.workerModel !== undefined) next.workerModel = payload.workerModel;
      if (payload.reviewerModel !== undefined) next.reviewerModel = payload.reviewerModel;
      if (payload.keepDesks !== undefined) next.keepDesks = payload.keepDesks;
      if (payload.crdPassGrade !== undefined) next.crdPassGrade = clampInt(payload.crdPassGrade, 0, 100, next.crdPassGrade as number);
      if (payload.assumptionCheck !== undefined) next.assumptionCheck = payload.assumptionCheck;
      p.config = next;
      schedulePush();
      return ok();
    }

    case 'project_create': {
      const id = `proj-${Math.random().toString(36).slice(2, 8)}`;
      projects = [
        ...projects,
        {
          id,
          name: String(payload.name ?? 'Untitled Project'),
          phase: { kind: 'drafting' },
          deliveryPath: null,
          seq: 0,
          tasks: [],
          epics: [],
          stories: [],
          prdMarkdown: '',
          trdMarkdown: '',
          researchNotes: '',
          crdMarkdown: '',
          pendingAssumptions: [],
          lastAuditGrade: null,
          officeTranscript: [],
          officeSummary: '',
          outbox: [],
          config: defaultConfig(),
        },
      ];
      schedulePush();
      return ok();
    }

    case 'project_archive': {
      projects = projects.filter((p) => p.id !== payload.project);
      schedulePush();
      return ok();
    }

    case 'breakdown':
    case 'task_detail':
      // No UI affordance calls these today; ack so any future caller degrades
      // gracefully instead of hitting an "unknown op" error.
      schedulePush();
      return ok();

    default:
      return err(`unknown panel op: ${op}`);
  }
}

// ---------------------------------------------------------------------------
// Install
// ---------------------------------------------------------------------------

/** Stub `window.KomaPanel` with the in-memory fake described above. Call once,
 * before any code that reads `window.KomaPanel` (the real `bridge.ts` singleton
 * is constructed at import time) — see `main.tsx`'s dynamic-import ordering. */
export function installMockBridge(): void {
  seq = 0;
  projects = buildInitialProjects();
  pushHandler = null;

  window.KomaPanel = {
    send: (payload: any, _timeoutMs?: number) => Promise.resolve(handleOp(payload)),
    onPush: (handler: (payload: any) => void) => {
      pushHandler = handler;
    },
  };
}
