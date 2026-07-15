import React, { useMemo } from 'react';
import {
  ReactFlow,
  Background,
  Controls,
  BackgroundVariant,
  Handle,
  Position,
  type Node,
  type Edge,
  type NodeProps,
} from '@xyflow/react';
import '@xyflow/react/dist/style.css';
import { taskSlug, TaskStateKey } from './Card';

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

function stateColor(state: TaskStateKey): string {
  switch (state) {
    case 'parked':
      return 'var(--wf-status-parked)';
    case 'onprogress':
      return 'var(--wf-status-running)';
    case 'review':
      return 'var(--wf-status-review)';
    case 'done':
      return 'var(--wf-status-done)';
    default:
      return 'var(--wf-dim)';
  }
}

interface WfNodeData extends Record<string, unknown> {
  title: string;
  slug: string;
  state: TaskStateKey;
  culprit: boolean;
  running: boolean;
}

/** Flat koma chip node: truncating HTML (never SVG text soup), status dot + word,
 * slug line with the full id in the tooltip. Click bubbles via onNodeClick. */
const WfTaskNode: React.FC<NodeProps> = ({ data }) => {
  const d = data as WfNodeData;
  const color = stateColor(d.state);
  return (
    <div
      style={{
        width: 190,
        background: 'var(--wf-panel)',
        border: '1px solid var(--wf-border)',
        borderLeft: d.culprit ? '3px solid var(--wf-error)' : `3px solid ${color}`,
        borderRadius: 'var(--wf-radius)',
        padding: '0.45rem 0.6rem',
        cursor: 'pointer',
        fontFamily: 'inherit',
      }}
    >
      {/* invisible anchors — without Handles, React Flow renders no edges at all */}
      <Handle type="target" position={Position.Left} style={{ opacity: 0, width: 1, height: 1, border: 'none', minWidth: 0, minHeight: 0 }} />
      <Handle type="source" position={Position.Right} style={{ opacity: 0, width: 1, height: 1, border: 'none', minWidth: 0, minHeight: 0 }} />
      <div
        style={{
          fontSize: '0.75rem',
          color: 'var(--wf-fg)',
          overflow: 'hidden',
          textOverflow: 'ellipsis',
          whiteSpace: 'nowrap',
        }}
        title={d.title}
      >
        {d.title}
      </div>
      <div
        style={{ display: 'flex', alignItems: 'center', gap: '0.35rem', marginTop: 3, fontSize: '0.62rem', minWidth: 0 }}
      >
        <span className="wf-status-dot" style={{ background: color, width: 5, height: 5, flex: 'none' }} />
        <span style={{ color, flex: 'none' }}>{d.state === 'onprogress' ? 'running' : d.state}</span>
        <span
          style={{ color: 'var(--wf-dim)', overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}
        >
          {d.slug}
        </span>
      </div>
    </div>
  );
};

const NODE_TYPES = { wfTask: WfTaskNode };

export interface DepMapProps {
  tasks: DepTask[];
  halted?: boolean;
  /** Click a node to open its task detail (wired by Board to the drawer). */
  onTaskClick?: (taskId: string) => void;
}

export const DepMap: React.FC<DepMapProps> = ({ tasks, halted = false, onTaskClick }) => {
  const layout = useMemo(() => layoutDag(tasks), [tasks]);
  const culprits = useMemo(() => computeHaltCulprits(tasks), [tasks]);
  const taskById = useMemo(() => new Map(tasks.map((t) => [t.id, t])), [tasks]);

  const nodes: Node[] = useMemo(
    () =>
      layout.nodes.map((n) => {
        const culprit = halted && culprits.poisoned.has(n.id);
        return {
          id: n.id,
          type: 'wfTask',
          position: { x: n.lane * (LANE_WIDTH + 60), y: n.slot * ROW_HEIGHT },
          data: {
            title: n.title,
            slug: taskSlug(n.id),
            state: n.state,
            culprit,
            running: n.state === 'onprogress',
          } satisfies WfNodeData,
          // full id available on hover via the browser-native tooltip route too
          selectable: true,
          draggable: false,
        };
      }),
    [layout, halted, culprits],
  );

  const edges: Edge[] = useMemo(
    () =>
      layout.edges.map((e) => {
        const red =
          halted && culprits.poisoned.has(e.from) && culprits.poisoned.has(e.to);
        const active = taskById.get(e.to)?.state === 'onprogress';
        return {
          id: `${e.from}->${e.to}`,
          source: e.from,
          target: e.to,
          animated: active,
          style: {
            stroke: red ? 'var(--wf-error)' : 'var(--wf-grip)',
            strokeWidth: red ? 2 : 1,
          },
        };
      }),
    [layout, halted, culprits, taskById],
  );

  if (tasks.length === 0) {
    return (
      <div style={{ padding: '1.5rem', color: 'var(--wf-dim)', fontSize: '0.85rem' }}>
        No tasks yet — the dependency map fills in once the office breaks down the PRD.
      </div>
    );
  }

  return (
    <div
      style={{
        height: 'max(420px, calc(100vh - 240px))',
        borderTop: '1px solid var(--wf-border)',
      }}
      data-testid="dep-map"
    >
      <ReactFlow
        nodes={nodes}
        edges={edges}
        nodeTypes={NODE_TYPES}
        fitView
        minZoom={0.2}
        maxZoom={1.6}
        nodesConnectable={false}
        proOptions={{ hideAttribution: true }}
        onNodeClick={(_ev, node) => onTaskClick?.(node.id)}
        style={{ background: 'var(--wf-bg)' }}
      >
        <Background variant={BackgroundVariant.Dots} gap={22} size={1} color="var(--wf-border)" />
        <Controls showInteractive={false} position="bottom-right" />
      </ReactFlow>
    </div>
  );
};

export default DepMap;
