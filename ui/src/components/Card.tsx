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
  description?: string;
  acceptance?: string[];
  comments?: TaskComment[];
  lastReport?: string | null;
  lastReview?: string | null;
  history?: TaskHistoryEntry[];
}

const STATE_LABEL: Record<TaskStateKey, string> = {
  backlog: 'Backlog',
  todo: 'Todo',
  onprogress: 'Running',
  review: 'Review',
  parked: 'Parked',
  done: 'Done',
};

function stateBadgeStyle(state: TaskStateKey): React.CSSProperties {
  switch (state) {
    case 'parked':
      return { background: 'var(--wf-status-parked)', color: 'var(--wf-bg)' };
    case 'onprogress':
      return { background: 'var(--wf-status-running)', color: 'var(--wf-bg)' };
    case 'review':
      return { background: 'var(--wf-status-review)', color: 'var(--wf-bg)' };
    case 'done':
      return { background: 'var(--wf-status-done)', color: 'var(--wf-bg)' };
    default:
      return { background: 'var(--wf-bg)', color: 'var(--wf-fg-secondary)', border: '1px solid var(--wf-fg-secondary)' };
  }
}

export interface CardProps {
  task: Task;
  draggable?: boolean;
  culprit?: boolean;
  onDragStart?: (task: Task, e: React.DragEvent) => void;
  onClick?: (task: Task) => void;
}

export const Card: React.FC<CardProps> = ({ task, draggable = false, culprit = false, onDragStart, onClick }) => {
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
        background: 'var(--wf-bg-secondary)',
        borderRadius: 'var(--wf-radius)',
        boxShadow: 'var(--wf-shadow)',
        border: culprit ? '1px solid var(--wf-accent-pink)' : '1px solid transparent',
        padding: '0.6rem 0.7rem',
        cursor: draggable ? 'grab' : onClick ? 'pointer' : 'default',
      }}
      data-task-id={task.id}
      data-testid="task-card"
    >
      <div style={{ display: 'flex', alignItems: 'flex-start', justifyContent: 'space-between', gap: '0.5rem' }}>
        <span style={{ fontSize: '0.85rem', fontWeight: 600, color: 'var(--wf-fg)', flex: 1 }}>{task.title}</span>
        <span
          style={{
            fontSize: '0.65rem',
            fontWeight: 600,
            padding: '0.05rem 0.4rem',
            borderRadius: 'var(--wf-radius)',
            whiteSpace: 'nowrap',
            ...stateBadgeStyle(task.state),
          }}
        >
          {STATE_LABEL[task.state]}
        </span>
      </div>

      <div style={{ display: 'flex', alignItems: 'center', gap: '0.4rem', marginTop: '0.35rem' }}>
        {task.state === 'onprogress' && (
          <span style={{ display: 'inline-flex', alignItems: 'center', gap: '0.25rem' }}>
            <motion.span
              animate={{ opacity: [0.35, 1, 0.35] }}
              transition={{ duration: 1.4, repeat: Infinity, ease: 'easeInOut' }}
              style={{
                width: 6,
                height: 6,
                borderRadius: '50%',
                background: 'var(--wf-status-running)',
                display: 'inline-block',
              }}
            />
            <span style={{ fontSize: '0.65rem', color: 'var(--wf-fg-secondary)' }}>
              {task.agentId !== undefined ? `agent ${task.agentId}` : 'in progress'}
            </span>
          </span>
        )}
        <span
          style={{
            fontSize: '0.65rem',
            color: 'var(--wf-fg-secondary)',
            border: '1px solid var(--wf-border)',
            borderRadius: 'var(--wf-radius)',
            padding: '0.02rem 0.35rem',
          }}
        >
          p{task.priority}
        </span>
        {task.bounces > 0 && (
          <span
            style={{
              fontSize: '0.65rem',
              color: 'var(--wf-accent-pink)',
            }}
          >
            bounce x{task.bounces}
          </span>
        )}
      </div>

      {task.blockedBy.length > 0 && (
        <div style={{ display: 'flex', flexWrap: 'wrap', gap: '0.25rem', marginTop: '0.4rem' }}>
          {task.blockedBy.map((id) => (
            <span
              key={id}
              style={{
                fontSize: '0.6rem',
                color: 'var(--wf-fg-secondary)',
                border: '1px solid var(--wf-fg-secondary)',
                borderRadius: 'var(--wf-radius)',
                padding: '0.02rem 0.3rem',
              }}
              title={`blocked by ${id}`}
            >
              blocked-by {id}
            </span>
          ))}
        </div>
      )}
    </motion.div>
  );
};

export default Card;
