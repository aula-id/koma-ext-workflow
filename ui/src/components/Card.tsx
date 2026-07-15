import React from 'react';
import { motion } from 'framer-motion';

/**
 * Task/project shapes mirror docs/PANEL_PROTOCOL.md section 2.2 (frozen W7 contract)
 * exactly. Note: the frozen protocol does NOT carry a worker agent id on a task (only
 * the daemon's internal AgentBinding does) — running cards show a generic activity
 * pulse instead of "agent id" per the wave spec, with an optional `agentId` field kept
 * for forward compatibility should a future wave add it to the snapshot.
 */
export type ColumnKey = 'backlog' | 'todo' | 'onprogress' | 'review' | 'done';
export type TaskStateKey = 'backlog' | 'todo' | 'onprogress' | 'review' | 'parked' | 'done';

export interface CommentReceipt {
  state: 'pending' | 'delivered' | 'read';
  atMs?: number;
}

export interface TaskComment {
  id: number;
  author: 'user' | 'office' | 'system';
  text: string;
  createdMs: number;
  receipt: CommentReceipt;
}

export interface TaskHistoryEntry {
  atMs: number;
  event: string;
}

export interface Task {
  id: string;
  title: string;
  column: ColumnKey;
  state: TaskStateKey;
  priority: number;
  blockedBy: string[];
  bounces: number;
  agentId?: string | number;
  /** Short worker persona at this task's desk (e.g. `nova`) — office view only (digest.rs
   * full mode; present while the task is in progress / in review / parked). */
  persona?: string;
  description?: string;
  acceptance?: string[];
  comments?: TaskComment[];
  lastReport?: string | null;
  lastReview?: string | null;
  history?: TaskHistoryEntry[];
}

/** Last segment of a hierarchical task id — `<project>/<epic>/<story>/<task-slug>`
 * ids are far too long for chrome; UI shows the slug and tooltips the full id. */
export function taskSlug(id: string): string {
  return id.split('/').pop() || id;
}

export interface CardProps {
  task: Task;
  draggable?: boolean;
  culprit?: boolean;
  onDragStart?: (task: Task, e: React.DragEvent) => void;
  onClick?: (task: Task) => void;
}

/*
 * koma-flat card: the kanban card is the ONE legitimate box in this app
 * (a draggable, contained thing). It stays quiet: panel surface, hairline
 * border, no shadow, no state chip — the column it sits in already names its
 * state. The only per-card signals are the ones the column CANNOT tell you:
 *   - running: pulsing info dot (+ agent id when known)
 *   - parked: warn dot + word (a parked card sits in the Review column)
 *   - bounces: warn count, quiet until it exists
 *   - blocked-by: dim refs
 *   - halt culprit: 2px error left rule, not a full red frame
 */
export const Card: React.FC<CardProps> = ({ task, draggable = false, culprit = false, onDragStart, onClick }) => {
  const parked = task.state === 'parked';
  const running = task.state === 'onprogress';

  return (
    <motion.div
      layout
      initial={{ opacity: 0, y: 6 }}
      animate={{ opacity: 1, y: 0 }}
      exit={{ opacity: 0, y: -6 }}
      transition={{ duration: 0.18 }}
      draggable={draggable}
      onDragStart={(e) => onDragStart?.(task, e as unknown as React.DragEvent)}
      onClick={() => onClick?.(task)}
      style={{
        background: 'var(--wf-panel)',
        borderRadius: 'var(--wf-radius)',
        border: '1px solid var(--wf-border)',
        borderLeft: culprit ? '2px solid var(--wf-error)' : '1px solid var(--wf-border)',
        padding: '0.5rem 0.6rem',
        cursor: draggable ? 'grab' : onClick ? 'pointer' : 'default',
      }}
      whileHover={{ borderColor: 'var(--wf-grip)' }}
      data-task-id={task.id}
      data-testid="task-card"
    >
      <div style={{ fontSize: '0.82rem', color: 'var(--wf-fg)', lineHeight: 1.35 }}>{task.title}</div>

      <div
        style={{
          display: 'flex',
          alignItems: 'center',
          flexWrap: 'wrap',
          gap: '0.55rem',
          marginTop: '0.35rem',
          fontSize: '0.68rem',
          color: 'var(--wf-dim)',
        }}
      >
        <span>p{task.priority}</span>

        {running && (
          <span style={{ display: 'inline-flex', alignItems: 'center', gap: '0.3rem', color: 'var(--wf-info)' }}>
            <motion.span
              animate={{ opacity: [0.35, 1, 0.35] }}
              transition={{ duration: 1.4, repeat: Infinity, ease: 'easeInOut' }}
              style={{ width: 6, height: 6, borderRadius: '50%', background: 'var(--wf-info)', display: 'inline-block' }}
            />
            {task.agentId !== undefined ? `agent ${task.agentId}` : 'working'}
          </span>
        )}

        {parked && (
          <span style={{ display: 'inline-flex', alignItems: 'center', gap: '0.3rem', color: 'var(--wf-warn)' }}>
            <span style={{ width: 6, height: 6, borderRadius: '50%', background: 'var(--wf-warn)', display: 'inline-block' }} />
            parked
          </span>
        )}

        {task.bounces > 0 && (
          <span style={{ color: 'var(--wf-warn)' }}>
            {task.bounces} bounce{task.bounces === 1 ? '' : 's'}
          </span>
        )}

        {task.blockedBy.length > 0 && (
          <span title={`blocked by ${task.blockedBy.join(', ')}`}>
            blocked by {task.blockedBy.map(taskSlug).join(', ')}
          </span>
        )}
      </div>
    </motion.div>
  );
};

export default Card;
