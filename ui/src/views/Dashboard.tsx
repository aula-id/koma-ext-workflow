import React, { useEffect, useMemo, useState } from 'react';
import { motion } from 'framer-motion';
import { useStore, Project } from '../store';
import { bridge } from '../bridge';
import { formatElapsed } from '../lib/officeLayout';

interface DashboardProps {
  onProjectClick?: (projectId: string) => void;
  onSettings?: () => void;
}

/* Phase -> status color token. Status is a small dot + word, never a filled
 * chip and never a card background — koma grammar. */
const PHASE_COLOR_VAR: Record<string, string> = {
  drafting: 'var(--wf-status-drafting)',
  ready: 'var(--wf-accent)',
  running: 'var(--wf-status-running)',
  interrupted: 'var(--wf-status-parked)',
  halted: 'var(--wf-status-blocked)',
  done: 'var(--wf-status-done)',
};

const StatusDot: React.FC<{ colorVar: string; running: boolean; label: string }> = ({
  colorVar,
  running,
  label,
}) => (
  <span role="img" aria-label={label} title={label} className="wf-status-dot" style={{ background: colorVar, width: 8, height: 8 }}>
    {running && (
      <motion.span
        aria-hidden="true"
        animate={{ opacity: [0.35, 1, 0.35] }}
        transition={{ duration: 1.4, repeat: Infinity, ease: 'easeInOut' }}
        style={{ display: 'block', width: '100%', height: '100%', borderRadius: '9999px', background: colorVar }}
      />
    )}
  </span>
);

/* One flat row per project: dot | name + phase | inline counts | last notice.
 * Hairline separators between rows, hover wash, no boxes. */
const ProjectRow: React.FC<{ project: Project; onClick?: () => void; nowMs: number }> = ({ project, onClick, nowMs }) => {
  const phaseKind = project.phase.kind;
  const colorVar = PHASE_COLOR_VAR[phaseKind] || 'var(--wf-dim)';
  const running = project.runningCount || 0;
  const parked = project.parkedCount || 0;
  const done = project.doneCount || 0;
  const total = project.taskCount || 0;

  const stats: string[] = [];
  if (running > 0) stats.push(`${running} running`);
  if (parked > 0) stats.push(`${parked} parked`);
  stats.push(total > 0 ? `${done}/${total} done` : 'no tasks');
  // The last clean-build audit grade (6.2c), only when the project has been audited.
  if (project.lastAuditGrade != null) stats.push(`audit ${project.lastAuditGrade}`);

  return (
    <div
      onClick={onClick}
      className="flex items-start gap-3 cursor-pointer"
      style={{
        padding: '0.6rem 0.5rem',
        borderBottom: '1px solid var(--wf-border)',
      }}
      onMouseEnter={(e) => { (e.currentTarget as HTMLDivElement).style.background = 'var(--wf-hover)'; }}
      onMouseLeave={(e) => { (e.currentTarget as HTMLDivElement).style.background = 'transparent'; }}
    >
      <span style={{ marginTop: 5 }}>
        <StatusDot
          colorVar={colorVar}
          running={phaseKind === 'running'}
          label={`${done} of ${total} task${total === 1 ? '' : 's'} complete, ${phaseKind}`}
        />
      </span>
      <div className="flex-1 min-w-0">
        <div className="flex items-baseline gap-3">
          <span className="truncate" style={{ color: 'var(--wf-fg)', fontWeight: 600 }}>
            {project.name}
          </span>
          <span style={{ color: colorVar, fontSize: '0.75rem' }}>{phaseKind}</span>
        </div>
        {project.officeActivity ? (
          <div className="truncate flex items-center gap-1" style={{ color: 'var(--wf-info)', fontSize: '0.75rem', marginTop: 2 }}>
            <StatusDot colorVar="var(--wf-info)" running label={project.officeActivity.label} />
            <span>{project.officeActivity.label} · {formatElapsed(nowMs, project.officeActivity.sinceMs)}</span>
          </div>
        ) : (
          project.lastNotice && (
            <div className="truncate" style={{ color: 'var(--wf-dim)', fontSize: '0.75rem', marginTop: 2 }}>
              {project.lastNotice}
            </div>
          )
        )}
      </div>
      <div className="flex-none" style={{ color: 'var(--wf-dim)', fontSize: '0.78rem' }}>
        {stats.join(' · ')}
      </div>
    </div>
  );
};

