import { describe, it, expect } from 'vitest';
import {
  PERSONA_ORDER,
  TABLE,
  clampMaxWorkers,
  deskCountFor,
  formatElapsed,
  idleAssignmentsFor,
  idlePersonasFor,
  isAuditLive,
  isDraftingFamilyActivity,
  isResearchLive,
  isSprintReview,
  isWaitingOnUserActivity,
  meetingSeatsFor,
  occupiedCount,
  presenceFor,
  reviewedSprint,
  sprintAttendees,
  sprintBadgeText,
  stationsFor,
  stripPersona,
  tierFor,
  type OfficeProject,
  type OfficeSprint,
} from './officeLayout';

describe('tierFor', () => {
  it('maps worker counts to the four layout tiers', () => {
    expect(tierFor(1)).toBe('cozy');
    expect(tierFor(2)).toBe('cozy');
    expect(tierFor(3)).toBe('bullpen');
    expect(tierFor(4)).toBe('bullpen');
    expect(tierFor(5)).toBe('two-rows');
    expect(tierFor(6)).toBe('two-rows');
    expect(tierFor(7)).toBe('open-floor');
    expect(tierFor(10)).toBe('open-floor');
  });

  it('clamps out-of-range / non-finite input (total function)', () => {
    expect(tierFor(0)).toBe('cozy');
    expect(tierFor(-3)).toBe('cozy');
    expect(tierFor(11)).toBe('open-floor');
    expect(tierFor(999)).toBe('open-floor');
    expect(tierFor(NaN as unknown as number)).toBe('cozy');
  });
});

describe('deskCountFor / stationsFor', () => {
  it('draws the tier desk count', () => {
    expect(deskCountFor('cozy')).toBe(2);
    expect(deskCountFor('bullpen')).toBe(4);
    expect(deskCountFor('two-rows')).toBe(6);
    expect(deskCountFor('open-floor')).toBe(10);
  });

  it('returns one station per desk at deterministic grid coordinates', () => {
    expect(stationsFor('cozy')).toHaveLength(2);
    expect(stationsFor('bullpen')).toHaveLength(4);
    expect(stationsFor('two-rows')).toHaveLength(6);
    expect(stationsFor('open-floor')).toHaveLength(10);

    // cozy: one row of two.
    const cozy = stationsFor('cozy');
    expect(cozy[0]).toEqual({ index: 0, x: 40, y: 32 });
    expect(cozy[1]).toEqual({ index: 1, x: 234, y: 32 });

    // bullpen: 2x2 — the third desk wraps to the second row.
    const bullpen = stationsFor('bullpen');
    expect(bullpen[2]).toEqual({ index: 2, x: 40, y: 270 });

    // open-floor: 5-wide — desk 5 starts the second row.
    const open = stationsFor('open-floor');
    expect(open[4]).toEqual({ index: 4, x: 816, y: 32 });
    expect(open[5]).toEqual({ index: 5, x: 40, y: 270 });
  });
});

describe('stripPersona', () => {
  it('strips the office-worker- prefix, leaving already-short names alone', () => {
    expect(stripPersona('office-worker-nova')).toBe('nova');
    expect(stripPersona('nova')).toBe('nova');
    expect(stripPersona('office-reviewer')).toBe('office-reviewer');
    expect(stripPersona('')).toBe('');
    expect(stripPersona(undefined)).toBe('');
    expect(stripPersona(null)).toBe('');
  });
});

describe('clampMaxWorkers', () => {
  it('clamps to 1..4 and defaults missing config to the kernel default (2)', () => {
    expect(clampMaxWorkers(4)).toBe(4);
    expect(clampMaxWorkers(1)).toBe(1);
    expect(clampMaxWorkers(9)).toBe(4);
    expect(clampMaxWorkers(0)).toBe(1);
    expect(clampMaxWorkers(undefined)).toBe(2);
    expect(clampMaxWorkers(null)).toBe(2);
  });
});

function proj(tasks: OfficeProject['tasks'], maxWorkers?: number): OfficeProject {
  return { tasks, config: maxWorkers === undefined ? undefined : { maxWorkers } };
}
function forPersona(presence: ReturnType<typeof presenceFor>, name: string) {
  return presence.find((p) => p.persona === name)!;
}

