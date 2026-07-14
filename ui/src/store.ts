import { create } from 'zustand';
import { Snapshot } from './bridge';

export interface ProjectConfig {
  maxWorkers?: number;
  bounceBudget?: number;
  workerModel?: string;
  reviewerModel?: string;
  keepDesks?: boolean;
}

export interface Project {
  id: string;
  name: string;
  phase: 'Drafting' | 'Ready' | 'Running' | 'Interrupted' | 'Halted' | 'Done';
  taskCount?: number;
  doneCount?: number;
  runningCount?: number;
  parkedCount?: number;
  lastNotice?: string;
  truncated?: boolean;
  config?: ProjectConfig;
  [key: string]: any;
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
        phase: p.phase || 'Drafting',
        taskCount: p.tasks ? p.tasks.length : 0,
        doneCount: p.tasks ? p.tasks.filter((t: any) => t.state?.Done).length : 0,
        runningCount: p.tasks ? p.tasks.filter((t: any) => t.state?.OnProgress).length : 0,
        parkedCount: p.tasks ? p.tasks.filter((t: any) => t.state?.Parked).length : 0,
        lastNotice: p.outbox && p.outbox.length > 0 ? p.outbox[0].text : undefined,
      })),
    });
  },
  getProject: (id: string) => {
    return get().projects.find((p) => p.id === id);
  },
}));
