import { describe, it, expect } from 'vitest';
import { designStageCards, type DesignStage } from './designStages';

describe('designStageCards', () => {
  it('returns nothing for null/undefined/empty input', () => {
    expect(designStageCards(null)).toEqual([]);
    expect(designStageCards(undefined)).toEqual([]);
    expect(designStageCards([])).toEqual([]);
  });

  it('projects each stage onto its column by status', () => {
    const stages: DesignStage[] = [
      { id: 'triage', label: 'Triage', status: 'done', note: 'project' },
      { id: 'prd', label: 'PRD', status: 'inProgress' },
      { id: 'research', label: 'Research', status: 'todo' },
    ];
    const cards = designStageCards(stages);
    expect(cards).toEqual([
      { key: 'triage', title: 'Triage', column: 'done', status: 'done', note: 'project' },
      { key: 'prd', title: 'PRD', column: 'onprogress', status: 'inProgress', note: undefined },
      { key: 'research', title: 'Research', column: 'todo', status: 'todo', note: undefined },
    ]);
  });

  it('preserves the server-given order within the result', () => {
    const stages: DesignStage[] = [
      { id: 'triage', label: 'Triage', status: 'done' },
      { id: 'prd', label: 'PRD', status: 'done' },
      { id: 'research', label: 'Research', status: 'done' },
      { id: 'trdcrd', label: 'TRD+CRD', status: 'inProgress' },
      { id: 'breakdown', label: 'Breakdown', status: 'todo' },
    ];
    const cards = designStageCards(stages);
    expect(cards.map((c) => c.key)).toEqual(['triage', 'prd', 'research', 'trdcrd', 'breakdown']);
  });
});
