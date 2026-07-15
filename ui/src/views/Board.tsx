import React, { useEffect, useMemo, useRef, useState } from 'react';
import { AnimatePresence, motion } from 'framer-motion';
import { useStore } from '../store';
import { bridge } from '../bridge';
import { Card, ColumnKey, Task, TaskStateKey } from '../components/Card';
import Drilldown from './Drilldown';
import DepMap from '../components/DepMap';
import TaskDetail from './TaskDetail';
import Prd from './Prd';
import OfficeMap from './OfficeMap';
import ConfirmButton from '../components/ConfirmButton';

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

export interface ChatMsg {
  who: 'user' | 'office';
  text: string;
}

/** Not part of the frozen envelope (PANEL_PROTOCOL.md 2.1 does not serialize the
 * domain model's `outbox` onto `Project`) — same documented gap as `epics`/`stories`
 * below. Kept optional and forward-compatible: OfficeChat.tsx renders an empty
 * notice log today and picks this up automatically the day a later wave adds it. */
export interface OutboundNoticeView {
  id: number;
  text: string;
  sent: boolean;
  paused: boolean;
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
  /** Technical Requirements Document (full snapshot only, 6.2b); authored after web-research
   * in the Drafting pipeline. Rendered alongside the PRD in the 'docs' tab. */
  trdMarkdown?: string;
  /** Web-research findings (full snapshot only, 6.2b); shown collapsed under the docs. */
  researchNotes?: string;
  /** Clean-build Requirement Document (full snapshot only, 6.2c); rendered in the docs tab. */
  crdMarkdown?: string;
  /** The last clean-build audit grade 0-100 (6.2c), or null if never audited. */
  lastAuditGrade?: number | null;
  /** Ungrounded assumptions the safeguard flagged in the last doc gate (6.2c); rendered as an
   * amber strip at the top of the docs tab while the drafting pipeline waits on the user. */
  pendingAssumptions?: string[];
  officeTranscript?: ChatMsg[];
  officeSummary?: string;
  outbox?: OutboundNoticeView[];
  /** Fixed-staff liveness for the office view (6.2b/6.2c): whether the project-level
   * researcher / clean-build auditor sub-agent is currently in flight. Full mode only. */
  researchActive?: boolean;
  auditActive?: boolean;
  /** Raw binding presence the mock harness carries; the office view treats either as "live". */
  research?: unknown;
  audit?: unknown;
  /** Config subset the office view reads — `maxWorkers` chooses the office layout tier. */
  config?: { maxWorkers?: number | null };
  /** Live office activity (full snapshot only, 6.2d), present only while an activity is in
   * flight; omitted entirely (not null) when idle. At most one is live at a time. */
  officeActivity?: { label: string; sinceMs: number } | null;
}

const COLUMNS: { key: ColumnKey; label: string }[] = [
  { key: 'backlog', label: 'Backlog' },
  { key: 'todo', label: 'Todo' },
  { key: 'onprogress', label: 'In Progress' },
  { key: 'review', label: 'Review' },
  { key: 'done', label: 'Done' },
];

const PHASE_COLOR: Record<ProjectPhase['kind'], string> = {
  drafting: 'var(--wf-status-drafting)',
  ready: 'var(--wf-accent)',
  running: 'var(--wf-status-running)',
  interrupted: 'var(--wf-status-parked)',
  halted: 'var(--wf-status-blocked)',
  done: 'var(--wf-status-done)',
};

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
  onSettings?: () => void;
  /** Deep-link support (`?view=board|drilldown|task|office|office-map`, see App.tsx): which
   * tab to land on. Defaults to `'office'` — the pixel virtual office is the default project
   * view. */
  initialTab?: Tab;
  /** Deep-link support: pre-select a task's detail panel (`?view=task`) on mount. */
  initialTaskId?: string;
}

type Tab = 'office' | 'board' | 'drilldown' | 'depmap' | 'prd';

