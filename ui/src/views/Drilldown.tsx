import React, { useMemo, useState } from 'react';
import { motion, AnimatePresence } from 'framer-motion';
import type { Task } from '../components/Card';
import type { Project } from './Board';

export interface DrilldownStoryNode {
  id: string;
  title: string;
  tasks: Task[];
  done: number;
  total: number;
}

export interface DrilldownEpicNode {
  id: string;
  title: string;
  stories: DrilldownStoryNode[];
  done: number;
  total: number;
}

export interface DrilldownTree {
  epics: DrilldownEpicNode[];
  ungrouped: Task[];
  done: number;
  total: number;
}

/**
 * Builds the project -> epic -> story -> task tree from whatever the snapshot
 * actually carries. NOTE: the frozen panel protocol (docs/PANEL_PROTOCOL.md 2.1)
 * does not currently serialize `epics`/`stories` onto the pushed Project (only
 * `tasks`) even though the domain model (office-core domain.rs) has them — this is
 * a genuine cross-wave gap, not something this UI-only wave can fix (would mean
 * editing the frozen daemon digest, out of scope). The tree degrades gracefully:
 * when epics/stories are absent (today), every task falls into `ungrouped` and the
 * view renders a flat rollup; the nested tree activates automatically the day a
 * later wave adds the fields to the snapshot.
 */
export function buildDrilldownTree(project: Pick<Project, 'tasks' | 'epics' | 'stories'>): DrilldownTree {
  const tasks = project.tasks ?? [];
  const taskById = new Map(tasks.map((t) => [t.id, t]));
  const storyById = new Map((project.stories ?? []).map((s) => [s.id, s]));
  const usedTaskIds = new Set<string>();

  const epics: DrilldownEpicNode[] = (project.epics ?? []).map((epic) => {
    const stories: DrilldownStoryNode[] = (epic.stories ?? []).map((storyId) => {
      const story = storyById.get(storyId);
      const storyTasks = (story?.tasks ?? [])
        .map((tid) => taskById.get(tid))
        .filter((t): t is Task => Boolean(t));
      storyTasks.forEach((t) => usedTaskIds.add(t.id));
      const done = storyTasks.filter((t) => t.state === 'done').length;
      return { id: storyId, title: story?.title ?? storyId, tasks: storyTasks, done, total: storyTasks.length };
    });
    const done = stories.reduce((sum, s) => sum + s.done, 0);
    const total = stories.reduce((sum, s) => sum + s.total, 0);
    return { id: epic.id, title: epic.title ?? epic.id, stories, done, total };
  });

  const ungrouped = tasks.filter((t) => !usedTaskIds.has(t.id));
  const done = tasks.filter((t) => t.state === 'done').length;

  return { epics, ungrouped, done, total: tasks.length };
}

const RollupBadge: React.FC<{ done: number; total: number }> = ({ done, total }) => (
  <span
    style={{
      fontSize: '0.65rem',
      fontWeight: 600,
      color: done === total && total > 0 ? 'var(--wf-status-done)' : 'var(--wf-fg-secondary)',
      border: '1px solid var(--wf-fg-secondary)',
      borderRadius: 'var(--wf-radius)',
      padding: '0.05rem 0.4rem',
    }}
  >
    {done}/{total}
  </span>
);

// Design-critique round 2: rows used to be `justify-content: space-between` with
// only a title on the left and a status word on the right — a big empty gap in
// between and no other data, low information density for an at-a-glance tool.
// The midsection now carries priority/agent/bounce metadata via an explicit grid
// (title / meta cluster / fixed-width status) instead of two flex ends with a void
// between them.
const TaskRow: React.FC<{ task: Task }> = ({ task }) => (
  <div
    style={{
      display: 'grid',
      gridTemplateColumns: 'auto 1fr 90px',
      alignItems: 'center',
      gap: '0.5rem',
      padding: '0.4rem 0.5rem',
      borderRadius: 'var(--wf-radius)',
      background: 'var(--wf-bg)',
    }}
  >
    <span style={{ fontSize: '0.8rem', color: 'var(--wf-fg)' }}>{task.title}</span>
    <div style={{ display: 'flex', justifyContent: 'flex-end', alignItems: 'center', gap: '0.5rem', flexWrap: 'wrap' }}>
      <span
        style={{
          fontSize: '0.65rem',
          color: 'var(--wf-fg-secondary)',
          border: '1px solid var(--wf-border)',
          borderRadius: 'var(--wf-radius)',
          padding: '0.02rem 0.35rem',
        }}
      >
        p{task.priority}
      </span>
      {task.agentId !== undefined && (
        <span style={{ fontSize: '0.65rem', color: 'var(--wf-fg-secondary)' }}>agent {task.agentId}</span>
      )}
      {task.bounces > 0 && (
        <span style={{ fontSize: '0.65rem', color: 'var(--wf-accent-pink)' }}>bounce x{task.bounces}</span>
      )}
    </div>
    <span
      style={{
        fontSize: '0.65rem',
        textAlign: 'right',
        color:
          task.state === 'parked'
            ? 'var(--wf-status-parked)'
            : task.state === 'done'
              ? 'var(--wf-status-done)'
              : 'var(--wf-fg-secondary)',
      }}
    >
      {task.state}
    </span>
  </div>
);