export const Dashboard: React.FC<DashboardProps> = ({ onProjectClick, onSettings }) => {
  const { projects, snapshot } = useStore();

  // Ticking clock for live office activity elapsed times — only ticks while at least one
  // project has a live activity, so idle dashboards never re-render on a timer.
  const hasLiveActivity = projects.some((p) => p.officeActivity);
  const [nowMs, setNowMs] = useState(Date.now());
  useEffect(() => {
    if (!hasLiveActivity) return;
    const id = setInterval(() => setNowMs(Date.now()), 1000);
    return () => clearInterval(id);
  }, [hasLiveActivity]);

  useEffect(() => {
    // Regression (found while wiring the mock harness/deep links): calling
    // `useStore.setState((state) => { state.updateSnapshot(x); return state; })` runs
    // TWO nested zustand `setState`s — the inner one inside `updateSnapshot` applies the
    // new snapshot correctly, but the outer call then "returns" the STALE `state`
    // parameter it was invoked with (captured before the inner `set` ran) and zustand
    // merges that stale snapshot back on top, silently reverting every push. Calling the
    // store action directly does exactly one `set` and is also just less code.
    const unsubscribe = bridge.onSnapshot((newSnapshot) => {
      useStore.getState().updateSnapshot(newSnapshot);
    });

    bridge.hello('0.1.0').catch((err) => {
      console.error('Failed to initialize:', err);
    });

    return unsubscribe;
  }, []);

  const haltedProjects = useMemo(
    () => projects.filter((p) => p.phase.kind === 'halted'),
    [projects],
  );

  const attentionProjects = useMemo(
    () => projects.filter((p) => p.phase.kind === 'halted' || (p.parkedCount || 0) > 0),
    [projects],
  );
  const recentActivity = useMemo(
    () => projects.filter((p) => p.lastNotice && p.lastNotice.trim().length > 0),
    [projects],
  );

  return (
    <div className="p-6">
      <div style={{ maxWidth: 1100, margin: '0 auto' }}>
        <div
          className="flex items-center justify-between"
          style={{ paddingBottom: '0.75rem', borderBottom: '1px solid var(--wf-head)' }}
        >
          <div className="flex items-baseline gap-3">
            <h1 style={{ color: 'var(--wf-fg)', fontSize: '1.05rem', fontWeight: 700, margin: 0 }}>
              Workflow
            </h1>
            <span style={{ color: 'var(--wf-dim)', fontSize: '0.78rem' }}>
              {projects.length === 0
                ? 'no projects yet'
                : `${projects.length} project${projects.length === 1 ? '' : 's'}`}
            </span>
          </div>
          {onSettings && (
            <button onClick={onSettings} className="wf-btn wf-btn-ghost" aria-label="Settings">
              settings
            </button>
          )}
        </div>

        {/* Accurate, flat halt notice: says exactly how many lines are halted
            (the old banner claimed ALL lines were halted whenever ONE was). */}
        {haltedProjects.length > 0 && (
          <div
            className="text-sm"
            style={{
              marginTop: '0.75rem',
              padding: '0.45rem 0.6rem',
              borderLeft: '2px solid var(--wf-error)',
              background: 'var(--wf-tint-error)',
              color: 'var(--wf-error)',
            }}
          >
            {haltedProjects.length === 1
              ? `1 production line halted: ${haltedProjects[0].name}`
              : `${haltedProjects.length} production lines halted`}
            {' — a parked task blocks remaining work.'}
          </div>
        )}

        {projects.length === 0 ? (
          <div className="empty-state">
            <p>Create a project to get started. Use workflow_brief in chat or the panel.</p>
          </div>
        ) : (
          <div style={{ marginTop: '0.5rem' }}>
            {projects.map((project) => (
              <ProjectRow
                key={project.id}
                project={project}
                nowMs={nowMs}
                onClick={() => onProjectClick?.(project.id)}
              />
            ))}
          </div>
        )}

        {projects.length > 0 && (
          <div className="grid grid-cols-1 md:grid-cols-2" style={{ gap: '2rem' }}>
            <div className="wf-section">
              <h2 className="wf-section-title">Attention needed</h2>
              {attentionProjects.length === 0 ? (
                <p className="text-sm" style={{ color: 'var(--wf-dim)', margin: 0 }}>
                  Nothing needs attention right now.
                </p>
              ) : (
                <div className="flex flex-col" style={{ gap: '0.35rem' }}>
                  {attentionProjects.map((p) => (
                    <div
                      key={p.id}
                      className="flex items-center justify-between text-sm cursor-pointer"
                      onClick={() => onProjectClick?.(p.id)}
                    >
                      <span style={{ color: 'var(--wf-fg)' }}>{p.name}</span>
                      <span
                        style={{
                          color: p.phase.kind === 'halted' ? 'var(--wf-error)' : 'var(--wf-warn)',
                          fontSize: '0.78rem',
                        }}
                      >
                        {p.phase.kind === 'halted' ? 'halted' : `${p.parkedCount} parked`}
                      </span>
                    </div>
                  ))}
                </div>
              )}
            </div>

            <div className="wf-section">
              <h2 className="wf-section-title">Recent activity</h2>
              {recentActivity.length === 0 ? (
                <p className="text-sm" style={{ color: 'var(--wf-dim)', margin: 0 }}>
                  No recent activity yet.
                </p>
              ) : (
                <div className="flex flex-col" style={{ gap: '0.35rem' }}>
                  {recentActivity.map((p) => (
                    <div key={p.id} className="text-sm cursor-pointer truncate" onClick={() => onProjectClick?.(p.id)}>
                      <span style={{ color: 'var(--wf-fg)' }}>{p.name}</span>
                      <span style={{ color: 'var(--wf-dim)' }}>{' · '}{p.lastNotice}</span>
                    </div>
                  ))}
                </div>
              )}
            </div>
          </div>
        )}

        {snapshot && snapshot.truncated && (
          <div className="mt-4 text-xs" style={{ color: 'var(--wf-dim)' }}>
            Some data was truncated due to size limits. Refresh for full details.
          </div>
        )}
      </div>
    </div>
  );
};

export default Dashboard;