export const Board: React.FC<BoardProps> = ({ projectId, onBack, onSettings: _onSettings, initialTab, initialTaskId }) => {
  const rawProject = useStore((s) => s.getProject(projectId));
  const project = rawProject as unknown as Project | undefined;

  const [tab, setTab] = useState<Tab>(initialTab ?? 'office');
  const [dragTaskId, setDragTaskId] = useState<string | null>(null);
  const [dragOverColumn, setDragOverColumn] = useState<ColumnKey | null>(null);
  const [toast, setToast] = useState<string | null>(null);
  const [selectedTaskId, setSelectedTaskId] = useState<string | null>(initialTaskId ?? null);
  /** A drop that needs the kill-worker confirmation (running -> todo). Rendered as
   * an inline flat confirm strip — window.confirm does not exist in wry. */
  const [pendingKillMove, setPendingKillMove] = useState<{ taskId: string; to: ColumnKey } | null>(null);

  useEffect(() => {
    // See Dashboard.tsx's identical fix: call the store action directly (exactly one
    // zustand `set`) rather than wrapping it in `useStore.setState((state) => {...;
    // return state})`, which double-applies `setState` and silently reverts every push
    // back to stale state.
    const unsubscribe = bridge.onSnapshot((snap) => {
      useStore.getState().updateSnapshot(snap);
    });
    bridge.state().catch((err) => console.error('Failed to refresh state:', err));
    return unsubscribe;
  }, []);

  useEffect(() => {
    if (!toast) return;
    const t = setTimeout(() => setToast(null), 3500);
    return () => clearTimeout(t);
  }, [toast]);

  // `initialTaskId` resolves asynchronously (App picks it after the first snapshot),
  // usually AFTER this component mounted with `undefined` — a `useState` initial
  // value alone would silently ignore the deep link. Apply it whenever it arrives.
  useEffect(() => {
    if (initialTaskId) setSelectedTaskId(initialTaskId);
  }, [initialTaskId]);

  // Reset the task-detail selection on a genuine project switch, but not on the
  // initial mount — otherwise a deep-linked `initialTaskId` (?view=task) would be
  // wiped out by this same effect firing once for free on mount.
  const mountedProjectId = useRef(projectId);
  useEffect(() => {
    if (mountedProjectId.current === projectId) return;
    mountedProjectId.current = projectId;
    setSelectedTaskId(null);
    setPendingKillMove(null);
  }, [projectId]);

  const selectedTask = useMemo(
    () => project?.tasks.find((t) => t.id === selectedTaskId),
    [project, selectedTaskId],
  );

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

  const sendMove = async (taskId: string, to: ColumnKey, killWorker: boolean) => {
    try {
      const res = await bridge.send({ op: 'card_move', task: taskId, to, killWorker });
      if (res?.error) setToast(res.error);
    } catch (err) {
      setToast(err instanceof Error ? err.message : 'card move failed');
    }
  };

  const handleDrop = async (to: ColumnKey) => {
    setDragOverColumn(null);
    const taskId = dragTaskId;
    setDragTaskId(null);
    if (!taskId || !project) return;
    const task = project.tasks.find((t) => t.id === taskId);
    if (!task) return;

    if (task.state === 'onprogress' && to === 'todo') {
      // needs the kill-worker confirmation — arm the inline strip instead of
      // a native dialog that does not exist in wry
      setPendingKillMove({ taskId, to });
      return;
    }

    const guard = guardCardMove(task.state, to, false);
    if (!guard.legal) {
      setToast(guard.reason ?? 'that move is not allowed');
      return;
    }
    await sendMove(taskId, to, false);
  };

  const runInterrupt = async (mode: 'hard' | 'soft') => {
    if (!project) return;
    try {
      const res = await bridge.send({ op: 'interrupt', project: project.id, mode });
      if (res?.error) setToast(res.error);
    } catch (err) {
      setToast(err instanceof Error ? err.message : 'interrupt failed');
    }
  };

  const runResume = async () => {
    if (!project) return;
    try {
      const res = await bridge.send({ op: 'resume', project: project.id });
      if (res?.error) setToast(res.error);
    } catch (err) {
      setToast(err instanceof Error ? err.message : 'resume failed');
    }
  };

  if (!project) {
    return (
      <div style={{ padding: '2rem', color: 'var(--wf-dim)' }}>
        <button onClick={onBack} className="wf-btn wf-btn-ghost" style={{ paddingLeft: 0 }}>
          &larr; dashboard
        </button>
        <p>Loading project…</p>
      </div>
    );
  }

  const phaseKind = project.phase.kind;
  const halted = phaseKind === 'halted';
  const pendingKillTask = pendingKillMove ? project.tasks.find((t) => t.id === pendingKillMove.taskId) : undefined;

  return (
    <div style={{ minHeight: '100vh', padding: '1.25rem 1.5rem', background: 'var(--wf-bg)' }}>
      <div style={{ maxWidth: 1500, margin: '0 auto' }}>
        <div
          style={{
            display: 'flex',
            alignItems: 'center',
            justifyContent: 'space-between',
            paddingBottom: '0.6rem',
            borderBottom: '1px solid var(--wf-head)',
          }}
        >
          <div style={{ display: 'flex', alignItems: 'baseline', gap: '0.75rem', minWidth: 0 }}>
            <button onClick={onBack} className="wf-btn wf-btn-ghost" style={{ paddingLeft: 0 }}>
              &larr;
            </button>
            <h1 style={{ color: 'var(--wf-fg)', fontSize: '1rem', fontWeight: 700, margin: 0 }} className="truncate">
              {project.name}
            </h1>
            <span className="wf-status" style={{ color: PHASE_COLOR[phaseKind], flex: 'none', whiteSpace: 'nowrap' }}>
              <span className="wf-status-dot" style={{ background: PHASE_COLOR[phaseKind] }} />
              {phaseKind}
            </span>
          </div>
          {/* Phase-dependent actions: one set at a time, never three alarm
              buttons side by side for every state. flex:none so a long project
              name can never crush the buttons. */}
          <div style={{ display: 'flex', gap: '0.5rem', flex: 'none' }}>
            {phaseKind === 'running' && (
              <React.Fragment>
                <ConfirmButton label="drain" className="wf-btn" onConfirm={() => void runInterrupt('soft')} testId="drain-btn" />
                <ConfirmButton label="interrupt" className="wf-btn wf-btn-danger" onConfirm={() => void runInterrupt('hard')} testId="interrupt-btn" />
              </React.Fragment>
            )}
            {phaseKind === 'interrupted' && (
              <ConfirmButton label="resume" className="wf-btn wf-btn-accent" onConfirm={() => void runResume()} testId="resume-btn" />
            )}
          </div>
        </div>

        {halted && (
          <div
            style={{
              marginTop: '0.75rem',
              padding: '0.45rem 0.6rem',
              borderLeft: '2px solid var(--wf-error)',
              background: 'var(--wf-tint-error)',
              color: 'var(--wf-error)',
              fontSize: '0.8rem',
            }}
          >
            Halted: {project.phase.reason ?? 'a parked task blocks all remaining work'} — unpark the blocking task to resume.
          </div>
        )}

        {pendingKillMove && pendingKillTask && (
          <div
            style={{
              marginTop: '0.75rem',
              padding: '0.45rem 0.6rem',
              borderLeft: '2px solid var(--wf-warn)',
              background: 'var(--wf-tint-warn)',
              color: 'var(--wf-fg)',
              fontSize: '0.8rem',
              display: 'flex',
              alignItems: 'center',
              gap: '0.75rem',
            }}
          >
            <span>
              Requeue <strong>{pendingKillTask.title}</strong>? Its running worker will be killed.
            </span>
            <button
              className="wf-btn wf-btn-danger"
              data-testid="confirm-kill-move"
              onClick={() => {
                const mv = pendingKillMove;
                setPendingKillMove(null);
                void sendMove(mv.taskId, mv.to, true);
              }}
            >
              kill worker &amp; requeue
            </button>
            <button className="wf-btn wf-btn-ghost" onClick={() => setPendingKillMove(null)}>
              cancel
            </button>
          </div>
        )}

        {/* koma-flat tabs: text + active underline, no boxes */}
        <div style={{ display: 'flex', gap: '1.1rem', margin: '0.75rem 0 1rem', borderBottom: '1px solid var(--wf-border)' }}>
          {(['office', 'board', 'drilldown', 'depmap', 'prd'] as Tab[]).map((t) => (
            <button
              key={t}
              onClick={() => setTab(t)}
              style={{
                padding: '0.35rem 0.1rem',
                fontSize: '0.8rem',
                color: tab === t ? 'var(--wf-fg)' : 'var(--wf-dim)',
                borderBottom: tab === t ? '2px solid var(--wf-accent)' : '2px solid transparent',
                marginBottom: -1,
                borderRadius: 0,
              }}
            >
              {t === 'office' ? 'office' : t === 'board' ? 'board' : t === 'drilldown' ? 'drilldown' : t === 'depmap' ? 'dependencies' : 'docs'}
            </button>
          ))}
        </div>

        {tab === 'office' && <OfficeMap project={project} onTaskClick={(id) => setSelectedTaskId(id)} />}

        {tab === 'board' && (
          <React.Fragment>
          {/* Flat columns: no background panel per column — a small header with a
              hairline rule, cards below. The drop target affordance is a dashed
              hairline around the column area while dragging.
              The page keeps overflow-x hidden (drawer fix in index.css), so the
              BOARD ITSELF is the horizontal scroll container on narrow viewports —
              without this, columns past the viewport edge were simply unreachable. */}
          <div style={{ overflowX: 'auto', paddingBottom: '0.5rem' }}>
          <div
            style={{
              display: 'grid',
              gridTemplateColumns: 'repeat(5, minmax(220px, 1fr))',
              gap: '1rem',
              alignItems: 'start',
              minWidth: 1180,
            }}
          >
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
                  minHeight: 200,
                  borderRadius: 'var(--wf-radius)',
                  outline: dragOverColumn === col.key ? '1px dashed var(--wf-grip)' : 'none',
                  outlineOffset: 4,
                }}
              >
                <div
                  style={{
                    fontSize: '0.68rem',
                    fontWeight: 600,
                    letterSpacing: '0.08em',
                    textTransform: 'uppercase',
                    color: 'var(--wf-dim)',
                    paddingBottom: '0.35rem',
                    marginBottom: '0.6rem',
                    borderBottom: '1px solid var(--wf-head)',
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
                        onClick={(t) => setSelectedTaskId(t.id)}
                      />
                    ))}
                  </AnimatePresence>
                </div>
              </div>
            ))}
          </div>
          </div>

          </React.Fragment>
        )}

        {tab === 'drilldown' && <Drilldown project={project} />}

        {tab === 'depmap' && (
          <DepMap
            tasks={project.tasks.map((t) => ({ id: t.id, title: t.title, state: t.state, blockedBy: t.blockedBy }))}
            halted={halted}
            onTaskClick={(id) => setSelectedTaskId(id)}
          />
        )}

        {tab === 'prd' && <Prd project={project} />}

        {/* Drawer lives OUTSIDE the tab switch so a depmap node click opens it too. */}
        <AnimatePresence>
          {selectedTaskId && selectedTask && (
            <React.Fragment>
              {/* Overlay + dim, not a widened layout row: this drawer used to sit
                  in a flex row next to the column grid, whose combined width could
                  exceed the viewport and clip the drawer's own content while adding
                  a page-level horizontal scrollbar (design-critique round 2). It is
                  now a fixed panel (see TaskDetail.tsx) with a dimming backdrop. */}
              <motion.div
                initial={{ opacity: 0 }}
                animate={{ opacity: 1 }}
                exit={{ opacity: 0 }}
                transition={{ duration: 0.15 }}
                onClick={() => setSelectedTaskId(null)}
                style={{
                  position: 'fixed',
                  inset: 0,
                  background: 'rgba(0, 0, 0, 0.5)',
                  zIndex: 30,
                }}
              />
              <TaskDetail task={selectedTask} onClose={() => setSelectedTaskId(null)} />
            </React.Fragment>
          )}
        </AnimatePresence>

        {toast && (
          <motion.div
            initial={{ opacity: 0, y: 10 }}
            animate={{ opacity: 1, y: 0 }}
            exit={{ opacity: 0 }}
            style={{
              position: 'fixed',
              bottom: '1.25rem',
              right: '1.25rem',
              background: 'var(--wf-panel)',
              borderLeft: '2px solid var(--wf-warn)',
              border: '1px solid var(--wf-border)',
              color: 'var(--wf-fg)',
              borderRadius: 'var(--wf-radius)',
              padding: '0.6rem 0.9rem',
              fontSize: '0.8rem',
            }}
          >
            {toast}
          </motion.div>
        )}
      </div>
    </div>
  );
};

export default Board;