const StoryNode: React.FC<{ story: DrilldownStoryNode }> = ({ story }) => {
  const [open, setOpen] = useState(true);
  return (
    <div style={{ marginLeft: '1rem', marginTop: '0.4rem' }}>
      <div
        onClick={() => setOpen((o) => !o)}
        style={{ display: 'flex', alignItems: 'center', gap: '0.5rem', cursor: 'pointer' }}
      >
        <span style={{ color: 'var(--wf-fg-secondary)', fontSize: '0.7rem' }}>{open ? '▾' : '▸'}</span>
        <span style={{ fontSize: '0.85rem', color: 'var(--wf-fg)', fontWeight: 500 }}>{story.title}</span>
        <RollupBadge done={story.done} total={story.total} />
      </div>
      <AnimatePresence initial={false}>
        {open && (
          <motion.div
            initial={{ height: 0, opacity: 0 }}
            animate={{ height: 'auto', opacity: 1 }}
            exit={{ height: 0, opacity: 0 }}
            transition={{ duration: 0.18 }}
            style={{ overflow: 'hidden', marginLeft: '1.25rem', display: 'flex', flexDirection: 'column', gap: '0.3rem', marginTop: '0.3rem' }}
          >
            {story.tasks.map((t) => (
              <TaskRow key={t.id} task={t} />
            ))}
            {story.tasks.length === 0 && (
              <span style={{ fontSize: '0.7rem', color: 'var(--wf-fg-secondary)' }}>no tasks</span>
            )}
          </motion.div>
        )}
      </AnimatePresence>
    </div>
  );
};

const EpicNode: React.FC<{ epic: DrilldownEpicNode }> = ({ epic }) => {
  const [open, setOpen] = useState(true);
  return (
    <div
      style={{
        background: 'var(--wf-bg-secondary)',
        borderRadius: 'var(--wf-radius)',
        padding: '0.6rem 0.75rem',
      }}
    >
      <div
        onClick={() => setOpen((o) => !o)}
        style={{ display: 'flex', alignItems: 'center', gap: '0.5rem', cursor: 'pointer' }}
      >
        <span style={{ color: 'var(--wf-fg-secondary)', fontSize: '0.75rem' }}>{open ? '▾' : '▸'}</span>
        <span style={{ fontSize: '0.95rem', color: 'var(--wf-fg)', fontWeight: 700 }}>{epic.title}</span>
        <RollupBadge done={epic.done} total={epic.total} />
      </div>
      <AnimatePresence initial={false}>
        {open && (
          <motion.div
            initial={{ height: 0, opacity: 0 }}
            animate={{ height: 'auto', opacity: 1 }}
            exit={{ height: 0, opacity: 0 }}
            transition={{ duration: 0.18 }}
            style={{ overflow: 'hidden' }}
          >
            {epic.stories.map((s) => (
              <StoryNode key={s.id} story={s} />
            ))}
          </motion.div>
        )}
      </AnimatePresence>
    </div>
  );
};

export interface DrilldownProps {
  project: Pick<Project, 'tasks' | 'epics' | 'stories'>;
}

export const Drilldown: React.FC<DrilldownProps> = ({ project }) => {
  const tree = useMemo(() => buildDrilldownTree(project), [project]);

  return (
    <div style={{ display: 'flex', flexDirection: 'column', gap: '0.75rem', maxWidth: 1100 }}>
      <div style={{ display: 'flex', alignItems: 'center', gap: '0.5rem' }}>
        <span style={{ fontSize: '0.85rem', color: 'var(--wf-fg-secondary)' }}>Project rollup</span>
        <RollupBadge done={tree.done} total={tree.total} />
      </div>

      {tree.epics.map((epic) => (
        <EpicNode key={epic.id} epic={epic} />
      ))}

      {tree.ungrouped.length > 0 && (
        <div
          style={{
            background: 'var(--wf-bg-secondary)',
            borderRadius: 'var(--wf-radius)',
            padding: '0.6rem 0.75rem',
          }}
        >
          <div style={{ fontSize: '0.85rem', color: 'var(--wf-fg)', fontWeight: 600, marginBottom: '0.4rem' }}>
            {tree.epics.length > 0 ? 'Ungrouped tasks' : 'Tasks'}
          </div>
          <div style={{ display: 'flex', flexDirection: 'column', gap: '0.3rem' }}>
            {tree.ungrouped.map((t) => (
              <TaskRow key={t.id} task={t} />
            ))}
          </div>
        </div>
      )}

      {tree.total === 0 && (
        <p style={{ color: 'var(--wf-fg-secondary)', fontSize: '0.85rem' }}>
          No tasks yet — authorize the project to start the breakdown.
        </p>
      )}
    </div>
  );
};

export default Drilldown;
