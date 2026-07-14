import React, { useEffect, useState } from 'react';

/**
 * Two-step destructive button. wry webviews have NO native window.confirm /
 * alert / prompt — they silently return falsy, so any action guarded by them
 * simply never fires. All confirmations are therefore in-UI: the first click
 * arms the button ("confirm <label>?"), a second click within 3.5s fires, and
 * it disarms automatically after the timeout.
 */
export const ConfirmButton: React.FC<{
  label: string;
  className: string;
  onConfirm: () => void;
  disabled?: boolean;
  testId?: string;
}> = ({ label, className, onConfirm, disabled, testId }) => {
  const [armed, setArmed] = useState(false);

  useEffect(() => {
    if (!armed) return;
    const t = setTimeout(() => setArmed(false), 3500);
    return () => clearTimeout(t);
  }, [armed]);

  return (
    <button
      data-testid={testId}
      className={className}
      disabled={disabled}
      style={armed ? { fontWeight: 700 } : undefined}
      onClick={() => {
        if (armed) {
          setArmed(false);
          onConfirm();
        } else {
          setArmed(true);
        }
      }}
    >
      {armed ? `confirm ${label}?` : label}
    </button>
  );
};

export default ConfirmButton;
