import React, { useState, useEffect } from 'react';
import { useStore, Project } from '../store';
import { bridge } from '../bridge';
import { themeManager, Theme } from '../theme';
import { ConfirmButton } from '../components/ConfirmButton';

interface SettingsProps {
  projectId?: string;
  onBack?: () => void;
}

/** Clamp `maxWorkers` to the kernel's project ceiling (office-core kernel.rs
 * `MAX_PROJECT_WORKERS`, PANEL_PROTOCOL.md 1.2 `config_set`): 1..=4. Exported (not just
 * an inline closure) so it is the actual function under test, not a copy-pasted
 * reimplementation — see Settings.test.ts. */
export function clampMaxWorkers(value: number): number {
  return Math.max(1, Math.min(4, value));
}

const PHASE_COLOR: Record<string, string> = {
  drafting: 'var(--wf-status-drafting)',
  ready: 'var(--wf-accent)',
  running: 'var(--wf-status-running)',
  interrupted: 'var(--wf-status-parked)',
  halted: 'var(--wf-status-blocked)',
  done: 'var(--wf-status-done)',
};

/*
 * koma-flat settings: one centered column, sections separated by hairline
 * rules and small-caps titles — no boxed cards, no filled chips, no neon
 * buttons. Model bindings are deliberately ABSENT: worker/reviewer models are
 * bound on the extension's contributed sub-agents in koma's sub-agent
 * sidebar (single source of truth), not per-project free-text slugs.
 */
