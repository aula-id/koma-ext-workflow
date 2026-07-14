import React, { useEffect, useMemo, useState } from 'react';
import { AnimatePresence, motion } from 'framer-motion';
import { useStore } from '../store';
import { bridge } from '../bridge';
import { Card, ColumnKey, Task, TaskStateKey } from '../components/Card';
import Drilldown from './Drilldown';
import DepMap from '../components/DepMap';

/** Project shape, full mode, per docs/PANEL_PROTOCOL.md 2.1 (frozen W7 contract). */
export interface ProjectPhase {
  kind: 'drafting' | 'ready' | 'running' | 'interrupted' | 'halted' | 'done';
  reason?: string;
  atMs?: number;
}

export interface Epic {
  id: string;
  title: string;
  stories: string[];
}

export interface Story {
  id: string;
  title: string;
  tasks: string[];
}

export interface Project {
  id: string;
  name: string;
  phase: ProjectPhase;
  deliveryPath: string | null;
  seq: number;
  tasks: Task[];
  epics?: Epic[];
  stories?: Story[];
  prdMarkdown?: string;
}

const COLUMNS: { key: ColumnKey; label: string }[] = [
  { key: 'backlog', label: 'Backlog' },
  { key: 'todo', label: 'Todo' },
  { key: 'onprogress', label: 'In Progress' },
  { key: 'review', label: 'Review' },
  { key: 'done', label: 'Done' },
];

export interface MoveGuardResult {
  legal: boolean;
  requiresKillWorker?: boolean;
  reason?: string;
}

/**
 * Client-side legal-move guard, mirroring the user-drivable edges of office-core
 * machine.rs's TaskTransition (Groom / Unpark are user intents; Dispatch/Complete/
 * Pass/Bounce are kernel- or agent-driven and not offered as manual drags). The
 * kernel remains the authority (`card_move` is still validated server-side once a
 * later wave wires it into the kernel) — this guard only pre-filters obviously
 * illegal drops for immediate UX feedback.
 */
const LEGAL_TARGETS: Record<TaskStateKey, ColumnKey[]> = {
  backlog: ['todo'],
  todo: [],
  onprogress: ['review', 'todo'],
  review: ['done', 'todo'],
  parked: ['todo'],
  done: [],
};

export function guardCardMove(state: TaskStateKey, to: ColumnKey, killWorker: boolean): MoveGuardResult {
  const targets = LEGAL_TARGETS[state] ?? [];
  if (!targets.includes(to)) {
    return { legal: false, reason: `cannot move a ${state} task to ${to}` };
  }
  if (state === 'onprogress' && to === 'todo' && !killWorker) {
    return {
      legal: false,
      requiresKillWorker: true,
      reason: 'moving a running task back to todo requires killing the worker first',
    };
  }
  return { legal: true };
}

export interface BoardProps {
  projectId: string;
  onBack?: () => void;
}

type Tab = 'board' | 'drilldown' | 'depmap';

