/**
 * Design-stage placeholder cards (feature: design-stage-cards).
 *
 * While a project is pre-Ready (Drafting, or paused mid-Drafting via Interrupted), the
 * real kanban board has no task cards yet — those only exist once a breakdown lands
 * (Drafting -> Ready). office-core's digest.rs emits a `designStages` array on the full
 * snapshot describing the SDLC pipeline's current stage-by-stage progress (triage ->
 * PRD/change-brief -> research -> TRD+CRD -> breakdown, or the patch track's single
 * task) so the board is never empty while that pipeline is in flight.
 *
 * Unlike docCards.ts, there is no client-side derivation logic here — the kernel is the
 * SOLE authority on each stage's status (crates/office-core/src/digest.rs's
 * `design_stages`); this module is a thin, PURE projection of that already-computed
 * server state onto the 3 columns a design stage can occupy.
 */

export type DesignStageStatus = 'todo' | 'inProgress' | 'done';
export type DesignStageColumn = 'todo' | 'onprogress' | 'done';

/** One design-stage entry, per office-core digest.rs's `designStages[]` wire shape. */
export interface DesignStage {
  id: string;
  label: string;
  status: DesignStageStatus;
  note?: string;
}

export interface DesignStageCard {
  key: string;
  title: string;
  column: DesignStageColumn;
  status: DesignStageStatus;
  note?: string;
}

const COLUMN_BY_STATUS: Record<DesignStageStatus, DesignStageColumn> = {
  todo: 'todo',
  inProgress: 'onprogress',
  done: 'done',
};

/**
 * Project the server's `designStages` array onto board columns. `stages` is already
 * ordered server-side (triage first, breakdown/task last); this preserves that order
 * within each column. Absent/empty input (older snapshot, or a Ready+ project where the
 * field is omitted entirely) yields no cards.
 */
export function designStageCards(stages: DesignStage[] | null | undefined): DesignStageCard[] {
  if (!stages || stages.length === 0) return [];
  return stages.map((s) => ({
    key: s.id,
    title: s.label,
    column: COLUMN_BY_STATUS[s.status] ?? 'todo',
    status: s.status,
    note: s.note,
  }));
}

export default designStageCards;