describe('presenceFor', () => {
  it('returns exactly one entry per pool persona, idle by default', () => {
    const presence = presenceFor(proj([]));
    expect(presence).toHaveLength(PERSONA_ORDER.length);
    expect(presence.every((p) => p.status === 'idle')).toBe(true);
    expect(occupiedCount(presence)).toBe(0);
  });

  it('derives working (onprogress) with a live monitor within capacity', () => {
    const presence = presenceFor(
      proj([{ id: 'p/t1', title: 'do it', state: 'onprogress', priority: 5, persona: 'nova' }], 4),
    );
    const nova = forPersona(presence, 'nova');
    expect(nova.status).toBe('working');
    expect(nova.taskId).toBe('p/t1');
    expect(nova.monitorActive).toBe(true);
    expect(nova.waitingForChair).toBe(false);
    expect(occupiedCount(presence)).toBe(1);
  });

  it('derives review-debate (review) and parked, with dark monitors', () => {
    const presence = presenceFor(
      proj(
        [
          { id: 'p/r', title: 'reviewing', state: 'review', priority: 6, persona: 'tetsuo' },
          { id: 'p/k', title: 'stuck', state: 'parked', priority: 9, persona: 'bob' },
        ],
        4,
      ),
    );
    const tetsuo = forPersona(presence, 'tetsuo');
    expect(tetsuo.status).toBe('review-debate');
    expect(tetsuo.monitorActive).toBe(false);
    expect(tetsuo.waitingForChair).toBe(false);

    const bob = forPersona(presence, 'bob');
    expect(bob.status).toBe('parked');
    expect(bob.monitorActive).toBe(false);
    expect(occupiedCount(presence)).toBe(2);
  });

  it('marks working personas beyond blink capacity as waiting-for-a-chair', () => {
    // maxWorkers 1 -> only the top-priority worker keeps a live monitor; the rest wait.
    const presence = presenceFor(
      proj(
        [
          { id: 'p/a', state: 'onprogress', priority: 1, persona: 'tetsuo' },
          { id: 'p/b', state: 'onprogress', priority: 9, persona: 'nova' },
          { id: 'p/c', state: 'onprogress', priority: 5, persona: 'mika' },
        ],
        1,
      ),
    );
    const nova = forPersona(presence, 'nova'); // highest priority -> the one chair
    expect(nova.monitorActive).toBe(true);
    expect(nova.waitingForChair).toBe(false);

    for (const name of ['mika', 'tetsuo']) {
      const p = forPersona(presence, name);
      expect(p.status).toBe('working');
      expect(p.monitorActive).toBe(false);
      expect(p.waitingForChair).toBe(true);
    }
  });

  it('normalizes a full office-worker- binding persona to the short pool name', () => {
    const presence = presenceFor(
      proj([{ id: 'p/t', state: 'onprogress', priority: 5, persona: 'office-worker-vera' }], 4),
    );
    const vera = forPersona(presence, 'vera');
    expect(vera.status).toBe('working');
    expect(vera.taskId).toBe('p/t');
  });

  it('ignores tasks whose state does not occupy a desk (todo/done/backlog)', () => {
    const presence = presenceFor(
      proj([
        { id: 'p/d', state: 'done', priority: 5, persona: 'nova' },
        { id: 'p/t', state: 'todo', priority: 5, persona: 'mika' },
      ], 4),
    );
    expect(occupiedCount(presence)).toBe(0);
    expect(forPersona(presence, 'nova').status).toBe('idle');
  });

  it('prefers the most-active task when two hash to one persona', () => {
    const presence = presenceFor(
      proj([
        { id: 'p/parked', state: 'parked', priority: 9, persona: 'koji' },
        { id: 'p/live', state: 'onprogress', priority: 1, persona: 'koji' },
      ], 4),
    );
    const koji = forPersona(presence, 'koji');
    expect(koji.status).toBe('working'); // onprogress outranks parked
    expect(koji.taskId).toBe('p/live');
  });
});

describe('fixed-staff liveness', () => {
  it('reads researchActive/auditActive booleans or a raw binding object', () => {
    expect(isResearchLive({ researchActive: true })).toBe(true);
    expect(isResearchLive({ research: { extAgentId: 1 } })).toBe(true);
    expect(isResearchLive({})).toBe(false);
    expect(isAuditLive({ auditActive: true })).toBe(true);
    expect(isAuditLive({ audit: { extAgentId: 2 } })).toBe(true);
    expect(isAuditLive({})).toBe(false);
  });
});

