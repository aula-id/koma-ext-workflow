import React from 'react';
import { motion, AnimatePresence } from 'framer-motion';
import type { OutboundNoticeView, Project } from './Board';

/**
 * Folding indicator (ARCHITECTURE.md 6.2): the panel has no visibility into the
 * assembled invoke-prompt byte size (that lives only in the daemon), so a non-empty
 * `officeSummary` — which the kernel only ever populates once the transcript has
 * folded at least once — is the client-visible proxy for "the office is now working
 * from a rolling summary, not the full history".
 */
export function isFolded(project: Pick<Project, 'officeSummary'>): boolean {
  return Boolean(project.officeSummary && project.officeSummary.trim().length > 0);
}

export type NoticeStatus = 'sent' | 'paused' | 'queued';

/** Pure outbox-entry -> status mapping (ARCHITECTURE.md 6.5): paused takes priority
 * over sent (a notice can be marked sent from a prior send and later paused again is
 * not a real transition, but paused is always the more actionable state to surface). */
export function noticeStatus(n: Pick<OutboundNoticeView, 'sent' | 'paused'>): NoticeStatus {
  if (n.paused) return 'paused';
  if (n.sent) return 'sent';
  return 'queued';
}

const STATUS_COLOR: Record<NoticeStatus, string> = {
  sent: 'var(--wf-success)',
  paused: 'var(--wf-warn)',
  queued: 'var(--wf-dim)',
};

export interface OfficeChatProps {
  project: Project;
}

/**
 * READ-ONLY mirror of the PRD drafting conversation. The trio happens in the koma
 * MAIN CHAT (brief/reply flow via the workflow tools + chat notices) — this pane
 * only lets you watch it and see pending notices; there is deliberately no input
 * here (product decision 2026-07-15: "this chat should not exist — use the koma
 * main chat").
 */
export const OfficeChat: React.FC<OfficeChatProps> = ({ project }) => {
  const transcript = project.officeTranscript ?? [];
  const outbox = project.outbox ?? [];
  const folded = isFolded(project);

  return (
    <div
      style={{
        display: 'flex',
        flexDirection: 'column',
        gap: '0.6rem',
        borderLeft: '1px solid var(--wf-border)',
        paddingLeft: '1rem',
        maxHeight: 520,
      }}
      data-testid="office-chat"
    >
      <div style={{ display: 'flex', alignItems: 'baseline', justifyContent: 'space-between', gap: '0.5rem' }}>
        <h3 className="wf-section-title" style={{ margin: 0 }}>
          PRD drafting
        </h3>
        {folded && (
          <span title={project.officeSummary} style={{ fontSize: '0.62rem', color: 'var(--wf-dim)' }}>
            folded — earlier turns summarized
          </span>
        )}
      </div>

      <div
        style={{
          display: 'flex',
          flexDirection: 'column',
          gap: '0.4rem',
          overflowY: 'auto',
          flex: 1,
          minHeight: 120,
        }}
      >
        <AnimatePresence initial={false}>
          {transcript.map((m, i) => {
            const isUser = m.who === 'user';
            // Design-critique round 2: a full saturated 1px border wrapping every
            // bubble made the panel read as a stack of loud outlined boxes. Color
            // weight is now carried by a single 3px left accent bar on an otherwise
            // neutral (or lightly tinted, for "You") bubble background.
            const accentVar = isUser ? 'var(--wf-info)' : 'var(--wf-accent)';
            return (
              <motion.div
                key={i}
                initial={{ opacity: 0, y: 4 }}
                animate={{ opacity: 1, y: 0 }}
                exit={{ opacity: 0 }}
                style={{
                  alignSelf: isUser ? 'flex-end' : 'flex-start',
                  maxWidth: '85%',
                  background: 'var(--wf-panel2)',
                  borderLeft: `2px solid ${accentVar}`,
                  borderRadius: 'var(--wf-radius)',
                  padding: '0.4rem 0.6rem',
                }}
              >
                <div
                  style={{
                    fontSize: '0.6rem',
                    fontWeight: 600,
                    textTransform: 'uppercase',
                    letterSpacing: '0.04em',
                    color: 'var(--wf-fg-secondary)',
                    marginBottom: '0.15rem',
                  }}
                >
                  {isUser ? 'You' : 'Office'}
                </div>
                <div style={{ fontSize: '0.8rem', color: 'var(--wf-fg)', whiteSpace: 'pre-wrap' }}>{m.text}</div>
              </motion.div>
            );
          })}
        </AnimatePresence>
        {transcript.length === 0 && (
          <p style={{ fontSize: '0.75rem', color: 'var(--wf-dim)' }}>
            No conversation yet — brief the office from the koma chat (workflow_brief).
          </p>
        )}
      </div>

      <p style={{ fontSize: '0.68rem', color: 'var(--wf-dim)', margin: 0 }}>
        Read-only mirror — talk to the office in the koma chat; replies arrive there too.
      </p>

      <div>
        <h4 style={{ margin: '0 0 0.3rem', fontSize: '0.65rem', color: 'var(--wf-fg-secondary)', textTransform: 'uppercase' }}>
          Notices
        </h4>
        {outbox.length === 0 ? (
          <p style={{ fontSize: '0.7rem', color: 'var(--wf-fg-secondary)' }}>No pending notices.</p>
        ) : (
          <div style={{ display: 'flex', flexDirection: 'column', gap: '0.25rem' }}>
            {outbox.map((n) => {
              const status = noticeStatus(n);
              return (
                <div key={n.id} style={{ display: 'flex', gap: '0.4rem', alignItems: 'center', fontSize: '0.72rem' }}>
                  <span className="wf-status-dot" style={{ background: STATUS_COLOR[status], width: 5, height: 5 }} />
                  <span style={{ color: STATUS_COLOR[status] }}>{status}</span>
                  <span style={{ color: 'var(--wf-fg)' }}>{n.text}</span>
                </div>
              );
            })}
          </div>
        )}
      </div>
    </div>
  );
};

export default OfficeChat;
