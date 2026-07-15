import React from 'react';
import type { OutboundNoticeView, Project } from './Board';

/**
 * Folding indicator (ARCHITECTURE.md 6.2): the panel has no visibility into the
 * assembled invoke-prompt byte size (that lives only in the daemon), so a non-empty
 * `officeSummary` — which the kernel only ever populates once the transcript has
 * folded at least once — is the client-visible proxy for "the office is now working
 * from a rolling summary, not the full history". Kept exported for callers/tests even
 * though the read-only transcript mirror it used to annotate has been removed.
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
  /** Jump to the board's 'trace' tab (feature: tracelog). When absent the pointer degrades to
   * plain text naming the tab. */
  onOpenTrace?: () => void;
}

/**
 * The drafting sidebar. The old read-only chat MIRROR (YOU/OFFICE bubbles) was removed — the trio
 * happens in the koma MAIN CHAT (brief/reply via the workflow tools + chat notices), and mirroring
 * it here only nudged people back to the main chat (product decision 2026-07-15: "this chat is
 * bad"). In its place: a pointer to the koma chat for the conversation and to the TRACE TAB for the
 * machine's live activity ("this did what, that did what"). The pending NOTICES log is kept — it is
 * the one durable, panel-only signal that has no home in the main chat.
 */
export const OfficeChat: React.FC<OfficeChatProps> = ({ project, onOpenTrace }) => {
  const outbox = project.outbox ?? [];

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
      <h3 className="wf-section-title" style={{ margin: 0 }}>
        PRD drafting
      </h3>

      {/* Pointer that replaced the read-only chat mirror. */}
      <div style={{ fontSize: '0.75rem', color: 'var(--wf-fg-secondary)', lineHeight: 1.5 }}>
        <p style={{ margin: '0 0 0.5rem' }}>
          The office conversation happens in the koma main chat — brief the office there and its
          replies arrive there too.
        </p>
        <p style={{ margin: 0 }}>
          Watch what the machine is doing on the{' '}
          {onOpenTrace ? (
            <button
              type="button"
              onClick={onOpenTrace}
              data-testid="open-trace"
              style={{
                background: 'none',
                border: 'none',
                padding: 0,
                font: 'inherit',
                color: 'var(--wf-accent)',
                cursor: 'pointer',
                textDecoration: 'underline',
              }}
            >
              trace tab
            </button>
          ) : (
            <span style={{ color: 'var(--wf-fg)' }}>trace tab</span>
          )}
          .
        </p>
      </div>

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