export const Board: React.FC<BoardProps> = ({ projectId, onBack }) => {
  const rawProject = useStore((s) => s.getProject(projectId));
  const project = rawProject as unknown as Project | undefined;

  const [tab, setTab] = useState<Tab>('board');
  const [dragTaskId, setDragTaskId] = useState<string | null>(null);
  const [dragOverColumn, setDragOverColumn] = useState<ColumnKey | null>(null);
  const [toast, setToast] = useState<string | null>(null);

  useEffect(() => {
    const unsubscribe = bridge.onSnapshot((snap) => {
      useStore.setState((state) => {
        state.updateSnapshot(snap);
        return state;
      });
    });
    bridge.state().catch((err) => console.error('Failed to refresh state:', err));
    return unsubscribe;
  }, []);

  useEffect(() => {
    if (!toast) return;
    const t = setTimeout(() => setToast(null), 3500);
    return () => clearTimeout(t);
  }, [toast]);

  const tasksByColumn = useMemo(() => {
    const grouped: Record<ColumnKey, Task[]> = {
      backlog: [],
      todo: [],
      onprogress: [],
      review: [],
      done: [],
    };
    for (const t of project?.tasks ?? []) {
      (grouped[t.column] ?? grouped.backlog).push(t);
    }
    for (const key of Object.keys(grouped) as ColumnKey[]) {
      grouped[key] = grouped[key].slice().sort((a, b) => b.priority - a.priority || a.id.localeCompare(b.id));
    }
    return grouped;
  }, [project]);

  const handleDrop = async (to: ColumnKey) => {
    setDragOverColumn(null);
    const taskId = dragTaskId;
    setDragTaskId(null);
    if (!taskId || !project) return;
    const task = project.tasks.find((t) => t.id === taskId);
    if (!task) return;

    let killWorker = false;
    if (task.state === 'onprogress' && to === 'todo') {
      killWorker = window.confirm('Kill the running worker and requeue this task?');
      if (!killWorker) return;
    }

    const guard = guardCardMove(task.state, to, killWorker);
    if (!guard.legal) {
      setToast(guard.reason ?? 'that move is not allowed');
      return;
    }

    try {
      const res = await bridge.send({ op: 'card_move', task: taskId, to, killWorker });
      if (res?.error) setToast(res.error);
    } catch (err) {
      setToast(err instanceof Error ? err.message : 'card move failed');
    }
  };

  const runInterrupt = async (mode: 'hard' | 'soft') => {
    if (!project) return;
    const label = mode === 'hard' ? 'Interrupt (hard-kill all workers)' : 'Drain (soft, finish in-flight)';
    if (!window.confirm(`${label} for "${project.name}"?`)) return;
    try {
      const res = await bridge.send({ op: 'interrupt', project: project.id, mode });
      if (res?.error) setToast(res.error);
    } catch (err) {
      setToast(err instanceof Error ? err.message : 'interrupt failed');
    }
  };

  const runResume = async () => {
    if (!project) return;
    if (!window.confirm(`Resume "${project.name}"?`)) return;
    try {
      const res = await bridge.send({ op: 'resume', project: project.id });
      if (res?.error) setToast(res.error);
    } catch (err) {
      setToast(err instanceof Error ? err.message : 'resume failed');
    }
  };

  if (!project) {
    return (
      <div style={{ padding: '2rem', color: 'var(--wf-fg-secondary)' }}>
        <button onClick={onBack} style={backButtonStyle}>
          &larr; Dashboard
        </button>
        <p>Loading project…</p>
      </div>
    );
  }

  const halted = project.phase.kind === 'halted';

  return (
    <div style={{ minHeight: '100vh', padding: '1.5rem', background: 'var(--wf-bg)' }}>
      <div style={{ maxWidth: 1400, margin: '0 auto' }}>
        <div style={{ display: 'flex', alignItems: 'center', justifyContent: 'space-between', marginBottom: '0.75rem' }}>
          <div>
            <button onClick={onBack} style={backButtonStyle}>
              &larr; Dashboard
            </button>
            <h1 style={{ color: 'var(--wf-fg)', fontSize: '1.5rem', fontWeight: 700, margin: '0.25rem 0 0' }}>
              {project.name}
            </h1>
            <span style={{ fontSize: '0.75rem', color: 'var(--wf-fg-secondary)' }}>{project.phase.kind}</span>
          </div>
          <div style={{ display: 'flex', gap: '0.5rem' }}>
            <button onClick={() => runInterrupt('hard')} style={dangerButtonStyle}>
              Interrupt
            </button>
            <button onClick={() => runInterrupt('soft')} style={warnButtonStyle}>
              Drain
            </button>
            <button onClick={runResume} style={primaryButtonStyle}>
              Resume
            </button>
          </div>
        </div>

        {halted && (
          <div
            style={{
              background: 'var(--wf-bg-secondary)',
              border: '1px solid var(--wf-accent-pink)',
              color: 'var(--wf-accent-pink)',
              borderRadius: 'var(--wf-radius)',
              padding: '0.6rem 0.8rem',
              fontSize: '0.8rem',
              marginBottom: '0.75rem',
            }}
          >
            Halted: {project.phase.reason ?? 'a parked task blocks all work'}
          </div>
        )}

        <div style={{ display: 'flex', gap: '0.5rem', marginBottom: '1rem' }}>
          {(['board', 'drilldown', 'depmap'] as Tab[]).map((t) => (
            <button key={t} onClick={() => setTab(t)} style={tab === t ? tabActiveStyle : tabStyle}>
              {t === 'board' ? 'Board' : t === 'drilldown' ? 'Drilldown' : 'Dependency Map'}
            </button>
          ))}
        </div>

        {tab === 'board' && (
          <div style={{ display: 'grid', gridTemplateColumns: 'repeat(5, minmax(220px, 1fr))', gap: '0.75rem' }}>
            {COLUMNS.map((col) => (
              <div
                key={col.key}
                onDragOver={(e) => {
                  e.preventDefault();
                  setDragOverColumn(col.key);
                }}
                onDragLeave={() => setDragOverColumn((c) => (c === col.key ? null : c))}
                onDrop={(e) => {
                  e.preventDefault();
                  void handleDrop(col.key);
                }}
                style={{
                  background: 'var(--wf-bg-secondary)',
                  borderRadius: 'var(--wf-radius)',
                  padding: '0.6rem',
                  minHeight: 200,
                  border:
                    dragOverColumn === col.key
                      ? '1px dashed var(--wf-accent-blue)'
                      : '1px solid transparent',
                }}
              >
                <div
                  style={{
                    fontSize: '0.75rem',
                    fontWeight: 700,
                    color: 'var(--wf-fg-secondary)',
                    marginBottom: '0.5rem',
                    display: 'flex',
                    justifyContent: 'space-between',
                  }}
                >
                  <span>{col.label}</span>
                  <span>{tasksByColumn[col.key].length}</span>
                </div>
                <div style={{ display: 'flex', flexDirection: 'column', gap: '0.5rem' }}>
                  <AnimatePresence initial={false}>
                    {tasksByColumn[col.key].map((task) => (
                      <Card
                        key={task.id}
                        task={task}
                        draggable
                        onDragStart={(t, e) => {
                          setDragTaskId(t.id);
                          e.dataTransfer?.setData('text/plain', t.id);
                        }}
                      />
                    ))}
                  </AnimatePresence>
                </div>
              </div>
            ))}
          </div>
        )}

        {tab === 'drilldown' && <Drilldown project={project} />}

        {tab === 'depmap' && (
          <DepMap
            tasks={project.tasks.map((t) => ({ id: t.id, title: t.title, state: t.state, blockedBy: t.blockedBy }))}
            halted={halted}
          />
        )}

        {toast && (
          <motion.div
            initial={{ opacity: 0, y: 10 }}
            animate={{ opacity: 1, y: 0 }}
            exit={{ opacity: 0 }}
            style={{
              position: 'fixed',
              bottom: '1.25rem',
              right: '1.25rem',
              background: 'var(--wf-bg-secondary)',
              border: '1px solid var(--wf-accent-orange)',
              color: 'var(--wf-fg)',
              borderRadius: 'var(--wf-radius)',
              padding: '0.6rem 0.9rem',
              fontSize: '0.8rem',
              boxShadow: 'var(--wf-shadow)',
            }}
          >
            {toast}
          </motion.div>
        )}
      </div>
    </div>
  );
};

