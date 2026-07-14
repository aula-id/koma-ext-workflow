import React, { useState } from 'react';
import Dashboard from './views/Dashboard';

const App: React.FC = () => {
  const [currentView] = useState<'dashboard' | 'board'>('dashboard');

  const handleProjectClick = (projectId: string) => {
    console.log('Project clicked:', projectId);
  };

  return (
    <div className="bg-gray-900 min-h-screen">
      {currentView === 'dashboard' && <Dashboard onProjectClick={handleProjectClick} />}
    </div>
  );
};

export default App;
