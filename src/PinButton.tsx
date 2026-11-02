import { useEffect, useState } from 'react';
import { api } from './api';
import { Icon } from './icons';
import { isMobile } from './platform';

// Keeps the window open when another window takes the focus (it goes away
// then, like the OneDrive flyout): to drag files from the file explorer.
export function PinButton() {
  const [pinned, setPinned] = useState(false);

  useEffect(() => {
    let live = true;
    api.getWindowPinned().then(p => { if (live) setPinned(p); });
    return () => { live = false; };
  }, []);

  const toggle = async () => setPinned(await api.setWindowPinned(!pinned));

  return (
    <button
      className={`icon-btn ${pinned ? 'active' : ''}`}
      title={pinned ? 'Ne plus garder ouvert' : 'Garder ouvert (pour glisser des fichiers depuis l’Explorateur)'}
      aria-pressed={pinned}
      onClick={toggle}
    >
      <Icon name="pin" />
    </button>
  );
}

// Hides the flyout (back to the tray).
export function HideButton() {
  return <button className="icon-btn" title="Masquer" onClick={() => api.hideWindow()}><Icon name="close" /></button>;
}

// The flyout's own buttons, at the end of each header: none on a phone,
// where the app is full screen.
export function WindowButtons() {
  if (isMobile) return null;
  return (
    <>
      <PinButton />
      <HideButton />
    </>
  );
}
