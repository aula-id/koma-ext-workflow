import React, { useEffect, useState } from 'react';
import { motion, AnimatePresence } from 'framer-motion';
import { useStore, Project } from '../store';
import { bridge } from '../bridge';

interface DashboardProps {
  onProjectClick?: (projectId: string) => void;
  onSettings?: () => void;
}

const ProgressRing: React.FC<{
  done: number;
  total: number;
  size?: number;
}> = ({ done, total, size = 40 }) => {
  const radius = size / 2 - 2;
  const circumference = 2 * Math.PI * radius;
  const offset = circumference - (done / Math.max(total, 1)) * circumference;

  return (
    <svg width={size} height={size} className="inline-block">
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
        stroke="var(--wf-accent-green)"
        strokeWidth="2"
        strokeDasharray={circumference}
        strokeDashoffset={offset}
        strokeLinecap="round"
        animate={{ strokeDashoffset: offset }}
        transition={{ duration: 0.5 }}
      />
      <text
        x={size / 2}
        y={size / 2}
        textAnchor="middle"
        dy="0.3em"
        fontSize="10"
        fill="var(--wf-fg)"
      >
        {done}/{total}
      </text>
    </svg>
  );
};

const ProjectCard: React.FC<{
  project: Project;
  onClick?: () => void;
}> = ({ project, onClick }) => {
  const phaseColors: Record<string, string> = {
    drafting: 'bg-gray-600',
    ready: 'bg-blue-600',
    running: 'bg-green-600',
    interrupted: 'bg-yellow-600',
    halted: 'bg-red-600',
    done: 'bg-green-800',
  };

  const phaseKind = project.phase.kind;
  const phaseColor = phaseColors[phaseKind] || 'bg-gray-600';

  return (
    <motion.div
      layout
      initial={{ opacity: 0, scale: 0.95 }}
      animate={{ opacity: 1, scale: 1 }}
      exit={{ opacity: 0, scale: 0.95 }}
      transition={{ duration: 0.2 }}
      onClick={onClick}
      className="p-4 rounded-lg border border-gray-700 bg-gray-900 cursor-pointer hover:bg-gray-800 transition-colors"
    >
      <div className="flex items-start justify-between mb-3">
        <div className="flex-1">
          <h3 className="text-lg font-semibold text-white truncate">{project.name}</h3>
          <span
            className={`inline-block mt-1 px-2 py-1 text-xs font-semibold rounded ${phaseColor} text-white capitalize`}
          >
            {phaseKind}
          </span>
        </div>
        <div className="ml-2">
          <ProgressRing done={project.doneCount || 0} total={project.taskCount || 0} />
        </div>
      </div>

      <div className="grid grid-cols-3 gap-2 mb-3 text-sm">
        <div className="bg-gray-800 p-2 rounded">
          <div className="text-xs text-gray-400">Running</div>
          <div className="text-lg font-bold text-blue-400">{project.runningCount || 0}</div>
        </div>
        <div className="bg-gray-800 p-2 rounded">
          <div className="text-xs text-gray-400">Parked</div>
          <div className="text-lg font-bold text-orange-400">{project.parkedCount || 0}</div>
        </div>
        <div className="bg-gray-800 p-2 rounded">
          <div className="text-xs text-gray-400">Total</div>
          <div className="text-lg font-bold text-gray-300">{project.taskCount || 0}</div>
        </div>
      </div>

      {project.lastNotice && (
        <div className="bg-gray-800 p-2 rounded text-xs text-gray-300 truncate">
          {project.lastNotice}
        </div>
      )}
    </motion.div>
  );
};

const HaltIndicator: React.FC<{ halted: boolean }> = ({ halted }) => {
  if (!halted) return null;

  return (
    <motion.div
      initial={{ opacity: 0, y: -10 }}
      animate={{ opacity: 1, y: 0 }}
      className="bg-red-900 border border-red-700 p-3 rounded-lg mb-4 text-red-200 text-sm"
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
    <div className="min-h-screen p-6" style={{ backgroundColor: 'var(--wf-bg)' }}>
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
              className="px-4 py-2 rounded transition-colors"
              style={{
                backgroundColor: 'var(--wf-bg-secondary)',
                color: 'var(--wf-fg)',
              }}
            >
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
          <motion.div layout className="grid grid-cols-1 md:grid-cols-2 lg:grid-cols-3 gap-4">
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
          <div className="mt-4 text-xs text-gray-500 text-center">
            Some data was truncated due to size limits. Refresh for full details.
          </div>
        )}
      </div>
    </div>
  );
};

export default Dashboard;
