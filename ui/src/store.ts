import { create } from 'zustand';
import { Snapshot } from './bridge';

export interface ProjectConfig {
  maxWorkers?: number;
  bounceBudget?: number;
  workerModel?: string;
  reviewerModel?: string;
  keepDesks?: boolean;
}

/** Project lifecycle phase, per docs/PANEL_PROTOCOL.md 2.1 and office-core
 * digest.rs `phase_value`: serialized as an object `{ kind, reason?, atMs? }`
 * with a lowercase `kind`, never a bare string. */
export interface ProjectPhase {
  kind: 'drafting' | 'ready' | 'running' | 'interrupted' | 'halted' | 'done';
  reason?: string;
  atMs?: number;
}

export interface Project {
  id: string;
  name: string;
  phase: ProjectPhase;
  taskCount?: number;
  doneCount?: number;
  runningCount?: number;
  parkedCount?: number;
  lastNotice?: string;
  truncated?: boolean;
  config?: ProjectConfig;
  [key: string]: any;
}

/** Coerce a snapshot's `phase` into the frozen object shape. The daemon always
 * sends `{ kind, reason?, atMs? }`; this also defends against a missing/legacy
 * bare-string phase so consumers can safely read `phase.kind`. */
function normalizePhase(raw: any): ProjectPhase {
  if (raw && typeof raw === 'object' && typeof raw.kind === 'string') {
    return raw as ProjectPhase;
  }
  if (typeof raw === 'string' && raw) {
    return { kind: raw.toLowerCase() as ProjectPhase['kind'] };
  }
  return { kind: 'drafting' };
}

interface StoreState {
  snapshot: Snapshot | null;
  projects: Project[];
  updateSnapshot: (snapshot: Snapshot) => void;
  getProject: (id: string) => Project | undefined;
}

export const useStore = create<StoreState>((set, get) => ({
  snapshot: null,
  projects: [],
  updateSnapshot: (snapshot: Snapshot) => {
    const projects = snapshot.projects || [];
    set({
      snapshot,
      projects: projects.map((p) => ({
        ...p,
        id: p.id || p.projectId || `project-${Math.random()}`,
        name: p.name || 'Untitled Project',
        phase: normalizePhase(p.phase),
        taskCount: p.tasks ? p.tasks.length : 0,
        doneCount: p.tasks ? p.tasks.filter((t: any) => t.state === 'done').length : 0,
        runningCount: p.tasks ? p.tasks.filter((t: any) => t.state === 'onprogress').length : 0,
        parkedCount: p.tasks ? p.tasks.filter((t: any) => t.state === 'parked').length : 0,
        lastNotice: p.outbox && p.outbox.length > 0 ? p.outbox[0].text : undefined,
      })),
    });
  },
  getProject: (id: string) => {
    return get().projects.find((p) => p.id === id);
  },
}));
