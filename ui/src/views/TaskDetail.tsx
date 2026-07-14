import React, { useEffect, useState } from 'react';
import { motion } from 'framer-motion';
import { bridge } from '../bridge';
import type { Task } from '../components/Card';
import Comments from '../components/Comments';

function formatTime(ms: number): string {
  try {
    return new Date(ms).toLocaleString();
  } catch {
    return String(ms);
  }
}

const STATE_LABEL: Record<Task['state'], string> = {
  backlog: 'Backlog',
  todo: 'Todo',
  onprogress: 'Running',
  review: 'Review',
  parked: 'Parked',
  done: 'Done',
};

export interface TaskDetailProps {
  task: Task;
  onClose?: () => void;
}

/**
 * Full-body task view (ARCHITECTURE.md 10.3). The frozen snapshot (PANEL_PROTOCOL.md
 * 2.2) drops description/acceptance/comments/report/review/history in summary mode
 * (>900KB push guard); when this task arrives without those fields, we opportunistically
 * fire `{ op: "task_detail", task }` (10.2) so the next push can fill them in. This is
 * fire-and-forget by protocol design (the ack carries no inline data — mutating-op
 * shape), so the view still renders gracefully from whatever the current snapshot has.
 */
export const TaskDetail: React.FC<TaskDetailProps> = ({ task, onClose }) => {
  const [priorityDraft, setPriorityDraft] = useState(task.priority);
  const [toast, setToast] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    setPriorityDraft(task.priority);
  }, [task.id, task.priority]);

  useEffect(() => {
    if (task.description === undefined) {
      bridge.send({ op: 'task_detail', task: task.id }).catch(() => {
        // best-effort; the panel just keeps showing whatever it already has
      });
    }
  }, [task.id, task.description]);

  useEffect(() => {
    if (!toast) return;
    const t = setTimeout(() => setToast(null), 3500);
    return () => clearTimeout(t);
  }, [toast]);

  const run = async (payload: Record<string, unknown>, confirmMsg?: string) => {
    if (confirmMsg && !window.confirm(confirmMsg)) return;
    setBusy(true);
    try {
      const res = await bridge.send(payload);
      if (res?.error) setToast(res.error);
    } catch (err) {
      setToast(err instanceof Error ? err.message : 'action failed');
    } finally {
      setBusy(false);
    }
  };

  const savePriority = () => run({ op: 'edit_task', task: task.id, priority: priorityDraft });
  const unpark = () => run({ op: 'unpark', task: task.id }, `Unpark "${task.title}" and send it back to Todo?`);
  const killWorker = () =>
    run(
      { op: 'card_move', task: task.id, to: 'todo', killWorker: true },
      `Kill the running worker for "${task.title}" and requeue it?`,
    );

  const history = (task.history ?? []).slice().sort((a, b) => a.atMs - b.atMs);
  const loaded = task.description !== undefined;

  return (
    <motion.div
      initial={{ x: 24, opacity: 0 }}
      animate={{ x: 0, opacity: 1 }}
      exit={{ x: 24, opacity: 0 }}
      transition={{ duration: 0.2 }}
      style={{
        background: 'var(--wf-bg-secondary)',
        borderRadius: 'var(--wf-radius)',
        boxShadow: 'var(--wf-shadow)',
        padding: '1rem 1.25rem',
        display: 'flex',
        flexDirection: 'column',
        gap: '0.9rem',
        maxHeight: '100%',
        overflowY: 'auto',
      }}
      data-testid="task-detail"
    >
      <div style={{ display: 'flex', alignItems: 'flex-start', justifyContent: 'space-between', gap: '0.5rem' }}>
        <div>
          <span style={{ fontSize: '0.65rem', color: 'var(--wf-fg-secondary)' }}>{task.id}</span>
          <h2 style={{ margin: '0.15rem 0 0', color: 'var(--wf-fg)', fontSize: '1.1rem' }}>{task.title}</h2>
          <span style={{ fontSize: '0.7rem', color: 'var(--wf-accent-blue)' }}>{STATE_LABEL[task.state]}</span>
        </div>
        {onClose && (
          <button onClick={onClose} style={closeButtonStyle} aria-label="close task detail">
            close
          </button>
        )}
      </div>

      {!loaded && (
        <p style={{ fontSize: '0.75rem', color: 'var(--wf-fg-secondary)' }}>Loading full detail…</p>
      )}

      {task.description && (
        <section>
          <h3 style={sectionHeadStyle}>Description</h3>
          <p style={{ fontSize: '0.85rem', color: 'var(--wf-fg)', whiteSpace: 'pre-wrap' }}>{task.description}</p>
        </section>
      )}

      {task.acceptance && task.acceptance.length > 0 && (
        <section>
          <h3 style={sectionHeadStyle}>Acceptance criteria</h3>
          <ul style={{ margin: 0, paddingLeft: '1.1rem', display: 'flex', flexDirection: 'column', gap: '0.2rem' }}>
            {task.acceptance.map((a, i) => (
              <li key={i} style={{ fontSize: '0.8rem', color: 'var(--wf-fg)' }}>
                {a}
              </li>
            ))}
          </ul>
        </section>
      )}

      <section>
        <h3 style={sectionHeadStyle}>Attempts &amp; bounces</h3>
        <div style={{ display: 'flex', gap: '0.5rem', alignItems: 'center' }}>
          <span
            style={{
              fontSize: '0.75rem',
              fontWeight: 600,
              color: task.bounces > 0 ? 'var(--wf-accent-pink)' : 'var(--wf-fg-secondary)',
            }}
          >
            bounces: {task.bounces}
          </span>
          <span style={{ fontSize: '0.75rem', color: 'var(--wf-fg-secondary)' }}>priority: p{task.priority}</span>
        </div>
      </section>

      <section>
        <h3 style={sectionHeadStyle}>Priority</h3>
        <div style={{ display: 'flex', gap: '0.4rem', alignItems: 'center' }}>
          <input
            type="number"
            value={priorityDraft}
            onChange={(e) => setPriorityDraft(Number(e.target.value))}
            style={{
              width: 70,
              background: 'var(--wf-bg)',
              border: '1px solid var(--wf-fg-secondary)',
              borderRadius: 'var(--wf-radius)',
              padding: '0.3rem 0.4rem',
              color: 'var(--wf-fg)',
              fontSize: '0.8rem',
            }}
          />
          <button
            onClick={() => void savePriority()}
            disabled={busy || priorityDraft === task.priority}
            style={actionButtonStyle('var(--wf-accent-blue)', busy || priorityDraft === task.priority)}
          >
            Save
          </button>
        </div>
      </section>

      <section>
        <h3 style={sectionHeadStyle}>Actions</h3>
        <div style={{ display: 'flex', gap: '0.5rem' }}>
          {task.state === 'parked' && (
            <button onClick={() => void unpark()} disabled={busy} style={actionButtonStyle('var(--wf-accent-green)', busy)}>
              Unpark
            </button>
          )}
          {task.state === 'onprogress' && (
            <button onClick={() => void killWorker()} disabled={busy} style={actionButtonStyle('var(--wf-accent-pink)', busy)}>
              Kill worker
            </button>
          )}
          {task.state !== 'parked' && task.state !== 'onprogress' && (
            <span style={{ fontSize: '0.75rem', color: 'var(--wf-fg-secondary)' }}>no actions available</span>
          )}
        </div>
      </section>

      {task.lastReport && (
        <section>
          <h3 style={sectionHeadStyle}>Worker report</h3>
          <pre style={monoBlockStyle}>{task.lastReport}</pre>
        </section>
      )}

      {task.lastReview && (
        <section>
          <h3 style={sectionHeadStyle}>Review verdict</h3>
          <pre style={monoBlockStyle}>{task.lastReview}</pre>
        </section>
      )}

      <section>
        <h3 style={sectionHeadStyle}>State history</h3>
        {history.length === 0 ? (
          <p style={{ fontSize: '0.75rem', color: 'var(--wf-fg-secondary)' }}>No history yet.</p>
        ) : (
          <div style={{ display: 'flex', flexDirection: 'column', gap: '0.2rem' }}>
            {history.map((h, i) => (
              <div key={i} style={{ display: 'flex', gap: '0.5rem', fontSize: '0.75rem' }}>
                <span style={{ color: 'var(--wf-fg-secondary)', minWidth: 130 }}>{formatTime(h.atMs)}</span>
                <span style={{ color: 'var(--wf-fg)' }}>{h.event}</span>
              </div>
            ))}
          </div>
        )}
      </section>

      <section>
        <h3 style={sectionHeadStyle}>Comments</h3>
        <Comments taskId={task.id} comments={task.comments ?? []} taskDone={task.state === 'done'} />
      </section>

      {toast && (
        <span style={{ fontSize: '0.75rem', color: 'var(--wf-accent-orange)' }}>{toast}</span>
      )}
    </motion.div>
  );
};

