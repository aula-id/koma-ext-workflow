import { describe, it, expect } from 'vitest';
import { docCards, type DocCardProject } from './docCards';

function project(overrides: Partial<DocCardProject> = {}): DocCardProject {
  return {
    phase: { kind: 'drafting' },
    prdMarkdown: '',
    trdMarkdown: '',
    researchNotes: '',
    crdMarkdown: '',
    pendingAssumptions: [],
    lastAuditGrade: null,
    officeActivity: null,
    researchActive: false,
    auditActive: false,
    ...overrides,
  };
}

function find(cards: ReturnType<typeof docCards>, key: string) {
  return cards.find((c) => c.key === key);
}

describe('docCards — phase gating', () => {
  it('returns nothing for a null/undefined project', () => {
    expect(docCards(null)).toEqual([]);
    expect(docCards(undefined)).toEqual([]);
  });

  it('renders no cards outside drafting/running (ready/interrupted/halted/done)', () => {
    for (const kind of ['ready', 'interrupted', 'halted', 'done']) {
      const cards = docCards(project({ phase: { kind }, auditActive: true, lastAuditGrade: 90 }));
      expect(cards).toEqual([]);
    }
  });

  it('renders prd/research/trd/crd while drafting, never audit', () => {
    const cards = docCards(project());
    expect(cards.map((c) => c.key)).toEqual(['prd', 'research', 'trd', 'crd']);
  });

  it('never renders the drafting docs while running', () => {
    const cards = docCards(project({ phase: { kind: 'running' } }));
    expect(cards.find((c) => c.key !== 'audit')).toBeUndefined();
  });
});

describe('docCards — everything untouched (all backlog/pending, blockedBy chain)', () => {
  it('chains blockedBy prd -> research -> trd -> crd', () => {
    const cards = docCards(project());
    expect(find(cards, 'prd')).toMatchObject({ column: 'backlog', state: 'pending', blockedBy: [] });
    expect(find(cards, 'research')).toMatchObject({ column: 'backlog', state: 'pending', blockedBy: ['prd'] });
    expect(find(cards, 'trd')).toMatchObject({ column: 'backlog', state: 'pending', blockedBy: ['prd', 'research'] });
    expect(find(cards, 'crd')).toMatchObject({ column: 'backlog', state: 'pending', blockedBy: ['prd', 'research', 'trd'] });
  });
});

describe('docCards — done via non-empty markdown', () => {
  it('marks prd/trd/crd done when their markdown is non-empty', () => {
    const cards = docCards(project({ prdMarkdown: '# PRD', trdMarkdown: '# TRD', crdMarkdown: '# CRD' }));
    expect(find(cards, 'prd')).toMatchObject({ column: 'done', state: 'done' });
    expect(find(cards, 'trd')).toMatchObject({ column: 'done', state: 'done' });
    expect(find(cards, 'crd')).toMatchObject({ column: 'done', state: 'done' });
  });

  it('marks research done when researchNotes is non-empty', () => {
    const cards = docCards(project({ researchNotes: '- fastify is fine' }));
    expect(find(cards, 'research')).toMatchObject({ column: 'done', state: 'done' });
  });

  it('unblocks a downstream doc once its predecessor is done', () => {
    const cards = docCards(project({ prdMarkdown: '# PRD' }));
    expect(find(cards, 'research')).toMatchObject({ blockedBy: [] });
    expect(find(cards, 'trd')).toMatchObject({ blockedBy: ['research'] });
  });

  it('whitespace-only markdown does not count as done', () => {
    const cards = docCards(project({ prdMarkdown: '   \n  ' }));
    expect(find(cards, 'prd')).toMatchObject({ column: 'backlog', state: 'pending' });
  });
});