const buttonBase: React.CSSProperties = {
  fontSize: '0.75rem',
  fontWeight: 600,
  borderRadius: 'var(--wf-radius)',
  padding: '0.4rem 0.75rem',
  border: '1px solid transparent',
  cursor: 'pointer',
  background: 'var(--wf-bg-secondary)',
  color: 'var(--wf-fg)',
};

const backButtonStyle: React.CSSProperties = {
  ...buttonBase,
  background: 'transparent',
  color: 'var(--wf-fg-secondary)',
  padding: '0.2rem 0',
};

const dangerButtonStyle: React.CSSProperties = {
  ...buttonBase,
  border: '1px solid var(--wf-accent-pink)',
  color: 'var(--wf-accent-pink)',
};

const warnButtonStyle: React.CSSProperties = {
  ...buttonBase,
  border: '1px solid var(--wf-accent-orange)',
  color: 'var(--wf-accent-orange)',
};

const primaryButtonStyle: React.CSSProperties = {
  ...buttonBase,
  border: '1px solid var(--wf-accent-green)',
  color: 'var(--wf-accent-green)',
};

const tabStyle: React.CSSProperties = {
  ...buttonBase,
  background: 'transparent',
  color: 'var(--wf-fg-secondary)',
  border: '1px solid var(--wf-bg-secondary)',
};

const tabActiveStyle: React.CSSProperties = {
  ...buttonBase,
  background: 'var(--wf-bg-secondary)',
  color: 'var(--wf-accent-blue)',
  border: '1px solid var(--wf-accent-blue)',
};

export default Board;
