import React, { useMemo } from 'react';
import { TaskStateKey } from './Card';

export interface DepTask {
  id: string;
  title: string;
  state: TaskStateKey;
  blockedBy: string[];
}

export interface LayoutNode {
  id: string;
  title: string;
  state: TaskStateKey;
  lane: number;
  slot: number;
  x: number;
  y: number;
}

export interface LayoutEdge {
  from: string;
  to: string;
}

export interface DagLayout {
  nodes: LayoutNode[];
  edges: LayoutEdge[];
  laneCount: number;
  maxSlot: number;
}

export const LANE_WIDTH = 200;
export const ROW_HEIGHT = 70;
export const MARGIN_X = 90;
export const MARGIN_Y = 40;
export const NODE_W = 150;
export const NODE_H = 44;

/**
 * Topo-sorted lane layout: lane(t) = 1 + max(lane(dep)) over present blockers, 0 for
 * roots. Deterministic (tasks sorted by id before assignment); tolerant of cycles in
 * malformed data (back-edge treated as lane 0, mirrors graph.rs's tolerant DFS) so a
 * bad snapshot never infinite-loops the panel.
 */
export function layoutDag(tasks: DepTask[]): DagLayout {
  const byId = new Map(tasks.map((t) => [t.id, t]));
  const laneMemo = new Map<string, number>();
  const visiting = new Set<string>();

  function laneOf(id: string): number {
    const cached = laneMemo.get(id);
    if (cached !== undefined) return cached;
    const task = byId.get(id);
    if (!task) return 0;
    if (visiting.has(id)) {
      // cycle guard: don't recurse further on the back-edge
      return 0;
    }
    visiting.add(id);
    let lane = 0;
    for (const dep of task.blockedBy) {
      if (byId.has(dep)) {
        lane = Math.max(lane, laneOf(dep) + 1);
      }
    }
    visiting.delete(id);
    laneMemo.set(id, lane);
    return lane;
  }

  const sorted = [...tasks].sort((a, b) => (a.id < b.id ? -1 : a.id > b.id ? 1 : 0));
  const laneBuckets = new Map<number, string[]>();
  let laneCount = 0;
  for (const t of sorted) {
    const lane = laneOf(t.id);
    laneCount = Math.max(laneCount, lane + 1);
    const bucket = laneBuckets.get(lane) ?? [];
    bucket.push(t.id);
    laneBuckets.set(lane, bucket);
  }

  const nodes: LayoutNode[] = [];
  let maxSlot = 0;
  for (const [lane, ids] of laneBuckets.entries()) {
    ids.forEach((id, slot) => {
      const task = byId.get(id)!;
      maxSlot = Math.max(maxSlot, slot);
      nodes.push({
        id,
        title: task.title,
        state: task.state,
        lane,
        slot,
        x: MARGIN_X + lane * LANE_WIDTH,
        y: MARGIN_Y + slot * ROW_HEIGHT,
      });
    });
  }

  const edges: LayoutEdge[] = [];
  for (const t of tasks) {
    for (const dep of t.blockedBy) {
      if (byId.has(dep)) {
        edges.push({ from: dep, to: t.id });
      }
    }
  }

  return { nodes, edges, laneCount, maxSlot };
}

export interface HaltCulprits {
  poisoned: Set<string>;
  parkedRoots: Set<string>;
}

/**
 * Mirrors office-core graph.rs `line_is_stuck`'s poisoning DFS: a task is poisoned if
 * it is itself Parked, or transitively `blockedBy` a poisoned task. Used to paint the
 * halt-culprit path red on the map. This is purely a client-side view computation
 * (does not decide whether the project IS halted — the caller gates styling on
 * `project.phase.kind === "halted"`).
 */
export function computeHaltCulprits(tasks: DepTask[]): HaltCulprits {
  const byId = new Map(tasks.map((t) => [t.id, t]));
  const poisoned = new Map<string, boolean>();
  const visiting = new Set<string>();

  function poison(id: string): boolean {
    const cached = poisoned.get(id);
    if (cached !== undefined) return cached;
    const task = byId.get(id);
    if (!task) return false;
    if (visiting.has(id)) return false;
    visiting.add(id);
    const result = task.state === 'parked' ? true : task.blockedBy.some((dep) => byId.has(dep) && poison(dep));
    visiting.delete(id);
    poisoned.set(id, result);
    return result;
  }

  const parkedRoots = new Set<string>();
  for (const t of tasks) {
    if (t.state !== 'done') poison(t.id);
    if (t.state === 'parked') parkedRoots.add(t.id);
  }

  const poisonedSet = new Set<string>();
  poisoned.forEach((v, k) => {
    if (v) poisonedSet.add(k);
  });

  return { poisoned: poisonedSet, parkedRoots };
}

