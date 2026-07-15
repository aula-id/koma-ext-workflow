import React, { useEffect, useState } from 'react';
import { motion } from 'framer-motion';
import { bridge } from '../bridge';
import type { Task } from '../components/Card';
import Comments from '../components/Comments';
import ConfirmButton from '../components/ConfirmButton';

/** Compact timestamps: time-of-day for today, short date + time otherwise —
 * the drawer used to repeat eight full `toLocaleString()` datetimes in a row. */
function formatTime(ms: number): string {
  try {
    const d = new Date(ms);
    const now = new Date();
    const sameDay =
      d.getFullYear() === now.getFullYear() && d.getMonth() === now.getMonth() && d.getDate() === now.getDate();
    const time = d.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' });
    if (sameDay) return time;
    return `${d.toLocaleDateString([], { month: 'short', day: 'numeric' })} ${time}`;
  } catch {
    return String(ms);
  }
}

const STATE_LABEL: Record<Task['state'], string> = {
  backlog: 'backlog',
  todo: 'todo',
  onprogress: 'running',
  review: 'review',
  parked: 'parked',
  done: 'done',
};

const STATE_COLOR: Record<Task['state'], string> = {
  backlog: 'var(--wf-dim)',
  todo: 'var(--wf-dim)',
  onprogress: 'var(--wf-status-running)',
  review: 'var(--wf-status-review)',
  parked: 'var(--wf-status-parked)',
  done: 'var(--wf-status-done)',
};

export interface TaskDetailProps {
  task: Task;
  onClose?: () => void;
}