describe('formatElapsed', () => {
  it('formats sub-minute durations as "Ns"', () => {
    expect(formatElapsed(1000, 1000)).toBe('0s');
    expect(formatElapsed(46000, 1000)).toBe('45s');
    expect(formatElapsed(60000, 1000)).toBe('59s');
  });

  it('formats minute-plus durations as "m:ss"', () => {
    expect(formatElapsed(61000, 1000)).toBe('1:00');
    expect(formatElapsed(126000, 1000)).toBe('2:05');
  });

  it('clamps to "0s" when nowMs is before sinceMs (never goes negative)', () => {
    expect(formatElapsed(1000, 5000)).toBe('0s');
  });
});

describe('isDraftingFamilyActivity', () => {
  it('classifies drafting-family labels as true, research/audit/waiting as false', () => {
    expect(isDraftingFamilyActivity('drafting the TRD')).toBe(true);
    expect(isDraftingFamilyActivity('researching the stack')).toBe(false);
    expect(isDraftingFamilyActivity('auditing the delivery')).toBe(false);
    // The waiting-on-user state is not live work, so it is not a drafting-family activity.
    expect(isDraftingFamilyActivity('waiting on you — 2 assumptions')).toBe(false);
    expect(isDraftingFamilyActivity(undefined)).toBe(false);
    expect(isDraftingFamilyActivity(null)).toBe(false);
  });
});

describe('isWaitingOnUserActivity', () => {
  it('prefix-matches the waiting-on-user label regardless of the assumption count', () => {
    expect(isWaitingOnUserActivity('waiting on you — 1 assumption')).toBe(true);
    expect(isWaitingOnUserActivity('waiting on you — 7 assumptions')).toBe(true);
    expect(isWaitingOnUserActivity('drafting the TRD')).toBe(false);
    expect(isWaitingOnUserActivity('researching the stack')).toBe(false);
    expect(isWaitingOnUserActivity(undefined)).toBe(false);
    expect(isWaitingOnUserActivity(null)).toBe(false);
  });
});

// ---------------------------------------------------------------------------
// Sprint-review meeting room (feature: sprints)
// ---------------------------------------------------------------------------

function sprint(over: Partial<OfficeSprint>): OfficeSprint {
  return { index: 0, goal: 'Foundation', status: 'active', total: 1, done: 0, tasks: [], ...over };
}

describe('isSprintReview / reviewedSprint / sprintBadgeText', () => {
  it('is false/null on a pre-sprints (no activeSprint) snapshot — back-compat', () => {
    expect(isSprintReview({})).toBe(false);
    expect(reviewedSprint({})).toBeNull();
    expect(sprintBadgeText({})).toBeNull();
  });

  it('is false while the current sprint is merely active (not in review)', () => {
    const p: OfficeProject = {
      sprints: [sprint({ status: 'active' })],
      activeSprint: { index: 0, count: 2, goal: 'Foundation', total: 3, done: 1, inReview: false },
    };
    expect(isSprintReview(p)).toBe(false);
    expect(reviewedSprint(p)).toBeNull();
    expect(sprintBadgeText(p)).toBe('sprint 1/2 — Foundation');
  });

  it('is true and resolves the reviewed sprint (with transcript) while inReview', () => {
    const reviewed = sprint({
      index: 1,
      goal: 'Second',
      status: 'inreview',
      transcript: [{ speaker: 'nova', text: 'shipped the client' }],
    });
    const p: OfficeProject = {
      sprints: [sprint({ status: 'done' }), reviewed],
      activeSprint: { index: 1, count: 2, goal: 'Second', total: 2, done: 2, inReview: true },
    };
    expect(isSprintReview(p)).toBe(true);
    expect(reviewedSprint(p)).toBe(reviewed);
    expect(sprintBadgeText(p)).toBe('sprint 2/2 — Second');
  });
});

describe('sprintAttendees', () => {
  it('returns null-safe empty for no sprint', () => {
    expect(sprintAttendees({ tasks: [] }, null)).toEqual([]);
  });

  it('resolves the sprint tasks’ personas, PERSONA_ORDER-stable, via the live task binding', () => {
    const p: OfficeProject = {
      tasks: [
        { id: 't1', state: 'done', persona: 'office-worker-mika' },
        { id: 't2', state: 'done', persona: 'nova' },
        { id: 't3', state: 'done', persona: 'nova' }, // same persona as t2 — de-duped
        { id: 'other', state: 'onprogress', persona: 'bob' }, // not in this sprint — excluded
      ],
    };
    const s = sprint({ tasks: ['t1', 't2', 't3'] });
    // PERSONA_ORDER is ['nova', 'mika', ...] — nova sorts before mika regardless of task order.
    expect(sprintAttendees(p, s)).toEqual(['nova', 'mika']);
  });
});

