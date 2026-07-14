import { describe, it, expect } from 'vitest';
import { buildDrilldownTree } from './Drilldown';
import type { Task } from '../components/Card';

function task(id: string, state: Task['state']): Task {
  return { id, title: id, column: 'todo', state, priority: 0, blockedBy: [], bounces: 0 };
}

describe('buildDrilldownTree', () => {
  it('falls back to a flat ungrouped bucket when epics/stories are absent', () => {
    const tree = buildDrilldownTree({ tasks: [task('t1', 'todo'), task('t2', 'done')] });
    expect(tree.epics).toEqual([]);
    expect(tree.ungrouped.map((t) => t.id)).toEqual(['t1', 't2']);
    expect(tree.done).toBe(1);
    expect(tree.total).toBe(2);
  });

  it('nests tasks under story under epic when the data is present', () => {
    const tree = buildDrilldownTree({
      tasks: [task('t1', 'done'), task('t2', 'todo')],
      epics: [{ id: 'e1', title: 'Epic One', stories: ['s1'] }],
      stories: [{ id: 's1', title: 'Story One', tasks: ['t1', 't2'] }],
    });
    expect(tree.epics).toHaveLength(1);
    expect(tree.epics[0].stories).toHaveLength(1);
    expect(tree.epics[0].stories[0].tasks.map((t) => t.id)).toEqual(['t1', 't2']);
    expect(tree.epics[0].stories[0].done).toBe(1);
    expect(tree.epics[0].stories[0].total).toBe(2);
    expect(tree.epics[0].done).toBe(1);
    expect(tree.epics[0].total).toBe(2);
    expect(tree.ungrouped).toEqual([]);
  });

  it('puts tasks not referenced by any story into ungrouped even when epics exist', () => {
    const tree = buildDrilldownTree({
      tasks: [task('t1', 'todo'), task('orphan', 'todo')],
      epics: [{ id: 'e1', title: 'Epic One', stories: ['s1'] }],
      stories: [{ id: 's1', title: 'Story One', tasks: ['t1'] }],
    });
    expect(tree.ungrouped.map((t) => t.id)).toEqual(['orphan']);
  });

  it('tolerates a dangling story reference (story id not in stories[])', () => {
    const tree = buildDrilldownTree({
      tasks: [task('t1', 'todo')],
      epics: [{ id: 'e1', title: 'Epic One', stories: ['ghost'] }],
      stories: [],
    });
    expect(tree.epics[0].stories[0].tasks).toEqual([]);
    expect(tree.epics[0].stories[0].total).toBe(0);
    expect(tree.ungrouped.map((t) => t.id)).toEqual(['t1']);
  });

  it('handles an empty project', () => {
    const tree = buildDrilldownTree({ tasks: [] });
    expect(tree.total).toBe(0);
    expect(tree.done).toBe(0);
    expect(tree.epics).toEqual([]);
    expect(tree.ungrouped).toEqual([]);
  });
});
