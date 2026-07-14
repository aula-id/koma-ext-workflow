import React, { useState } from 'react';
import { motion, AnimatePresence } from 'framer-motion';
import { bridge } from '../bridge';
import type { CommentReceipt, TaskComment } from './Card';

export interface ReceiptPill {
  label: string;
  colorVar: string;
  /** Set only for a `pending` receipt on a `done` task (5.3): the comment was never
   * placed in any agent prompt, so the panel must not imply it was read. */
  flag?: string;
}

function formatTime(ms: number): string {
  try {
    return new Date(ms).toLocaleString();
  } catch {
    return String(ms);
  }
}

/**
 * Pure receipt -> pill mapping (ARCHITECTURE.md 5.3, PANEL_PROTOCOL.md 2.2). A
 * `pending` receipt still sitting on a `done` task is flagged "never delivered" so
 * the user is never misled into thinking the agent saw the comment; `delivered`/
 * `read` always carry their timestamp when the daemon sends one.
 */
export function receiptPill(receipt: CommentReceipt, taskDone: boolean): ReceiptPill {
  switch (receipt.state) {
    case 'read':
      return {
        label: receipt.atMs ? `read · ${formatTime(receipt.atMs)}` : 'read',
        colorVar: 'var(--wf-accent-green)',
      };
    case 'delivered':
      return {
        label: receipt.atMs ? `delivered · ${formatTime(receipt.atMs)}` : 'delivered',
        colorVar: 'var(--wf-accent-blue)',
      };
    case 'pending':
    default:
      if (taskDone) {
        return {
          label: 'pending',
          colorVar: 'var(--wf-accent-orange)',
          flag: 'never delivered — reopen to send',
        };
      }
      return { label: 'pending', colorVar: 'var(--wf-fg-secondary)' };
  }
}

const AUTHOR_LABEL: Record<TaskComment['author'], string> = {
  user: 'You',
  office: 'Office',
  system: 'System',
};

export interface CommentsProps {
  taskId: string;
  comments: TaskComment[];
  taskDone: boolean;
}

export const Comments: React.FC<CommentsProps> = ({ taskId, comments, taskDone }) => {
  const [draft, setDraft] = useState('');
  const [sending, setSending] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const sorted = comments.slice().sort((a, b) => a.createdMs - b.createdMs);

  const submit = async () => {
    const text = draft.trim();
    if (!text) return;
    setSending(true);
    setError(null);
    try {
      const res = await bridge.send({ op: 'comment_add', task: taskId, text });
      if (res?.error) {
        setError(res.error);
      } else {
        setDraft('');
      }
    } catch (err) {
      setError(err instanceof Error ? err.message : 'failed to add comment');
    } finally {
      setSending(false);
    }
  };

  return (
    <div style={{ display: 'flex', flexDirection: 'column', gap: '0.5rem' }}>
      <div style={{ display: 'flex', flexDirection: 'column', gap: '0.4rem' }}>
        <AnimatePresence initial={false}>
          {sorted.map((c) => {
            const pill = receiptPill(c.receipt, taskDone);
            return (
              <motion.div
                key={c.id}
                initial={{ opacity: 0, y: 4 }}
                animate={{ opacity: 1, y: 0 }}
                exit={{ opacity: 0 }}
                data-testid="comment-row"
                style={{
                  borderBottom: '1px solid var(--wf-border)',
                  padding: '0.35rem 0 0.45rem',
                }}
              >
                <div style={{ display: 'flex', justifyContent: 'space-between', alignItems: 'baseline', gap: '0.5rem' }}>
                  <span style={{ fontSize: '0.72rem', fontWeight: 700, color: 'var(--wf-fg)' }}>
                    {AUTHOR_LABEL[c.author]}
                  </span>
                  <span style={{ fontSize: '0.65rem', color: 'var(--wf-dim)' }}>
                    {formatTime(c.createdMs)}
                  </span>
                </div>
                <p style={{ fontSize: '0.8rem', color: 'var(--wf-fg)', margin: '0.2rem 0' }}>{c.text}</p>
                <div style={{ display: 'flex', alignItems: 'center', gap: '0.4rem', fontSize: '0.65rem' }}>
                  <span className="wf-status-dot" style={{ background: pill.colorVar, width: 5, height: 5 }} />
                  <span style={{ color: pill.colorVar }}>{pill.label}</span>
                  {pill.flag && <span style={{ color: 'var(--wf-warn)' }}>{pill.flag}</span>}
                </div>
              </motion.div>
            );
          })}
        </AnimatePresence>
        {sorted.length === 0 && (
          <p style={{ fontSize: '0.75rem', color: 'var(--wf-fg-secondary)' }}>No comments yet.</p>
        )}
      </div>

      <div style={{ display: 'flex', gap: '0.4rem' }}>
        <input
          value={draft}
          onChange={(e) => setDraft(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === 'Enter' && !e.shiftKey) {
              e.preventDefault();
              void submit();
            }
          }}
          placeholder="Add a comment for the agent..."
          style={{ flex: 1, fontSize: '0.8rem' }}
        />
        <button onClick={() => void submit()} disabled={sending || !draft.trim()} className="wf-btn wf-btn-accent">
          send
        </button>
      </div>
      {error && <span style={{ fontSize: '0.7rem', color: "var(--wf-error)" }}>{error}</span>}
    </div>
  );
};

export default Comments;
