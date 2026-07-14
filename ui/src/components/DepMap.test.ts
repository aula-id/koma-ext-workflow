import { describe, it, expect } from 'vitest';
import { layoutDag, computeHaltCulprits, DepTask } from './DepMap';

function task(id: string, state: DepTask['state'], blockedBy: string[] = []): DepTask {
  return { id, title: id, state, blockedBy };
}

describe('layoutDag', () => {
  it('places a root task in lane 0', () => {
    const layout = layoutDag([task('a', 'todo')]);
    expect(layout.nodes).toHaveLength(1);
    expect(layout.nodes[0].lane).toBe(0);
    expect(layout.laneCount).toBe(1);
  });

  it('assigns lane = 1 + max(lane(dep)) over present blockers', () => {
    const layout = layoutDag([
      task('a', 'done'),
      task('b', 'done', ['a']),
      task('c', 'todo', ['b']),
    ]);
    const byId = Object.fromEntries(layout.nodes.map((n) => [n.id, n]));
    expect(byId.a.lane).toBe(0);
    expect(byId.b.lane).toBe(1);
    expect(byId.c.lane).toBe(2);
    expect(layout.laneCount).toBe(3);
  });

  it('takes the max lane across multiple blockers (diamond dependency)', () => {
    const layout = layoutDag([
      task('a', 'done'),
      task('b', 'done', ['a']),
      task('c', 'done', ['a']),
      task('d', 'todo', ['b', 'c']),
    ]);
    const byId = Object.fromEntries(layout.nodes.map((n) => [n.id, n]));
    expect(byId.d.lane).toBe(2);
  });

  it('ignores dangling blockedBy references (not in the task set)', () => {
    const layout = layoutDag([task('a', 'todo', ['ghost'])]);
    expect(layout.nodes[0].lane).toBe(0);
    expect(layout.edges).toHaveLength(0);
  });

  it('is deterministic: same input twice produces identical layout', () => {
    const tasks = [
      task('c', 'todo', ['b']),
      task('a', 'done'),
      task('b', 'done', ['a']),
    ];
    const l1 = layoutDag(tasks);
    const l2 = layoutDag(tasks);
    expect(l1).toEqual(l2);
  });

  it('assigns deterministic slots within a lane sorted by id', () => {
    const layout = layoutDag([task('z', 'todo'), task('a', 'todo'), task('m', 'todo')]);
    const lane0 = layout.nodes.filter((n) => n.lane === 0).sort((x, y) => x.slot - y.slot);
    expect(lane0.map((n) => n.id)).toEqual(['a', 'm', 'z']);
  });

  it('tolerates a cycle without infinite looping (back-edge treated as lane 0)', () => {
    const layout = layoutDag([task('a', 'todo', ['b']), task('b', 'todo', ['a'])]);
    expect(layout.nodes).toHaveLength(2);
    expect(layout.laneCount).toBeGreaterThan(0);
  });

  it('builds an edge for every present blocked-by relationship', () => {
    const layout = layoutDag([task('a', 'done'), task('b', 'todo', ['a'])]);
    expect(layout.edges).toEqual([{ from: 'a', to: 'b' }]);
  });
});

describe('computeHaltCulprits', () => {
  it('marks a parked task itself as poisoned', () => {
    const { poisoned, parkedRoots } = computeHaltCulprits([task('a', 'parked')]);
    expect(poisoned.has('a')).toBe(true);
    expect(parkedRoots.has('a')).toBe(true);
  });

  it('marks transitively-blocked tasks poisoned', () => {
    const { poisoned } = computeHaltCulprits([
      task('a', 'parked'),
      task('b', 'todo', ['a']),
      task('c', 'todo', ['b']),
    ]);
    expect(poisoned.has('a')).toBe(true);
    expect(poisoned.has('b')).toBe(true);
    expect(poisoned.has('c')).toBe(true);
  });

  it('does not poison a task with an independent path (only-one-of-many blocked)', () => {
    const { poisoned } = computeHaltCulprits([
      task('a', 'parked'),
      task('b', 'todo'),
      task('c', 'todo', ['a', 'b']),
    ]);
    expect(poisoned.has('b')).toBe(false);
    // c depends on a (poisoned) so c is poisoned via the `some` semantics
    expect(poisoned.has('c')).toBe(true);
  });

  it('never poisons a done task set with no parked tasks', () => {
    const { poisoned, parkedRoots } = computeHaltCulprits([task('a', 'done'), task('b', 'todo', ['a'])]);
    expect(poisoned.size).toBe(0);
    expect(parkedRoots.size).toBe(0);
  });

  it('tolerates a cycle without infinite looping', () => {
    const { poisoned } = computeHaltCulprits([task('a', 'todo', ['b']), task('b', 'todo', ['a'])]);
    expect(poisoned.size).toBeGreaterThanOrEqual(0);
  });
});