function nodeFill(state: TaskStateKey): string {
  switch (state) {
    case 'parked':
      return 'var(--wf-status-parked)';
    case 'onprogress':
      return 'var(--wf-status-running)';
    case 'review':
      return 'var(--wf-accent-purple)';
    case 'done':
      return 'var(--wf-status-done)';
    default:
      return 'var(--wf-bg-secondary)';
  }
}

export interface DepMapProps {
  tasks: DepTask[];
  halted?: boolean;
}

export const DepMap: React.FC<DepMapProps> = ({ tasks, halted = false }) => {
  const layout = useMemo(() => layoutDag(tasks), [tasks]);
  const culprits = useMemo(() => computeHaltCulprits(tasks), [tasks]);

  const width = MARGIN_X * 2 + layout.laneCount * LANE_WIDTH;
  const height = MARGIN_Y * 2 + (layout.maxSlot + 1) * ROW_HEIGHT;

  const nodeById = new Map(layout.nodes.map((n) => [n.id, n]));

  if (tasks.length === 0) {
    return (
      <div style={{ padding: '1.5rem', color: 'var(--wf-fg-secondary)', fontSize: '0.85rem' }}>
        No tasks yet — the dependency map fills in once the office breaks down the PRD.
      </div>
    );
  }

  return (
    <div style={{ overflow: 'auto', background: 'var(--wf-bg)', borderRadius: 'var(--wf-radius)' }}>
      <svg width={Math.max(width, 320)} height={Math.max(height, 160)} role="img" aria-label="dependency map">
        <defs>
          <marker id="wf-dep-arrow" markerWidth="8" markerHeight="8" refX="7" refY="4" orient="auto">
            <path d="M0,0 L8,4 L0,8 z" fill="var(--wf-fg-secondary)" />
          </marker>
          <marker id="wf-dep-arrow-red" markerWidth="8" markerHeight="8" refX="7" refY="4" orient="auto">
            <path d="M0,0 L8,4 L0,8 z" fill="var(--wf-accent-pink)" />
          </marker>
        </defs>

        {layout.edges.map((e) => {
          const from = nodeById.get(e.from);
          const to = nodeById.get(e.to);
          if (!from || !to) return null;
          const red = halted && culprits.poisoned.has(e.from) && culprits.poisoned.has(e.to);
          const x1 = from.x + NODE_W;
          const y1 = from.y + NODE_H / 2;
          const x2 = to.x;
          const y2 = to.y + NODE_H / 2;
          const midX = (x1 + x2) / 2;
          return (
            <path
              key={`${e.from}->${e.to}`}
              d={`M${x1},${y1} C${midX},${y1} ${midX},${y2} ${x2},${y2}`}
              fill="none"
              stroke={red ? 'var(--wf-accent-pink)' : 'var(--wf-fg-secondary)'}
              strokeWidth={red ? 2 : 1}
              opacity={red ? 0.9 : 0.5}
              markerEnd={red ? 'url(#wf-dep-arrow-red)' : 'url(#wf-dep-arrow)'}
            />
          );
        })}

        {layout.nodes.map((n) => {
          const isCulprit = halted && culprits.poisoned.has(n.id);
          return (
            <g key={n.id} transform={`translate(${n.x}, ${n.y})`} data-testid="dep-node" data-task-id={n.id}>
              <rect
                width={NODE_W}
                height={NODE_H}
                rx={6}
                fill="var(--wf-bg-secondary)"
                stroke={isCulprit ? 'var(--wf-accent-pink)' : nodeFill(n.state)}
                strokeWidth={isCulprit ? 2 : 1.5}
              />
              <rect x={0} y={0} width={6} height={NODE_H} rx={3} fill={nodeFill(n.state)} />
              <text x={12} y={18} fontSize={11} fontWeight={600} fill="var(--wf-fg)">
                {n.title.length > 20 ? `${n.title.slice(0, 19)}…` : n.title}
              </text>
              <text x={12} y={33} fontSize={9} fill="var(--wf-fg-secondary)">
                {n.id} · {n.state}
              </text>
            </g>
          );
        })}
      </svg>
    </div>
  );
};

export default DepMap;