describe('docCards — research skipped', () => {
  it('marks research done+skipped when TRD exists but researchNotes is empty', () => {
    const cards = docCards(project({ prdMarkdown: '# PRD', trdMarkdown: '# TRD' }));
    expect(find(cards, 'research')).toMatchObject({ column: 'done', state: 'skipped', detail: 'skipped' });
  });

  it('marks research done+skipped when only CRD exists (research + TRD both skipped ahead)', () => {
    const cards = docCards(project({ prdMarkdown: '# PRD', crdMarkdown: '# CRD' }));
    expect(find(cards, 'research')).toMatchObject({ column: 'done', state: 'skipped', detail: 'skipped' });
  });

  it('a skipped research unblocks TRD same as a done one', () => {
    const cards = docCards(project({ prdMarkdown: '# PRD', trdMarkdown: '# TRD' }));
    // trd itself is done (non-empty markdown); crd is unblocked once research(skipped)+trd(done) resolve
    expect(find(cards, 'crd')).toMatchObject({ blockedBy: [] });
  });
});

describe('docCards — active via officeActivity', () => {
  it('drafting the TRD activates trd', () => {
    const cards = docCards(project({ prdMarkdown: '# PRD', officeActivity: { label: 'drafting the TRD', sinceMs: 1 } }));
    expect(find(cards, 'trd')).toMatchObject({ column: 'onprogress', state: 'active' });
  });

  it('drafting the CRD activates crd', () => {
    const cards = docCards(project({ trdMarkdown: '# TRD', officeActivity: { label: 'drafting the CRD', sinceMs: 1 } }));
    expect(find(cards, 'crd')).toMatchObject({ column: 'onprogress', state: 'active' });
  });

  it('researching the stack activates research', () => {
    const cards = docCards(project({ prdMarkdown: '# PRD', officeActivity: { label: 'researching the stack', sinceMs: 1 } }));
    expect(find(cards, 'research')).toMatchObject({ column: 'onprogress', state: 'active' });
  });

  it('researchActive flag alone (no matching label) also activates research', () => {
    const cards = docCards(project({ researchActive: true }));
    expect(find(cards, 'research')).toMatchObject({ column: 'onprogress', state: 'active' });
  });

  it('"office is replying" activates prd only while prd is empty', () => {
    const empty = docCards(project({ officeActivity: { label: 'office is replying', sinceMs: 1 } }));
    expect(find(empty, 'prd')).toMatchObject({ column: 'onprogress', state: 'active' });

    const already = docCards(project({ prdMarkdown: '# PRD', officeActivity: { label: 'office is replying', sinceMs: 1 } }));
    expect(find(already, 'prd')).toMatchObject({ column: 'done', state: 'done' });
  });

  it('"breaking down the plan" activates nothing — crd stays done from its markdown', () => {
    const cards = docCards(
      project({
        prdMarkdown: '# PRD',
        trdMarkdown: '# TRD',
        crdMarkdown: '# CRD',
        officeActivity: { label: 'breaking down the plan', sinceMs: 1 },
      }),
    );
    expect(find(cards, 'prd')).toMatchObject({ state: 'done' });
    expect(find(cards, 'trd')).toMatchObject({ state: 'done' });
    expect(find(cards, 'crd')).toMatchObject({ state: 'done' });
    expect(cards.some((c) => c.state === 'active')).toBe(false);
  });
});

describe('docCards — checking via fact-check activity', () => {
  it('fact-checking the PRD -> prd review/checking, even though prd markdown already exists', () => {
    const cards = docCards(project({ prdMarkdown: '# PRD', officeActivity: { label: 'fact-checking the PRD', sinceMs: 1 } }));
    expect(find(cards, 'prd')).toMatchObject({ column: 'review', state: 'checking', detail: 'fact-checking' });
  });

  it('fact-checking the TRD -> trd review/checking', () => {
    const cards = docCards(project({ trdMarkdown: '# TRD', officeActivity: { label: 'fact-checking the TRD', sinceMs: 1 } }));
    expect(find(cards, 'trd')).toMatchObject({ column: 'review', state: 'checking', detail: 'fact-checking' });
  });

  it('fact-checking the CRD -> crd review/checking', () => {
    const cards = docCards(project({ crdMarkdown: '# CRD', officeActivity: { label: 'fact-checking the CRD', sinceMs: 1 } }));
    expect(find(cards, 'crd')).toMatchObject({ column: 'review', state: 'checking', detail: 'fact-checking' });
  });
});

