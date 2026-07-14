import React, { useEffect, useMemo, useState } from 'react';
import { motion, AnimatePresence } from 'framer-motion';
import { useStore, Project } from '../store';
import { bridge } from '../bridge';

interface DashboardProps {
  onProjectClick?: (projectId: string) => void;
  onSettings?: () => void;
}

/**
 * Deliberate, always-present status affordance. This replaces a per-card SVG
 * progress ring whose colored arc was a thin sliver at most done/total
 * fractions — and fully invisible at 0/0 — reading as a broken/clipped
 * spinner rather than an intentional indicator, and inconsistent across cards
 * (design-critique round 2). Every card now renders the exact same slot: a
 * pulsing dot for a running project, a static dot (matching the phase badge
 * color) for every other phase — present and visually identical in shape
 * regardless of state, never half-drawn.
 */
const StatusDot: React.FC<{ colorVar: string; running: boolean; label: string }> = ({
  colorVar,
  running,
  label,
}) => (
  <span
    role="img"
    aria-label={label}
    title={label}
    style={{ display: 'inline-flex', width: 12, height: 12, alignItems: 'center', justifyContent: 'center' }}
  >
    <motion.span
      animate={running ? { opacity: [0.4, 1, 0.4] } : { opacity: 1 }}
      transition={running ? { duration: 1.4, repeat: Infinity, ease: 'easeInOut' } : undefined}
      style={{
        width: 10,
        height: 10,
        borderRadius: '9999px',
        background: colorVar,
        display: 'inline-block',
        boxShadow: running ? `0 0 0 3px color-mix(in srgb, ${colorVar} 25%, transparent)` : 'none',
      }}
    />
  </span>
);

// Project phase badge colors. `running` intentionally uses the same green as
// `done`/success rather than the koma "info" blue role: this is the single
// semantic-color source for a running project across the whole card — the
// phase badge, the status dot (see StatusDot's `running` prop) and the
// "Running" stat tile below all read from here (or its literal value) so the
// same state never shows up in two colors on one card. interrupted/parked read
// as the "amber" bucket, halted/blocked as the "red" bucket (accent-pink is
// koma's error role, i.e. red), ready keeps the purple accent as the
// actionable/next-step color. drafting gets its own dedicated indigo status
// color (rendered as a tinted, not solid, chip — see `isDrafting` below):
// it used to share --wf-fg-secondary with the *text* drawn on top of it,
// which read as gray-on-gray (design-critique round 2).
const PHASE_COLOR_VAR: Record<string, string> = {
  drafting: 'var(--wf-status-drafting)',
  ready: 'var(--wf-accent-purple)',
  running: 'var(--wf-accent-green)',
  interrupted: 'var(--wf-accent-orange)',
  halted: 'var(--wf-accent-pink)',
  done: 'var(--wf-accent-green)',
};

const ProjectCard: React.FC<{
  project: Project;
  onClick?: () => void;
}> = ({ project, onClick }) => {
  const phaseKind = project.phase.kind;
  const phaseColorVar = PHASE_COLOR_VAR[phaseKind] || 'var(--wf-fg-secondary)';
  const isDrafting = phaseKind === 'drafting';

  const isRunning = phaseKind === 'running';

  return (
    <motion.div
      layout
      // Opacity stays pinned at 1 on entrance — only `scale` animates. A card
      // whose *first painted frame* is opacity:0 means every label/number on it
      // is briefly illegible, which is what design-critique round 1 caught as a
      // "washed out" dashboard (worst-cased by whatever moment a screenshot
      // landed on). The scale pop is still there; the text is always readable.
      initial={{ opacity: 1, scale: 0.95 }}
      animate={{ opacity: 1, scale: 1 }}
      exit={{ opacity: 0, scale: 0.95 }}
      transition={{ duration: 0.2 }}
      onClick={onClick}
      className="p-4 rounded-lg border cursor-pointer transition-colors hover:bg-[var(--wf-hover)]"
      style={{ borderColor: 'var(--wf-border)', backgroundColor: 'var(--wf-bg-secondary)' }}
    >
      <div className="flex items-start justify-between mb-3">
        <div className="flex-1">
          <h3 className="text-lg font-semibold truncate" style={{ color: 'var(--wf-fg)' }}>
            {project.name}
          </h3>
          <span
            className="inline-block mt-1 px-2 py-1 text-xs font-semibold rounded capitalize"
            style={{
              backgroundColor: isDrafting ? 'var(--wf-tint-drafting)' : phaseColorVar,
              color: isDrafting ? 'var(--wf-status-drafting)' : 'var(--wf-bg)',
            }}
          >
            {phaseKind}
          </span>
        </div>
        <div className="ml-2">
          <StatusDot
            colorVar={phaseColorVar}
            running={isRunning}
            label={`${project.doneCount || 0} of ${project.taskCount || 0} task${
              (project.taskCount || 0) === 1 ? '' : 's'
            } complete, ${phaseKind}`}
          />
        </div>
      </div>

      <div className="grid grid-cols-3 gap-3 mb-3 text-sm">
        <div className="p-2 rounded" style={{ backgroundColor: 'var(--wf-bg)' }}>
          <div className="text-xs uppercase tracking-wide" style={{ color: 'var(--wf-fg-secondary)' }}>Running</div>
          <div className="text-2xl font-semibold" style={{ color: 'var(--wf-accent-green)' }}>
            {project.runningCount || 0}
          </div>
        </div>
        <div className="p-2 rounded" style={{ backgroundColor: 'var(--wf-bg)' }}>
          <div className="text-xs uppercase tracking-wide" style={{ color: 'var(--wf-fg-secondary)' }}>Parked</div>
          <div className="text-2xl font-semibold" style={{ color: 'var(--wf-accent-orange)' }}>
            {project.parkedCount || 0}
          </div>
        </div>
        <div className="p-2 rounded" style={{ backgroundColor: 'var(--wf-bg)' }}>
          <div className="text-xs uppercase tracking-wide" style={{ color: 'var(--wf-fg-secondary)' }}>Total</div>
          <div className="text-2xl font-semibold" style={{ color: 'var(--wf-fg-secondary)' }}>
            {project.taskCount || 0}
          </div>
        </div>
      </div>

      {/* Always reserve the note row (a muted placeholder when there's nothing to
          show) rather than letting cards without a `lastNotice` go quietly taller
          via grid row-stretch — see the `items-start` on the grid below, which
          stops row-stretch from padding shorter cards out in the first place; this
          placeholder additionally keeps every card's own internal layout identical
          regardless of content. */}
      <div
        className="p-2 rounded text-xs truncate"
        style={{ backgroundColor: 'var(--wf-bg)', color: 'var(--wf-fg-secondary)' }}
      >
        {project.lastNotice || 'No recent activity'}
      </div>
    </motion.div>
  );
};

