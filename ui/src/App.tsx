import React, { useEffect, useMemo, useRef, useState } from 'react';
import Dashboard from './views/Dashboard';
import Board from './views/Board';
import Settings from './views/Settings';
import { bridge } from './bridge';
import { useStore } from './store';

type ViewType = 'dashboard' | 'board' | 'settings';
type BoardTab = 'board' | 'drilldown' | 'depmap' | 'prd';

interface DeepLink {
  view: ViewType;
  boardTab?: BoardTab;
  /** `?view=task`: also pre-select a task's detail panel once a project resolves. */
  wantsTask?: boolean;
}

/** `?view=dashboard|board|drilldown|task|settings|office` (deterministic initial view
 * for screenshots/deep links). `drilldown`/`task`/`office` are all board sub-views —
 * `office` lands on the PRD tab, where the office chat panel lives (Prd.tsx only shows
 * it once the project is Drafting). Anything unrecognized falls back to the dashboard. */
function parseDeepLink(raw: string | null): DeepLink {
  switch (raw) {
    case 'board':
      return { view: 'board', boardTab: 'board' };
    case 'drilldown':
      return { view: 'board', boardTab: 'drilldown' };
    case 'task':
      return { view: 'board', boardTab: 'board', wantsTask: true };
    case 'office':
      return { view: 'board', boardTab: 'prd' };
    case 'settings':
      return { view: 'settings' };
    default:
      return { view: 'dashboard' };
  }
}

/** Pick a representative project for a project-scoped deep link when the URL didn't
 * name one explicitly via `?project=<id>`: a Drafting project for the office/PRD view
 * (the only phase with a live chat transcript), a Running project otherwise (richest
 * board), falling back to whichever project loaded first. Generic over any snapshot
 * shape — not mock-specific — so this also does the right thing against a real daemon
 * with exactly one project. */
function pickDeepLinkProject(projects: any[], boardTab: BoardTab | undefined, explicitId: string | null): any {
  if (explicitId) {
    const found = projects.find((p) => p.id === explicitId);
    if (found) return found;
  }
  if (projects.length === 0) return undefined;
  const wantPhase = boardTab === 'prd' ? 'drafting' : 'running';
  return projects.find((p) => p.phase?.kind === wantPhase) ?? projects[0];
}

/** Pick a task worth showing for `?view=task`: prefer one with actual detail-panel
 * content (comments/report/review) over an empty backlog stub. */
function pickRichTask(tasks: any[]): any {
  return (
    tasks.find((t) => (t.comments && t.comments.length > 0) || t.lastReport || t.lastReview) ?? tasks[0]
  );
}

const App: React.FC = () => {
  const params = useMemo(() => new URLSearchParams(window.location.search), []);
  const deepLink = useMemo(() => parseDeepLink(params.get('view')), [params]);
  const explicitProjectId = params.get('project');

  const [currentView, setCurrentView] = useState<ViewType>(deepLink.view);
  const [selectedProject, setSelectedProject] = useState<string | null>(null);
  const [boardTab, setBoardTab] = useState<BoardTab | undefined>(deepLink.boardTab);
  const [initialTaskId, setInitialTaskId] = useState<string | undefined>(undefined);

  const projects = useStore((s) => s.projects);
  // Views that don't need a project (dashboard/settings) are "resolved" immediately;
  // board/drilldown/task/office wait for the first snapshot before picking one.
  const deepLinkResolved = useRef(deepLink.view !== 'board');

  // Normally Dashboard.tsx kicks off the initial `hello` + snapshot subscription, but a
  // deep link can land directly on Board/Settings without Dashboard ever mounting — so
  // App owns the load itself. `bridge.hello` is idempotent/safe to call more than once
  // (it's also how the panel rehydrates on visibilitychange).
  useEffect(() => {
    bridge.hello('0.1.0').catch((err) => {
      console.error('Failed to initialize:', err);
    });
    // Call the store action directly rather than `useStore.setState((state) => {...;
    // return state})` — see Dashboard.tsx's fix note for why that wrapper silently
    // reverts every push.
    const unsubscribe = bridge.onSnapshot((snap) => {
      useStore.getState().updateSnapshot(snap);
    });
    return unsubscribe;
  }, []);

  useEffect(() => {
    if (deepLinkResolved.current) return;
    if (projects.length === 0) return;
    const project = pickDeepLinkProject(projects, deepLink.boardTab, explicitProjectId);
    if (!project) return;
    setSelectedProject(project.id);
    if (deepLink.wantsTask) {
      setInitialTaskId(pickRichTask(project.tasks ?? [])?.id);
    }
    deepLinkResolved.current = true;
  }, [projects, deepLink, explicitProjectId]);

  const handleProjectClick = (projectId: string) => {
    setSelectedProject(projectId);
    setBoardTab('board');
    setCurrentView('board');
  };

  const handleBack = () => {
    setCurrentView('dashboard');
    setSelectedProject(null);
  };

  const handleSettingsClick = () => {
    setCurrentView('settings');
  };

  return (
    <div style={{ minHeight: '100vh', backgroundColor: 'var(--wf-bg)' }}>
      {currentView === 'board' && selectedProject ? (
        <Board
          projectId={selectedProject}
          onBack={handleBack}
          onSettings={() => handleSettingsClick()}
          initialTab={boardTab}
          initialTaskId={initialTaskId}
        />
      ) : currentView === 'board' ? (
        // `currentView` flips to 'board' the instant a `?view=board|drilldown|task|office`
        // deep link is parsed, but `selectedProject` only resolves once the first
        // snapshot arrives (see the deepLinkResolved effect above) — a real async gap,
        // not just a screenshot-timing artifact. Without this branch the ternary fell
        // through to the `else` and rendered Dashboard for that whole window, which is
        // exactly the "every board/drilldown/task/office route shows the dashboard"
        // routing bug design-critique round 1 caught. Render a neutral loading state
        // instead of ever silently substituting a different view.
        <div
          style={{
            minHeight: '100vh',
            display: 'flex',
            alignItems: 'center',
            justifyContent: 'center',
            color: 'var(--wf-fg-secondary)',
            fontSize: '0.875rem',
          }}
        >
          Loading project…
        </div>
      ) : currentView === 'settings' ? (
        <Settings
          projectId={selectedProject || undefined}
          onBack={handleBack}
        />
      ) : (
        <Dashboard
          onProjectClick={handleProjectClick}
          onSettings={handleSettingsClick}
        />
      )}
    </div>
  );
};

export default App;
