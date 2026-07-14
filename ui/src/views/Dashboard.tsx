import React, { useEffect, useState } from 'react';
import { motion, AnimatePresence } from 'framer-motion';
import { useStore, Project } from '../store';
import { bridge } from '../bridge';

interface DashboardProps {
  onProjectClick?: (projectId: string) => void;
  onSettings?: () => void;
}

/**
 * Task-completion ring. `running` gates both the color and the semantics: a
 * colored, "live" ring only appears while the project is actually running —
 * showing the same active-green arc on a Halted or still-empty Drafting project
 * (as the old always-green version did) reads as a stuck/broken loading spinner.
 * Non-running projects get a static neutral ring instead.
 *
 * The done/total fraction used to be baked into the SVG as visible `<text>`,
 * which (a) duplicated the "Total" stat tile below and (b) routinely overflowed
 * the ring's own bounds at this size, making it look like loose text floating
 * next to a broken arc. It's exposed as an accessible label instead — the
 * numbers still live in the stat tiles, once, not twice.
 */
const ProgressRing: React.FC<{
  done: number;
  total: number;
  running: boolean;
  size?: number;
}> = ({ done, total, running, size = 40 }) => {
  const radius = size / 2 - 2;
  const circumference = 2 * Math.PI * radius;
  const offset = circumference - (done / Math.max(total, 1)) * circumference;
  const ringColor = running ? 'var(--wf-accent-green)' : 'var(--wf-fg-secondary)';
  const label = `${done} of ${total} task${total === 1 ? '' : 's'} complete`;

  return (
    <svg width={size} height={size} className="inline-block" role="img" aria-label={label}>
      <title>{label}</title>
      <circle
        cx={size / 2}
        cy={size / 2}
        r={radius}
        fill="none"
        stroke="var(--wf-bg-secondary)"
        strokeWidth="2"
      />
      <motion.circle
        cx={size / 2}
        cy={size / 2}
        r={radius}
        fill="none"
        stroke={ringColor}
        strokeWidth="2"
        strokeDasharray={circumference}
        strokeDashoffset={offset}
        strokeLinecap="round"
        animate={{ strokeDashoffset: offset }}
        transition={{ duration: 0.5 }}
      />
    </svg>
  );
};

// Project phase badge colors. `running` intentionally uses the same green as
// `done`/success rather than the koma "info" blue role: this is the single
// semantic-color source for a running project across the whole card — the
// phase badge, the progress ring (see ProgressRing's `running` prop) and the
// "Running" stat tile below all read from here (or its literal value) so the
// same state never shows up in two colors on one card. interrupted/parked read
// as the "amber" bucket, halted/blocked as the "red" bucket (accent-pink is
// koma's error role, i.e. red), drafting has no strong status yet so it stays
// neutral, ready keeps the purple accent as the actionable/next-step color.
const PHASE_COLOR_VAR: Record<string, string> = {
  drafting: 'var(--wf-fg-secondary)',
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
  const isNeutralPhase = phaseColorVar === 'var(--wf-fg-secondary)';

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
              backgroundColor: phaseColorVar,
              color: isNeutralPhase ? 'var(--wf-fg)' : 'var(--wf-bg)',
            }}
          >
            {phaseKind}
          </span>
        </div>
        <div className="ml-2">
          <ProgressRing
            done={project.doneCount || 0}
            total={project.taskCount || 0}
            running={isRunning}
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

  return (
    // Was `min-h-screen`: pinning this content wrapper to the full viewport height
    // when it only ever holds a few short cards rendered a large dead void below
    // them (design-critique round 1). The outer App shell already paints
    // `--wf-bg` across the full viewport (see App.tsx), so this container only
    // needs to size to its own content.
    <div className="p-6" style={{ backgroundColor: 'var(--wf-bg)' }}>
      <div className="max-w-7xl mx-auto">
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