describe('meetingSeatsFor', () => {
  it('anchors the PM and reviewer at the table head, deterministically', () => {
    const seats = meetingSeatsFor([]);
    expect(seats).toEqual([
      { role: 'pm', x: TABLE.padX + TABLE.seatW, y: TABLE.padY },
      { role: 'reviewer', x: TABLE.padX + TABLE.seatW * 2, y: TABLE.padY },
    ]);
  });

  it('rings one seat per attendee, wrapping rows at TABLE.cols', () => {
    const seats = meetingSeatsFor(['nova', 'mika', 'tetsuo', 'bob']);
    const workers = seats.filter((s) => s.role === 'worker');
    expect(workers).toHaveLength(4);
    expect(workers[0]).toEqual({ role: 'worker', persona: 'nova', x: TABLE.padX, y: TABLE.padY + TABLE.seatH });
    // 4th attendee (index 3) wraps to the second row (cols = 3).
    expect(workers[3]).toEqual({ role: 'worker', persona: 'bob', x: TABLE.padX, y: TABLE.padY + TABLE.seatH * 2 });
  });
});

// ---------------------------------------------------------------------------
// Ambient idle life (feature: ambient-idle-life)
// ---------------------------------------------------------------------------

describe('idlePersonasFor', () => {
  it('returns only idle personas, excluding any given (e.g. meeting attendees)', () => {
    const presence = presenceFor(
      proj([
        { id: 'p/a', state: 'onprogress', priority: 5, persona: 'nova' }, // occupied, not idle
      ]),
    );
    const idle = idlePersonasFor(presence);
    expect(idle).not.toContain('nova');
    expect(idle).toContain('mika');
    expect(idle).toHaveLength(PERSONA_ORDER.length - 1);
  });

  it('excludes meeting attendees even though presenceFor reads them as idle (their sprint tasks are Done)', () => {
    const presence = presenceFor(proj([{ id: 'p/a', state: 'done', priority: 5, persona: 'nova' }]));
    // nova reads idle here (done isn't an occupied-desk state) — but during a review nova is
    // seated at the meeting table, so must be excluded from the idle-life pool.
    expect(forPersona(presence, 'nova').status).toBe('idle');
    const idle = idlePersonasFor(presence, ['nova']);
    expect(idle).not.toContain('nova');
  });
});

describe('idleAssignmentsFor', () => {
  const idle = ['bob', 'dax', 'ines', 'koji'];

  it('is a pure/deterministic function of (idlePersonas, tick) — same input, same output', () => {
    expect(idleAssignmentsFor(idle, 5)).toEqual(idleAssignmentsFor(idle, 5));
    expect(idleAssignmentsFor(idle, 5)).not.toEqual(idleAssignmentsFor(idle, 200)); // different window
  });

  it('returns exactly one assignment per idle persona, each a recognized activity', () => {
    const assignments = idleAssignmentsFor(idle, 0);
    expect(assignments).toHaveLength(idle.length);
    expect(assignments.map((a) => a.persona).sort()).toEqual([...idle].sort());
    for (const a of assignments) {
      expect(['wander', 'gossip', 'cooler']).toContain(a.activity);
      expect(a.t).toBeGreaterThanOrEqual(0);
      expect(a.t).toBeLessThan(1);
    }
  });

  it('gossip pairs point back at each other', () => {
    // Sweep a range of ticks/persona sets until a gossip pair turns up (deterministic hash, so
    // this always finds one — no flakiness).
    for (let tick = 0; tick < 400; tick += 40) {
      const assignments = idleAssignmentsFor(idle, tick);
      const gossiping = assignments.filter((a) => a.activity === 'gossip');
      if (gossiping.length > 0) {
        for (const a of gossiping) {
          const partner = assignments.find((x) => x.persona === a.partner);
          expect(partner).toBeDefined();
          expect(partner!.activity).toBe('gossip');
          expect(partner!.partner).toBe(a.persona);
        }
        return;
      }
    }
    throw new Error('expected at least one gossip pair across the swept ticks');
  });

  it('never assigns an idle persona that was not passed in (structural: meeting/desk exclusion is the caller’s job)', () => {
    const assignments = idleAssignmentsFor(idle, 12);
    for (const a of assignments) {
      expect(idle).toContain(a.persona);
    }
  });
});