const HaltIndicator: React.FC<{ halted: boolean }> = ({ halted }) => {
  if (!halted) return null;

  return (
    <motion.div
      initial={{ opacity: 0, y: -10 }}
      animate={{ opacity: 1, y: 0 }}
      className="border p-3 rounded-lg mb-4 text-sm"
      style={{
        backgroundColor: 'var(--wf-tint-error)',
        borderColor: 'var(--wf-accent-pink)',
        color: 'var(--wf-accent-pink)',
      }}
    >
      All production lines halted: a parked task blocks all work.
    </motion.div>
  );
};

export const Dashboard: React.FC<DashboardProps> = ({ onProjectClick, onSettings }) => {
  const { projects, snapshot } = useStore();
  const [haltedProjects, setHaltedProjects] = useState(0);

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

  useEffect(() => {
    const halted = projects.filter((p) => p.phase.kind === 'halted').length;
    setHaltedProjects(halted);
  }, [projects]);

  const globalHalted = haltedProjects > 0;

  // Second information tier (design-critique round 2): with only a handful of
  // project cards the rest of the viewport read as a dead void, "a toy dashboard"
  // for what should be a dense control surface. Both lists are derived from data
  // the dashboard already has per project (no new protocol fields) so they degrade
  // gracefully to an empty-state line rather than ever being wrong/stale.
  const attentionProjects = useMemo(
    () => projects.filter((p) => p.phase.kind === 'halted' || (p.parkedCount || 0) > 0),
    [projects],
  );
  const recentActivity = useMemo(
    () => projects.filter((p) => p.lastNotice && p.lastNotice.trim().length > 0),
    [projects],
  );

  return (
    // Was `min-h-screen`: pinning this content wrapper to the full viewport height
    // when it only ever holds a few short cards rendered a large dead void below
    // them (design-critique round 1). The outer App shell already paints
    // `--wf-bg` across the full viewport (see App.tsx), so this container only
    // needs to size to its own content.
    <div className="p-6" style={{ backgroundColor: 'var(--wf-bg)' }}>
      {/* Constrained + left-anchored (not centered with `mx-auto`): a centered
          ~1200px column on a wide viewport with only a few cards symmetrically
          doubles the dead space on both sides (design-critique round 2). */}
      <div style={{ maxWidth: 1200 }}>
        <div className="flex items-center justify-between mb-6">
          <div>
            <h1 className="text-3xl font-bold mb-2" style={{ color: 'var(--wf-fg)' }}>
              Workflow
            </h1>
            <p className="text-sm" style={{ color: 'var(--wf-fg-secondary)' }}>
              {projects.length === 0
                ? 'No projects yet'
                : `${projects.length} project${projects.length === 1 ? '' : 's'} active`}
            </p>
          </div>
          {onSettings && (
            <button
              onClick={onSettings}
              className="flex items-center gap-2 px-4 py-2 rounded border transition-colors hover:bg-[var(--wf-hover)]"
              style={{
                backgroundColor: 'transparent',
                borderColor: 'var(--wf-border)',
                color: 'var(--wf-fg)',
              }}
            >
              <svg width="16" height="16" viewBox="0 0 24 24" fill="none" aria-hidden="true">
                <path
                  d="M12 15a3 3 0 100-6 3 3 0 000 6z"
                  stroke="currentColor"
                  strokeWidth="2"
                  strokeLinecap="round"
                  strokeLinejoin="round"
                />
                <path
                  d="M19.4 15a1.65 1.65 0 00.33 1.82l.06.06a2 2 0 11-2.83 2.83l-.06-.06a1.65 1.65 0 00-1.82-.33 1.65 1.65 0 00-1 1.51V21a2 2 0 11-4 0v-.09a1.65 1.65 0 00-1.08-1.51 1.65 1.65 0 00-1.82.33l-.06.06a2 2 0 11-2.83-2.83l.06-.06a1.65 1.65 0 00.33-1.82 1.65 1.65 0 00-1.51-1H3a2 2 0 110-4h.09a1.65 1.65 0 001.51-1 1.65 1.65 0 00-.33-1.82l-.06-.06a2 2 0 112.83-2.83l.06.06a1.65 1.65 0 001.82.33H9a1.65 1.65 0 001-1.51V3a2 2 0 114 0v.09a1.65 1.65 0 001 1.51 1.65 1.65 0 001.82-.33l.06-.06a2 2 0 112.83 2.83l-.06.06a1.65 1.65 0 00-.33 1.82V9a1.65 1.65 0 001.51 1H21a2 2 0 110 4h-.09a1.65 1.65 0 00-1.51 1z"
                  stroke="currentColor"
                  strokeWidth="2"
                  strokeLinecap="round"
                  strokeLinejoin="round"
                />
              </svg>
              Settings
            </button>
          )}
        </div>

        <HaltIndicator halted={globalHalted} />

        {projects.length === 0 ? (
          <div className="text-center py-12">
            <p style={{ color: 'var(--wf-fg-secondary)' }}>
              Create a project to get started. Use workflow_brief in chat or the panel.
            </p>
          </div>
        ) : (
          <motion.div layout className="grid grid-cols-1 md:grid-cols-2 lg:grid-cols-3 gap-4 items-start">
            <AnimatePresence mode="popLayout">
              {projects.map((project) => (
                <ProjectCard
                  key={project.id}
                  project={project}
                  onClick={() => onProjectClick?.(project.id)}
                />
              ))}
            </AnimatePresence>
          </motion.div>
        )}

        {projects.length > 0 && (
          <div className="grid grid-cols-1 md:grid-cols-2 gap-4 mt-6">
            <div
              className="p-4 rounded-lg border"
              style={{ borderColor: 'var(--wf-border)', backgroundColor: 'var(--wf-bg-secondary)' }}
            >
              <h2
                className="text-xs font-semibold mb-3 uppercase tracking-wide"
                style={{ color: 'var(--wf-fg-secondary)' }}
              >
                Attention needed
              </h2>
              {attentionProjects.length === 0 ? (
                <p className="text-sm" style={{ color: 'var(--wf-fg-secondary)' }}>
                  Nothing needs attention right now.
                </p>
              ) : (
                <div className="flex flex-col gap-2">
                  {attentionProjects.map((p) => (
                    <div
                      key={p.id}
                      className="flex items-center justify-between text-sm cursor-pointer"
                      onClick={() => onProjectClick?.(p.id)}
                    >
                      <span style={{ color: 'var(--wf-fg)' }}>{p.name}</span>
                      <span
                        style={{
                          color: p.phase.kind === 'halted' ? 'var(--wf-accent-pink)' : 'var(--wf-accent-orange)',
                        }}
                      >
                        {p.phase.kind === 'halted' ? 'halted' : `${p.parkedCount} parked`}
                      </span>
                    </div>
                  ))}
                </div>
              )}
            </div>

            <div
              className="p-4 rounded-lg border"
              style={{ borderColor: 'var(--wf-border)', backgroundColor: 'var(--wf-bg-secondary)' }}
            >
              <h2
                className="text-xs font-semibold mb-3 uppercase tracking-wide"
                style={{ color: 'var(--wf-fg-secondary)' }}
              >
                Recent activity across projects
              </h2>
              {recentActivity.length === 0 ? (
                <p className="text-sm" style={{ color: 'var(--wf-fg-secondary)' }}>
                  No recent activity yet.
                </p>
              ) : (
                <div className="flex flex-col gap-2">
                  {recentActivity.map((p) => (
                    <div
                      key={p.id}
                      className="text-sm cursor-pointer"
                      onClick={() => onProjectClick?.(p.id)}
                    >
                      <span className="font-semibold" style={{ color: 'var(--wf-fg)' }}>
                        {p.name}:{' '}
                      </span>
                      <span style={{ color: 'var(--wf-fg-secondary)' }}>{p.lastNotice}</span>
                    </div>
                  ))}
                </div>
              )}
            </div>
          </div>
        )}

        {snapshot && snapshot.truncated && (
          <div className="mt-4 text-xs text-center" style={{ color: 'var(--wf-fg-secondary)' }}>
            Some data was truncated due to size limits. Refresh for full details.
          </div>
        )}
      </div>
    </div>
  );
};

export default Dashboard;