/** Section: small-caps title over a hairline — koma grammar, no boxes. */
const Section: React.FC<{ title: string; children: React.ReactNode }> = ({ title, children }) => (
  <section style={{ borderTop: '1px solid var(--wf-border)', paddingTop: '0.55rem' }}>
    <h3
      style={{
        fontSize: '0.65rem',
        fontWeight: 600,
        textTransform: 'uppercase',
        letterSpacing: '0.08em',
        color: 'var(--wf-dim)',
        margin: '0 0 0.4rem',
      }}
    >
      {title}
    </h3>
    {children}
  </section>
);

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

  const run = async (payload: Record<string, unknown>) => {
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
  const unpark = () => run({ op: 'unpark', task: task.id });
  const killWorker = () => run({ op: 'card_move', task: task.id, to: 'todo', killWorker: true });

  const history = (task.history ?? []).slice().sort((a, b) => a.atMs - b.atMs);
  const loaded = task.description !== undefined;

  return (
    <motion.div
      initial={{ x: 24, opacity: 0 }}
      animate={{ x: 0, opacity: 1 }}
      exit={{ x: 24, opacity: 0 }}
      transition={{ duration: 0.2 }}
      style={{
        position: 'fixed',
        top: 0,
        right: 0,
        height: '100vh',
        width: 'min(420px, 100vw)',
        boxSizing: 'border-box',
        zIndex: 40,
        background: 'var(--wf-panel)',
        borderLeft: '1px solid var(--wf-border)',
        padding: '1rem 1.25rem',
        display: 'flex',
        flexDirection: 'column',
        gap: '0.9rem',
        overflowY: 'auto',
        overflowX: 'hidden',
      }}
      data-testid="task-detail"
    >
      {/* Header flex discipline: chrome (status, close) is flex:none + nowrap so
          a monster hierarchical id can never crush it into vertical letters; the
          id itself is a single truncated line with the full value in a tooltip.
          Word-breaking (`anywhere`) is applied ONLY to content sections below,
          never to this chrome. */}
      <div style={{ display: 'flex', flexDirection: 'column', gap: '0.35rem' }}>
        <div style={{ display: 'flex', alignItems: 'center', gap: '0.6rem' }}>
          <span
            className="wf-status"
            style={{ color: STATE_COLOR[task.state], flex: 'none', whiteSpace: 'nowrap' }}
          >
            <span className="wf-status-dot" style={{ background: STATE_COLOR[task.state] }} />
            {STATE_LABEL[task.state]}
          </span>
          <span
            title={task.id}
            style={{
              fontSize: '0.68rem',
              color: 'var(--wf-dim)',
              flex: 1,
              minWidth: 0,
              overflow: 'hidden',
              textOverflow: 'ellipsis',
              whiteSpace: 'nowrap',
            }}
          >
            {task.id}
          </span>
          {onClose && (
            <button
              onClick={onClose}
              className="wf-btn wf-btn-ghost"
              aria-label="close task detail"
              style={{ flex: 'none', whiteSpace: 'nowrap' }}
            >
              close
            </button>
          )}
        </div>

        <h2 style={{ margin: 0, color: 'var(--wf-fg)', fontSize: '0.95rem', fontWeight: 700, lineHeight: 1.35 }}>
          {task.title}
        </h2>

        {/* meta line: priority editor inline, bounces only when present */}
        <div style={{ display: 'flex', alignItems: 'center', gap: '0.6rem', fontSize: '0.75rem', flexWrap: 'wrap' }}>
          <span style={{ color: 'var(--wf-dim)', flex: 'none' }}>priority</span>
          <input
            type="number"
            value={priorityDraft}
            onChange={(e) => setPriorityDraft(Number(e.target.value))}
            style={{ width: 54, padding: '0.15rem 0.35rem', flex: 'none' }}
            aria-label="priority"
          />
          {priorityDraft !== task.priority && (
            <button onClick={() => void savePriority()} disabled={busy} className="wf-btn wf-btn-accent" style={{ padding: '0.15rem 0.5rem', flex: 'none' }}>
              save
            </button>
          )}
          {task.bounces > 0 && (
            <span style={{ color: 'var(--wf-warn)', flex: 'none', whiteSpace: 'nowrap' }}>
              {task.bounces} bounce{task.bounces === 1 ? '' : 's'}
            </span>
          )}
        </div>
      </div>

      {!loaded && <p style={{ fontSize: '0.75rem', color: 'var(--wf-dim)', margin: 0 }}>Loading full detail…</p>}

      {(task.state === 'parked' || task.state === 'onprogress') && (
        <div style={{ display: 'flex', gap: '0.5rem' }}>
          {task.state === 'parked' && (
            <ConfirmButton label="unpark" className="wf-btn wf-btn-accent" disabled={busy} onConfirm={() => void unpark()} testId="unpark-btn" />
          )}
          {task.state === 'onprogress' && (
            <ConfirmButton label="kill worker" className="wf-btn wf-btn-danger" disabled={busy} onConfirm={() => void killWorker()} testId="kill-worker-btn" />
          )}
        </div>
      )}

      {task.description && (
        <Section title="Description">
          <p style={{ fontSize: '0.82rem', color: 'var(--wf-fg)', whiteSpace: 'pre-wrap', margin: 0, overflowWrap: 'anywhere' }}>
            {task.description}
          </p>
        </Section>
      )}

      {task.acceptance && task.acceptance.length > 0 && (
        <Section title="Acceptance criteria">
          <ul style={{ margin: 0, paddingLeft: '1.1rem', display: 'flex', flexDirection: 'column', gap: '0.2rem' }}>
            {task.acceptance.map((a, i) => (
              <li key={i} style={{ fontSize: '0.8rem', color: 'var(--wf-fg)' }}>
                {a}
              </li>
            ))}
          </ul>
        </Section>
      )}

      {task.lastReport && (
        <Section title="Worker report">
          <pre style={monoBlockStyle}>{task.lastReport}</pre>
        </Section>
      )}

      {task.lastReview && (
        <Section title="Review verdict">
          <pre style={monoBlockStyle}>{task.lastReview}</pre>
        </Section>
      )}

      <Section title="History">
        {history.length === 0 ? (
          <p style={{ fontSize: '0.75rem', color: 'var(--wf-dim)', margin: 0 }}>No history yet.</p>
        ) : (
          <div style={{ display: 'flex', flexDirection: 'column', gap: '0.2rem' }}>
            {history.map((h, i) => (
              <div key={i} style={{ display: 'flex', gap: '0.6rem', fontSize: '0.74rem' }}>
                <span style={{ color: 'var(--wf-dim)', minWidth: 84, textAlign: 'right', flex: 'none' }}>
                  {formatTime(h.atMs)}
                </span>
                <span style={{ color: 'var(--wf-fg)', overflowWrap: 'anywhere' }}>{h.event}</span>
              </div>
            ))}
          </div>
        )}
      </Section>

      <Section title="Comments">
        <Comments taskId={task.id} comments={task.comments ?? []} taskDone={task.state === 'done'} />
      </Section>

      {toast && <span style={{ fontSize: '0.75rem', color: 'var(--wf-warn)' }}>{toast}</span>}
    </motion.div>
  );
};

const monoBlockStyle: React.CSSProperties = {
  fontFamily: 'inherit',
  fontSize: '0.75rem',
  color: 'var(--wf-fg)',
  background: 'var(--wf-panel2)',
  border: '1px solid var(--wf-border)',
  borderRadius: 'var(--wf-radius)',
  padding: '0.5rem 0.6rem',
  overflowX: 'auto',
  whiteSpace: 'pre-wrap',
  overflowWrap: 'anywhere',
  margin: 0,
};

export default TaskDetail;
