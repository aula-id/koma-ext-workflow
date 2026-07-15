import React, { useEffect, useRef } from 'react';
import type { Project, TraceEvent } from './Board';

/** `HH:MM:SS` (local time, zero-padded) from epoch milliseconds — the timestamp prefix on each
 * trace line. Local time is deliberate: the diary reads like a log the operator is watching. */
export function formatTraceTime(ts: number): string {
  const d = new Date(ts);
  const pad = (n: number) => n.toString().padStart(2, '0');
  return `${pad(d.getHours())}:${pad(d.getMinutes())}:${pad(d.getSeconds())}`;
}

export interface TraceLogProps {
  project: Project;
}

const MONO = 'ui-monospace, SFMono-Regular, Menlo, Consolas, "Liberation Mono", monospace';

/**
 * The machine's diary (feature: tracelog): a read-only, newest-last monospace timeline of what
 * the office machine DID — persona/TRD/CRD invokes, doc captures, safeguard-gate stops/approvals,
 * research + audit lifecycle, breakdown/authorize, task transitions, interrupt/resume. Each line
 * is `HH:MM:SS kind summary`; summaries never carry document content (byte counts / reasons only,
 * enforced server-side in office-core).
 *
 * Standard log-tail UX: the viewport auto-scrolls to the bottom on new entries UNLESS the user has
 * scrolled up to read history, in which case it stays put (the `stick` ref tracks whether we are
 * pinned to the bottom, re-armed the moment the user scrolls back down).
 */
export const TraceLog: React.FC<TraceLogProps> = ({ project }) => {
  const events: TraceEvent[] = project.trace ?? [];
  const scrollRef = useRef<HTMLDivElement>(null);
  const stick = useRef(true);

  const onScroll = () => {
    const el = scrollRef.current;
    if (!el) return;
    const dist = el.scrollHeight - el.scrollTop - el.clientHeight;
    stick.current = dist < 24; // within 24px of the bottom counts as "tailing"
  };

  useEffect(() => {
    const el = scrollRef.current;
    if (el && stick.current) {
      el.scrollTop = el.scrollHeight;
    }
  }, [events.length]);

  return (
    <div>
      <div style={{ display: 'flex', alignItems: 'baseline', justifyContent: 'space-between', marginBottom: '0.6rem' }}>
        <h3 className="wf-section-title" style={{ margin: 0 }}>
          machine trace
        </h3>
        <span style={{ fontSize: '0.65rem', color: 'var(--wf-dim)' }}>
          {events.length} event{events.length === 1 ? '' : 's'}
        </span>
      </div>

      <div
        ref={scrollRef}
        onScroll={onScroll}
        data-testid="trace-log"
        style={{
          fontFamily: MONO,
          fontSize: '0.72rem',
          lineHeight: 1.65,
          maxHeight: 560,
          overflowY: 'auto',
          background: 'var(--wf-panel2)',
          borderLeft: '2px solid var(--wf-border)',
          padding: '0.6rem 0.8rem',
          color: 'var(--wf-fg)',
        }}
      >
        {events.length === 0 ? (
          <div style={{ color: 'var(--wf-dim)' }}>
            No activity yet — the machine&rsquo;s diary fills in as the office works.
          </div>
        ) : (
          events.map((e, i) => (
            <div
              key={i}
              style={{ display: 'flex', gap: '0.6rem', whiteSpace: 'pre-wrap', wordBreak: 'break-word' }}
            >
              <span style={{ color: 'var(--wf-dim)', flexShrink: 0 }}>{formatTraceTime(e.ts)}</span>
              <span style={{ color: 'var(--wf-accent)', flexShrink: 0, minWidth: 82 }}>{e.kind}</span>
              <span>{e.summary}</span>
            </div>
          ))
        )}
      </div>
    </div>
  );
};

export default TraceLog;
