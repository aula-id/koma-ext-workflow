import React, { useState } from 'react';
import Dashboard from './views/Dashboard';
import Board from './views/Board';

const App: React.FC = () => {
  const [selectedProject, setSelectedProject] = useState<string | null>(null);

  const handleProjectClick = (projectId: string) => {
    setSelectedProject(projectId);
  };

  const handleBack = () => {
    setSelectedProject(null);
  };

  return (
    <div style={{ minHeight: '100vh', backgroundColor: 'var(--wf-bg)' }}>
      {selectedProject ? (
        <Board projectId={selectedProject} onBack={handleBack} />
      ) : (
        <Dashboard onProjectClick={handleProjectClick} />
      )}
    </div>
  );
};

export default App;