const Settings: React.FC<SettingsProps> = ({ projectId, onBack }) => {
  const { projects } = useStore();
  const [selectedProject, setSelectedProject] = useState<Project | null>(null);
  const [theme, setTheme] = useState<Theme>('dark');
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [success, setSuccess] = useState<string | null>(null);

  const [formData, setFormData] = useState({
    maxWorkers: 2,
    bounceBudget: 3,
    keepDesks: false,
  });

  useEffect(() => {
    const currentTheme = themeManager.getTheme();
    setTheme(currentTheme);
    const unsubscribe = themeManager.subscribe(setTheme);
    return unsubscribe;
  }, []);

  useEffect(() => {
    if (projectId) {
      const project = projects.find((p) => p.id === projectId);
      if (project) {
        setSelectedProject(project);
        initializeFormData(project);
      }
    } else if (projects.length > 0) {
      setSelectedProject(projects[0]);
      initializeFormData(projects[0]);
    }
  }, [projectId, projects]);

  const initializeFormData = (project: Project) => {
    setFormData({
      maxWorkers: project.config?.maxWorkers || 2,
      bounceBudget: project.config?.bounceBudget || 3,
      keepDesks: project.config?.keepDesks || false,
    });
  };

  const handleProjectSelect = (project: Project) => {
    setSelectedProject(project);
    initializeFormData(project);
    setError(null);
    setSuccess(null);
  };

  const handleMaxWorkersChange = (e: React.ChangeEvent<HTMLInputElement>) => {
    const value = parseInt(e.target.value, 10);
    if (!isNaN(value)) {
      setFormData({ ...formData, maxWorkers: clampMaxWorkers(value) });
    }
  };

  const handleBounceBudgetChange = (e: React.ChangeEvent<HTMLInputElement>) => {
    const value = parseInt(e.target.value, 10);
    if (!isNaN(value) && value >= 0) {
      setFormData({ ...formData, bounceBudget: value });
    }
  };

  const handleKeepDesksToggle = () => {
    setFormData({ ...formData, keepDesks: !formData.keepDesks });
  };

  const handleThemeChange = (newTheme: Theme) => {
    themeManager.setTheme(newTheme);
    setTheme(newTheme);
  };

  const handleSubmit = async (e: React.FormEvent) => {
    e.preventDefault();
    if (!selectedProject) return;

    setLoading(true);
    setError(null);
    setSuccess(null);

    try {
      const clampedMaxWorkers = clampMaxWorkers(formData.maxWorkers);
      const payload = {
        op: 'config_set',
        project: selectedProject.id,
        maxWorkers: clampedMaxWorkers,
        bounceBudget: formData.bounceBudget,
        keepDesks: formData.keepDesks,
      };

      const result = await bridge.send(payload);

      if (result.error) {
        if (result.error.includes('grant denied')) {
          setError('Access denied: insufficient permissions');
        } else if (result.error.includes('read-only')) {
          setError('Project is read-only: owned by another session');
        } else {
          setError(result.error);
        }
      } else {
        setSuccess('Settings saved');
        setTimeout(() => setSuccess(null), 3000);
      }
    } catch (err: any) {
      setError(err.message || 'Failed to save settings');
    } finally {
      setLoading(false);
    }
  };

  const handleDeleteProject = async () => {
    if (!selectedProject) return;

    setError(null);
    setSuccess(null);

    try {
      const result = await bridge.send({ op: 'project_archive', project: selectedProject.id });

      if (result.error) {
        setError(result.error);
      } else {
        setSelectedProject(null);
        setSuccess('Project deleted');
        setTimeout(() => setSuccess(null), 3000);
      }
    } catch (err: any) {
      setError(err.message || 'Failed to delete project');
    }
  };

  const header = (
    <div
      style={{
        display: 'flex',
        alignItems: 'baseline',
        gap: '0.75rem',
        paddingBottom: '0.6rem',
        borderBottom: '1px solid var(--wf-head)',
      }}
    >
      {onBack && (
        <button onClick={onBack} className="wf-btn wf-btn-ghost" style={{ paddingLeft: 0 }}>
          &larr;
        </button>
      )}
      <h1 style={{ color: 'var(--wf-fg)', fontSize: '1rem', fontWeight: 700, margin: 0 }}>Settings</h1>
    </div>
  );

  if (projects.length === 0) {
    return (
      <div style={{ minHeight: '100vh', padding: '1.25rem 1.5rem' }}>
        <div style={{ maxWidth: 640, margin: '0 auto' }}>
          {header}
          <p style={{ color: 'var(--wf-dim)', marginTop: '1rem' }}>No projects to configure yet.</p>
        </div>
      </div>
    );
  }

  return (
    <div style={{ minHeight: '100vh', padding: '1.25rem 1.5rem' }}>
      <div style={{ maxWidth: 640, margin: '0 auto' }}>
        {header}

        {/* Appearance */}
        <div className="wf-section" style={{ borderTop: 'none', marginTop: '1rem', paddingTop: 0 }}>
          <h2 className="wf-section-title">Appearance</h2>
          <div style={{ display: 'flex', gap: '0.5rem' }}>
            {(['dark', 'light'] as const).map((t) => (
              <button
                key={t}
                onClick={() => handleThemeChange(t)}
                className="wf-btn"
                style={
                  theme === t
                    ? { borderColor: 'var(--wf-grip)', color: 'var(--wf-fg)', background: 'var(--wf-hover)' }
                    : { color: 'var(--wf-dim)' }
                }
              >
                {t}
              </button>
            ))}
          </div>
        </div>

        {/* Project selector: flat rows, accent left bar for the active one */}
        {projects.length > 1 && (
          <div className="wf-section">
            <h2 className="wf-section-title">Project</h2>
            <div>
              {projects.map((project) => {
                const selected = selectedProject?.id === project.id;
                const phaseColor = PHASE_COLOR[project.phase.kind] || 'var(--wf-dim)';
                return (
                  <button
                    key={project.id}
                    onClick={() => handleProjectSelect(project)}
                    style={{
                      display: 'flex',
                      alignItems: 'center',
                      gap: '0.6rem',
                      width: '100%',
                      textAlign: 'left',
                      padding: '0.45rem 0.6rem',
                      borderLeft: selected ? '2px solid var(--wf-accent)' : '2px solid transparent',
                      background: selected ? 'var(--wf-hover)' : 'transparent',
                      color: selected ? 'var(--wf-fg)' : 'var(--wf-dim)',
                      borderRadius: 0,
                      borderBottom: '1px solid var(--wf-border)',
                    }}
                  >
                    <span className="wf-status-dot" style={{ background: phaseColor }} />
                    <span style={{ flex: 1 }}>{project.name}</span>
                    <span style={{ fontSize: '0.72rem', color: phaseColor }}>{project.phase.kind}</span>
                  </button>
                );
              })}
            </div>
          </div>
        )}

        {selectedProject && (
          <>
            {/* Storage */}
            <div className="wf-section">
              <h2 className="wf-section-title">Storage</h2>
              {/* a path is a contained/code thing — the one allowed box */}
              <div
                style={{
                  padding: '0.4rem 0.6rem',
                  background: 'var(--wf-panel2)',
                  border: '1px solid var(--wf-border)',
                  borderRadius: 'var(--wf-radius)',
                  color: 'var(--wf-fg)',
                  fontSize: '0.8rem',
                }}
              >
                ~/.koma-workflow/
              </div>
              <p style={{ fontSize: '0.72rem', color: 'var(--wf-dim)', margin: '0.4rem 0 0' }}>
                All project state lives here. Do not edit manually.
              </p>
            </div>

            {error && (
              <div
                style={{
                  marginTop: '1rem',
                  padding: '0.45rem 0.6rem',
                  borderLeft: '2px solid var(--wf-error)',
                  background: 'var(--wf-tint-error)',
                  color: 'var(--wf-error)',
                  fontSize: '0.8rem',
                }}
              >
                {error}
              </div>
            )}

            {success && (
              <div
                style={{
                  marginTop: '1rem',
                  padding: '0.45rem 0.6rem',
                  borderLeft: '2px solid var(--wf-success)',
                  background: 'var(--wf-tint-success)',
                  color: 'var(--wf-success)',
                  fontSize: '0.8rem',
                }}
              >
                {success}
              </div>
            )}

            {/* Project configuration */}
            <form onSubmit={handleSubmit}>
              <div className="wf-section">
                <h2 className="wf-section-title">Project configuration</h2>

                <div style={{ display: 'flex', flexDirection: 'column', gap: '0.9rem' }}>
                  <label style={{ display: 'flex', alignItems: 'center', gap: '0.75rem' }}>
                    <input
                      type="number"
                      min="1"
                      max="4"
                      value={formData.maxWorkers}
                      onChange={handleMaxWorkersChange}
                      data-testid="settings-max-workers"
                      style={{ width: 64 }}
                    />
                    <span>
                      <span style={{ color: 'var(--wf-fg)', fontSize: '0.82rem' }}>Max concurrent workers</span>
                      <span style={{ color: 'var(--wf-dim)', fontSize: '0.72rem', display: 'block' }}>
                        1-4 per project; the office always leaves one koma sub-agent slot for you
                      </span>
                    </span>
                  </label>

                  <label style={{ display: 'flex', alignItems: 'center', gap: '0.75rem' }}>
                    <input
                      type="number"
                      min="0"
                      value={formData.bounceBudget}
                      onChange={handleBounceBudgetChange}
                      data-testid="settings-bounce-budget"
                      style={{ width: 64 }}
                    />
                    <span>
                      <span style={{ color: 'var(--wf-fg)', fontSize: '0.82rem' }}>Bounce budget</span>
                      <span style={{ color: 'var(--wf-dim)', fontSize: '0.72rem', display: 'block' }}>
                        failed review attempts before a task is parked
                      </span>
                    </span>
                  </label>

                  <label style={{ display: 'flex', alignItems: 'center', gap: '0.75rem', cursor: 'pointer' }}>
                    <input
                      type="checkbox"
                      role="switch"
                      aria-checked={formData.keepDesks}
                      checked={formData.keepDesks}
                      data-testid="settings-keep-desks-toggle"
                      onChange={handleKeepDesksToggle}
                      style={{ width: 16, height: 16, accentColor: 'var(--wf-accent)' }}
                    />
                    <span>
                      <span style={{ color: 'var(--wf-fg)', fontSize: '0.82rem' }}>Keep desks after completion</span>
                      <span style={{ color: 'var(--wf-dim)', fontSize: '0.72rem', display: 'block' }}>
                        retain task working directories for debugging
                      </span>
                    </span>
                  </label>

                  <p style={{ fontSize: '0.72rem', color: 'var(--wf-dim)', margin: 0 }}>
                    Worker and reviewer models are bound in koma&apos;s sub-agent sidebar (default: inherit
                    Main) — there is deliberately no per-project model override here.
                  </p>

                  <div>
                    <button type="submit" disabled={loading} data-testid="settings-submit" className="wf-btn wf-btn-accent">
                      {loading ? 'saving…' : 'save'}
                    </button>
                  </div>
                </div>
              </div>
            </form>

            {/* Danger zone */}
            <div className="wf-section">
              <h2 className="wf-section-title">Danger zone</h2>
              <p style={{ fontSize: '0.72rem', color: 'var(--wf-dim)', margin: '0 0 0.6rem' }}>
                Deletes the board, PRD, comments, and desks for {selectedProject.name}. Delivered code in the
                delivery path is not touched.
              </p>
              <ConfirmButton
                label="delete project"
                className="wf-btn wf-btn-danger"
                testId="settings-delete-project"
                onConfirm={handleDeleteProject}
              />
            </div>
          </>
        )}
      </div>
    </div>
  );
};

export default Settings;
