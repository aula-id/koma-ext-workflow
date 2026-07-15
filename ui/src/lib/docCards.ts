/**
 * Drafting-pipeline docs projected onto the kanban board as synthetic cards.
 *
 * PURE, testable derivation (no React, no DOM) — mirrors officeLayout.ts's shape. The
 * drafting pipeline authors four docs in order (PRD -> research -> TRD -> CRD,
 * ARCHITECTURE.md 6.2b/6.2c) before breakdown; this module turns that pipeline's live
 * state (markdown presence, `officeActivity` label, `pendingAssumptions`) into cards the
 * board can render inside its normal backlog/onprogress/review/done columns, and a fifth
 * "audit" card that surfaces the clean-build auditor once the project is running.
 *
 * Field names below are the frozen wire names from docs/PANEL_PROTOCOL.md 2.1 plus the
 * `crdMarkdown`/`pendingAssumptions` CRD-wave additions (verified against Board.tsx's
 * `Project` interface and the office-daemon `InvokePurpose` activity-label constants in
 * crates/office-daemon/src/driver.rs).
 */

export type DocKey = 'prd' | 'research' | 'trd' | 'crd' | 'audit';
export type DocColumn = 'backlog' | 'onprogress' | 'review' | 'done';
export type DocState = 'pending' | 'active' | 'checking' | 'assumptions' | 'done' | 'skipped';

export interface DocCard {
  key: DocKey;
  title: string;
  column: DocColumn;
  state: DocState;
  detail?: string;
  blockedBy: string[];
}

/** Minimal project shape this derivation reads — a subset of Board.tsx's `Project`. */
export interface DocCardProject {
  phase?: { kind?: string } | null;
  prdMarkdown?: string | null;
  trdMarkdown?: string | null;
  researchNotes?: string | null;
  crdMarkdown?: string | null;
  pendingAssumptions?: string[] | null;
  lastAuditGrade?: number | null;
  officeActivity?: { label: string; sinceMs: number } | null;
  researchActive?: boolean;
  auditActive?: boolean;
}

const DOC_TITLES: Record<DocKey, string> = {
  prd: 'PRD — product requirements',
  research: 'Research — web findings',
  trd: 'TRD — technical requirements',
  crd: 'CRD — clean-build requirements',
  audit: 'Clean-build audit',
};

/** The drafting doc chain, in authoring order. */
const CHAIN: Array<'prd' | 'research' | 'trd' | 'crd'> = ['prd', 'research', 'trd', 'crd'];

function nonEmpty(s: string | null | undefined): boolean {
  return typeof s === 'string' && s.trim().length > 0;
}

/** Office-brain activity labels (crates/office-daemon/src/driver.rs `InvokePurpose`
 * display strings) this derivation matches against `officeActivity.label`. */
const LABEL = {
  persona: 'office is replying',
  factCheckPrd: 'fact-checking the PRD',
  factCheckTrd: 'fact-checking the TRD',
  factCheckCrd: 'fact-checking the CRD',
  draftTrd: 'drafting the TRD',
  draftCrd: 'drafting the CRD',
  research: 'researching the stack',
} as const;

function pluralAssumptions(n: number): string {
  return `${n} assumption${n === 1 ? '' : 's'} pending`;
}

/**
 * Derive the synthetic doc cards for a project's current pipeline state.
 *
 * - prd/research/trd/crd only render while `phase.kind === 'drafting'`.
 * - `audit` only renders while `phase.kind === 'running'` AND (`auditActive` or a
 *   non-null `lastAuditGrade`) — the clean-build audit happens after drafting, not during it.
 */
export function docCards(project: DocCardProject | null | undefined): DocCard[] {
  const cards: DocCard[] = [];
  if (!project) return cards;

  const phaseKind = project.phase?.kind;

  if (phaseKind === 'drafting') {
    const prdDone = nonEmpty(project.prdMarkdown);
    const trdDone = nonEmpty(project.trdMarkdown);
    const crdDone = nonEmpty(project.crdMarkdown);
    const researchDone = nonEmpty(project.researchNotes);
    const researchSkipped = !researchDone && (trdDone || crdDone);

    const activityLabel = project.officeActivity?.label;
    const assumptions = project.pendingAssumptions ?? [];
    const hasAssumptions = assumptions.length > 0;

    // The safeguard's no-assume gate flags the doc it just authored — the NEWEST
    // non-empty doc in the chain — while the drafting pipeline waits on the user.
    const assumptionsTarget: 'prd' | 'research' | 'trd' | 'crd' | null = hasAssumptions
      ? crdDone
        ? 'crd'
        : trdDone
          ? 'trd'
          : prdDone
            ? 'prd'
            : null
      : null;

    // resolved = "no longer blocking a downstream doc" (authored, whether normally
    // finished or a legitimately skipped research step).
    const resolved = new Set<string>();

    for (const key of CHAIN) {
      let column: DocColumn;
      let state: DocState;
      let detail: string | undefined;

      if (assumptionsTarget === key) {
        column = 'review';
        state = 'assumptions';
        detail = pluralAssumptions(assumptions.length);
      } else if (
        (key === 'prd' && activityLabel === LABEL.factCheckPrd) ||
        (key === 'trd' && activityLabel === LABEL.factCheckTrd) ||
        (key === 'crd' && activityLabel === LABEL.factCheckCrd)
      ) {
        column = 'review';
        state = 'checking';
        detail = 'fact-checking';
      } else if (
        (key === 'trd' && activityLabel === LABEL.draftTrd) ||
        (key === 'crd' && activityLabel === LABEL.draftCrd) ||
        (key === 'research' && (activityLabel === LABEL.research || Boolean(project.researchActive))) ||
        (key === 'prd' && activityLabel === LABEL.persona && !prdDone)
      ) {
        column = 'onprogress';
        state = 'active';
      } else if (key === 'research' && researchSkipped) {
        column = 'done';
        state = 'skipped';
        detail = 'skipped';
      } else if (
        (key === 'prd' && prdDone) ||
        (key === 'research' && researchDone) ||
        (key === 'trd' && trdDone) ||
        (key === 'crd' && crdDone)
      ) {
        column = 'done';
        state = 'done';
      } else {
        column = 'backlog';
        state = 'pending';
      }

      const blockedBy: string[] = [];
      if (state === 'pending') {
        for (const prev of CHAIN) {
          if (prev === key) break;
          if (!resolved.has(prev)) blockedBy.push(prev);
        }
      }

      if (state === 'done' || state === 'skipped') resolved.add(key);

      cards.push({ key, title: DOC_TITLES[key], column, state, detail, blockedBy });
    }
  }

  if (phaseKind === 'running') {
    const auditActive = Boolean(project.auditActive);
    const grade = project.lastAuditGrade;
    const hasGrade = grade !== null && grade !== undefined;
    if (auditActive) {
      cards.push({ key: 'audit', title: DOC_TITLES.audit, column: 'review', state: 'active', detail: 'auditing', blockedBy: [] });
    } else if (hasGrade) {
      cards.push({ key: 'audit', title: DOC_TITLES.audit, column: 'done', state: 'done', detail: `grade ${grade}`, blockedBy: [] });
    }
  }

  return cards;
}

export default docCards;
