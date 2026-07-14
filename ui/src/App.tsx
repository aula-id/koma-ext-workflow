import React, { useState } from 'react';
import Dashboard from './views/Dashboard';
import Board from './views/Board';
import Settings from './views/Settings';

type ViewType = 'dashboard' | 'board' | 'settings';

const App: React.FC = () => {
  const [currentView, setCurrentView] = useState<ViewType>('dashboard');
  const [selectedProject, setSelectedProject] = useState<string | null>(null);

  const handleProjectClick = (projectId: string) => {
    setSelectedProject(projectId);
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
        />
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