const sectionHeadStyle: React.CSSProperties = {
  fontSize: '0.7rem',
  fontWeight: 700,
  textTransform: 'uppercase',
  letterSpacing: '0.03em',
  color: 'var(--wf-fg-secondary)',
  margin: '0 0 0.35rem',
};

const monoBlockStyle: React.CSSProperties = {
  fontFamily: 'ui-monospace, SFMono-Regular, Menlo, Consolas, monospace',
  fontSize: '0.75rem',
  color: 'var(--wf-fg)',
  background: 'var(--wf-bg)',
  borderRadius: 'var(--wf-radius)',
  padding: '0.6rem',
  overflowX: 'auto',
  whiteSpace: 'pre-wrap',
  margin: 0,
};

const closeButtonStyle: React.CSSProperties = {
  fontSize: '0.7rem',
  color: 'var(--wf-fg-secondary)',
  background: 'transparent',
  border: '1px solid var(--wf-fg-secondary)',
  borderRadius: 'var(--wf-radius)',
  padding: '0.2rem 0.5rem',
  cursor: 'pointer',
};

function actionButtonStyle(colorVar: string, disabled: boolean): React.CSSProperties {
  return {
    fontSize: '0.75rem',
    fontWeight: 600,
    borderRadius: 'var(--wf-radius)',
    padding: '0.35rem 0.7rem',
    border: `1px solid ${colorVar}`,
    color: colorVar,
    background: 'transparent',
    cursor: disabled ? 'not-allowed' : 'pointer',
    opacity: disabled ? 0.5 : 1,
  };
}

export default TaskDetail;
