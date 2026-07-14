import React, { useState, useEffect } from 'react';
import { motion } from 'framer-motion';
import { useStore, Project } from '../store';
import { bridge } from '../bridge';
import { themeManager, Theme } from '../theme';

interface SettingsProps {
  projectId?: string;
  onBack?: () => void;
}

const Settings: React.FC<SettingsProps> = ({ projectId, onBack }) => {
  const { projects } = useStore();
  const [selectedProject, setSelectedProject] = useState<Project | null>(null);
  const [theme, setTheme] = useState<Theme>('dark');
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [success, setSuccess] = useState<string | null>(null);

  // Form state for selected project
  const [formData, setFormData] = useState({
    maxWorkers: 2,
    bounceBudget: 3,
    workerModel: '',
    reviewerModel: '',
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
      workerModel: project.config?.workerModel || '',
      reviewerModel: project.config?.reviewerModel || '',
      keepDesks: project.config?.keepDesks || false,
    });
  };

  const handleProjectSelect = (project: Project) => {
    setSelectedProject(project);
    initializeFormData(project);
    setError(null);
    setSuccess(null);
  };

  const clampMaxWorkers = (value: number): number => {
    return Math.max(1, Math.min(4, value));
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

  const handleModelChange = (field: 'workerModel' | 'reviewerModel', value: string) => {
    setFormData({ ...formData, [field]: value });
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
        workerModel: formData.workerModel || undefined,
        reviewerModel: formData.reviewerModel || undefined,
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
        setSuccess('Settings saved successfully');
        setTimeout(() => setSuccess(null), 3000);
      }
    } catch (err: any) {
      setError(err.message || 'Failed to save settings');
    } finally {
      setLoading(false);
    }
  };

  if (projects.length === 0) {
    return (
      <div className="min-h-screen p-6" style={{ backgroundColor: 'var(--wf-bg)' }}>
        <div className="max-w-2xl mx-auto">
          <button
            onClick={onBack}
            className="mb-4 px-4 py-2 rounded"
            style={{ backgroundColor: 'var(--wf-bg-secondary)', color: 'var(--wf-fg)' }}
          >
            Back
          </button>
          <h1 className="text-2xl font-bold mb-4" style={{ color: 'var(--wf-fg)' }}>
            Settings
          </h1>
          <p style={{ color: 'var(--wf-fg-secondary)' }}>No projects to configure yet.</p>
        </div>
      </div>
    );
  }

  return (
    <div className="min-h-screen p-6" style={{ backgroundColor: 'var(--wf-bg)' }}>
      <div className="max-w-2xl mx-auto">
        {onBack && (
          <button
            onClick={onBack}
            className="mb-4 px-4 py-2 rounded transition-colors"
            style={{
              backgroundColor: 'var(--wf-bg-secondary)',
              color: 'var(--wf-fg)',
            }}
          >
            Back
          </button>
        )}

        <h1 className="text-3xl font-bold mb-6" style={{ color: 'var(--wf-fg)' }}>
          Settings
        </h1>

        <motion.div
          initial={{ opacity: 0, y: 10 }}
          animate={{ opacity: 1, y: 0 }}
          className="space-y-6"
        >
          {/* Theme Settings */}
          <section
            className="p-4 rounded-lg border"
            style={{
              backgroundColor: 'var(--wf-bg-secondary)',
              borderColor: 'var(--wf-accent-blue)',
            }}
          >
            <h2 className="text-lg font-semibold mb-4" style={{ color: 'var(--wf-fg)' }}>
              Appearance
            </h2>
            <div className="flex gap-3">
              {(['dark', 'light', 'milk'] as const).map((t) => (
                <button
                  key={t}
                  onClick={() => handleThemeChange(t)}
                  className={`px-4 py-2 rounded capitalize transition-all ${
                    theme === t ? 'font-semibold' : 'opacity-70'
                  }`}
                  style={{
                    backgroundColor:
                      theme === t ? 'var(--wf-accent-blue)' : 'var(--wf-bg)',
                    color: 'var(--wf-fg)',
                  }}
                >
                  {t}
                </button>
              ))}
            </div>
          </section>

          {/* Project Selection */}
          {projects.length > 1 && (
            <section
              className="p-4 rounded-lg border"
              style={{
                backgroundColor: 'var(--wf-bg-secondary)',
                borderColor: 'var(--wf-accent-blue)',
              }}
            >
              <h2 className="text-lg font-semibold mb-4" style={{ color: 'var(--wf-fg)' }}>
                Select Project
              </h2>
              <div className="grid grid-cols-2 gap-2">
                {projects.map((project) => (
                  <button
                    key={project.id}
                    onClick={() => handleProjectSelect(project)}
                    className={`p-3 rounded text-left transition-all ${
                      selectedProject?.id === project.id
                        ? 'ring-2'
                        : 'opacity-70 hover:opacity-100'
                    }`}
                    style={{
                      backgroundColor:
                        selectedProject?.id === project.id
                          ? 'var(--wf-accent-purple)'
                          : 'var(--wf-bg)',
                      color: 'var(--wf-fg)',
                      boxShadow:
                        selectedProject?.id === project.id
                          ? `0 0 0 2px var(--wf-accent-blue)`
                          : 'none',
                    }}
                  >
                    <div className="font-semibold text-sm">{project.name}</div>
                    <div className="text-xs opacity-70">{project.phase}</div>
                  </button>
                ))}
              </div>
            </section>
          )}

          {selectedProject && (
            <>
              {/* State Root Display */}
              <section
                className="p-4 rounded-lg border"
                style={{
                  backgroundColor: 'var(--wf-bg-secondary)',
                  borderColor: 'var(--wf-accent-green)',
                }}
              >
                <h2 className="text-lg font-semibold mb-2" style={{ color: 'var(--wf-fg)' }}>
                  State Root
                </h2>
                <div
                  className="p-2 rounded text-sm font-mono"
                  style={{ backgroundColor: 'var(--wf-bg)', color: 'var(--wf-accent-green)' }}
                >
                  ~/.koma-workflow/
                </div>
                <p className="text-xs mt-2" style={{ color: 'var(--wf-fg-secondary)' }}>
                  All project state is stored here. Do not edit manually.
                </p>
              </section>

              {/* Errors and Success Messages */}
              {error && (
                <motion.div
                  initial={{ opacity: 0, y: -10 }}
                  animate={{ opacity: 1, y: 0 }}
                  className="p-3 rounded border"
                  style={{
                    backgroundColor: 'rgba(217, 70, 70, 0.1)',
                    borderColor: 'var(--wf-accent-pink)',
                    color: 'var(--wf-accent-pink)',
                  }}
                >
                  {error}
                </motion.div>
              )}

              {success && (
                <motion.div
                  initial={{ opacity: 0, y: -10 }}
                  animate={{ opacity: 1, y: 0 }}
                  className="p-3 rounded border"
                  style={{
                    backgroundColor: 'rgba(166, 226, 46, 0.1)',
                    borderColor: 'var(--wf-accent-green)',
                    color: 'var(--wf-accent-green)',
                  }}
                >
                  {success}
                </motion.div>
              )}

              {/* Project Configuration Form */}
              <form onSubmit={handleSubmit}>
                <section
                  className="p-4 rounded-lg border space-y-4"
                  style={{
                    backgroundColor: 'var(--wf-bg-secondary)',
                    borderColor: 'var(--wf-accent-orange)',
                  }}
                >
                  <h2 className="text-lg font-semibold" style={{ color: 'var(--wf-fg)' }}>
                    Project Configuration
                  </h2>

                  {/* Max Workers */}
                  <div>
                    <label className="block text-sm font-semibold mb-2" style={{ color: 'var(--wf-fg)' }}>
                      Max Concurrent Workers
                    </label>
                    <div className="flex items-center gap-2">
                      <input
                        type="number"
                        min="1"
                        max="4"
                        value={formData.maxWorkers}
                        onChange={handleMaxWorkersChange}
                        className="w-20 px-3 py-2 rounded"
                        style={{
                          backgroundColor: 'var(--wf-bg)',
                          color: 'var(--wf-fg)',
                          borderColor: 'var(--wf-accent-blue)',
                        }}
                      />
                      <span className="text-sm" style={{ color: 'var(--wf-fg-secondary)' }}>
                        1-4 workers per project
                      </span>
                    </div>
                  </div>

                  {/* Bounce Budget */}
                  <div>
                    <label className="block text-sm font-semibold mb-2" style={{ color: 'var(--wf-fg)' }}>
                      Bounce Budget
                    </label>
                    <div className="flex items-center gap-2">
                      <input
                        type="number"
                        min="0"
                        value={formData.bounceBudget}
                        onChange={handleBounceBudgetChange}
                        className="w-20 px-3 py-2 rounded"
                        style={{
                          backgroundColor: 'var(--wf-bg)',
                          color: 'var(--wf-fg)',
                          borderColor: 'var(--wf-accent-blue)',
                        }}
                      />
                      <span className="text-sm" style={{ color: 'var(--wf-fg-secondary)' }}>
                        Failed review attempts before escalation
                      </span>
                    </div>
                  </div>

                  {/* Worker Model */}
                  <div>
                    <label className="block text-sm font-semibold mb-2" style={{ color: 'var(--wf-fg)' }}>
                      Worker Model
                    </label>
                    <input
                      type="text"
                      placeholder="Leave blank to inherit Main"
                      value={formData.workerModel}
                      onChange={(e) => handleModelChange('workerModel', e.target.value)}
                      className="w-full px-3 py-2 rounded"
                      style={{
                        backgroundColor: 'var(--wf-bg)',
                        color: 'var(--wf-fg)',
                        borderColor: 'var(--wf-accent-blue)',
                      }}
                    />
                    <p className="text-xs mt-1" style={{ color: 'var(--wf-fg-secondary)' }}>
                      Model slug (e.g., claude-opus, gpt-4)
                    </p>
                  </div>

                  {/* Reviewer Model */}
                  <div>
                    <label className="block text-sm font-semibold mb-2" style={{ color: 'var(--wf-fg)' }}>
                      Reviewer Model
                    </label>
                    <input
                      type="text"
                      placeholder="Leave blank to inherit Main"
                      value={formData.reviewerModel}
                      onChange={(e) => handleModelChange('reviewerModel', e.target.value)}
                      className="w-full px-3 py-2 rounded"
                      style={{
                        backgroundColor: 'var(--wf-bg)',
                        color: 'var(--wf-fg)',
                        borderColor: 'var(--wf-accent-blue)',
                      }}
                    />
                    <p className="text-xs mt-1" style={{ color: 'var(--wf-fg-secondary)' }}>
                      Model slug for review tasks
                    </p>
                  </div>

                  {/* Keep Desks Toggle */}
                  <div className="flex items-center gap-3">
                    <button
                      type="button"
                      onClick={handleKeepDesksToggle}
                      className="relative w-12 h-6 rounded-full transition-colors"
                      style={{
                        backgroundColor: formData.keepDesks
                          ? 'var(--wf-accent-green)'
                          : 'var(--wf-bg)',
                      }}
                    >
                      <motion.div
                        layout
                        className="absolute w-5 h-5 bg-white rounded-full top-0.5"
                        animate={{
                          left: formData.keepDesks ? '1.5rem' : '0.25rem',
                        }}
                      />
                    </button>
                    <label className="flex-1 cursor-pointer">
                      <div className="text-sm font-semibold" style={{ color: 'var(--wf-fg)' }}>
                        Keep Desks After Completion
                      </div>
                      <p className="text-xs" style={{ color: 'var(--wf-fg-secondary)' }}>
                        Retain task working directories for debugging
                      </p>
                    </label>
                  </div>

                  {/* Submit Button */}
                  <div className="flex gap-2 pt-4">
                    <button
                      type="submit"
                      disabled={loading}
                      className="px-6 py-2 rounded font-semibold transition-opacity"
                      style={{
                        backgroundColor: 'var(--wf-accent-green)',
                        color: '#000',
                        opacity: loading ? 0.5 : 1,
                      }}
                    >
                      {loading ? 'Saving...' : 'Save Settings'}
                    </button>
                  </div>
                </section>
              </form>
            </>
          )}
        </motion.div>
      </div>
    </div>
  );
};

export default Settings;