describe('docCards — pendingAssumptions targets the newest non-empty doc', () => {
  it('targets crd when crd is the newest non-empty doc', () => {
    const cards = docCards(
      project({
        prdMarkdown: '# PRD',
        trdMarkdown: '# TRD',
        crdMarkdown: '# CRD',
        pendingAssumptions: ['assumed X', 'assumed Y'],
      }),
    );
    expect(find(cards, 'crd')).toMatchObject({ column: 'review', state: 'assumptions', detail: '2 assumptions pending' });
    expect(find(cards, 'trd')).toMatchObject({ state: 'done' });
    expect(find(cards, 'prd')).toMatchObject({ state: 'done' });
  });

  it('targets trd when trd is the newest non-empty doc (no crd yet)', () => {
    const cards = docCards(
      project({ prdMarkdown: '# PRD', trdMarkdown: '# TRD', pendingAssumptions: ['assumed X'] }),
    );
    expect(find(cards, 'trd')).toMatchObject({ column: 'review', state: 'assumptions', detail: '1 assumption pending' });
    expect(find(cards, 'crd')).toMatchObject({ state: 'pending' });
  });

  it('targets prd when prd is the only non-empty doc', () => {
    const cards = docCards(project({ prdMarkdown: '# PRD', pendingAssumptions: ['assumed X'] }));
    expect(find(cards, 'prd')).toMatchObject({ column: 'review', state: 'assumptions', detail: '1 assumption pending' });
  });

  it('targets nothing when every doc is empty (no non-empty doc to flag)', () => {
    const cards = docCards(project({ pendingAssumptions: ['assumed X'] }));
    expect(cards.some((c) => c.state === 'assumptions')).toBe(false);
    expect(find(cards, 'prd')).toMatchObject({ state: 'pending' });
  });

  it('assumptions takes priority over the plain done state for its target', () => {
    const cards = docCards(project({ prdMarkdown: '# PRD', pendingAssumptions: ['assumed X'] }));
    expect(find(cards, 'prd')?.state).toBe('assumptions');
  });
});

describe('docCards — audit card', () => {
  it('renders active/review "auditing" while auditActive, running phase', () => {
    const cards = docCards(project({ phase: { kind: 'running' }, auditActive: true }));
    expect(find(cards, 'audit')).toMatchObject({ column: 'review', state: 'active', detail: 'auditing' });
  });

  it('renders done/"grade NN" once graded and no longer active', () => {
    const cards = docCards(project({ phase: { kind: 'running' }, lastAuditGrade: 93 }));
    expect(find(cards, 'audit')).toMatchObject({ column: 'done', state: 'done', detail: 'grade 93' });
  });

  it('auditActive takes priority over an existing grade', () => {
    const cards = docCards(project({ phase: { kind: 'running' }, auditActive: true, lastAuditGrade: 80 }));
    expect(find(cards, 'audit')).toMatchObject({ state: 'active', detail: 'auditing' });
  });

  it('renders no audit card when neither auditActive nor a grade is present', () => {
    const cards = docCards(project({ phase: { kind: 'running' } }));
    expect(find(cards, 'audit')).toBeUndefined();
  });

  it('never renders the audit card during drafting even if auditActive/graded', () => {
    const cards = docCards(project({ phase: { kind: 'drafting' }, auditActive: true, lastAuditGrade: 90 }));
    expect(find(cards, 'audit')).toBeUndefined();
  });
});

describe('docCards — blockedBy is empty for every non-pending state', () => {
  it('active/checking/assumptions/done/skipped cards never carry blockedBy', () => {
    const cards = docCards(
      project({
        prdMarkdown: '# PRD',
        trdMarkdown: '# TRD',
        officeActivity: { label: 'drafting the CRD', sinceMs: 1 },
      }),
    );
    for (const c of cards) {
      if (c.state !== 'pending') expect(c.blockedBy).toEqual([]);
    }
  });
});
