import { useEffect } from 'react';
import type { Line, Release } from './releaseNotes';

// **bold** in a line.
function Rich({ text }: { text: string }) {
  return <>{text.split('**').map((part, i) => (i % 2 ? <strong key={i}>{part}</strong> : part))}</>;
}

export function ReleaseLines({ lines }: { lines: Line[] }) {
  return (
    <>
      <ul className="changelog-list">
        {lines.filter(l => l.item).map((l, i) => <li key={i}><Rich text={l.text} /></li>)}
      </ul>
      {lines.filter(l => !l.item).map((l, i) => <p key={i} className="changelog-note"><Rich text={l.text} /></p>)}
    </>
  );
}

// The news of one version (after an update) or of all of them (settings).
export function ChangelogDialog({ title, releases, onClose }: {
  title: string;
  releases: Release[];
  onClose: () => void;
}) {
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => { if (e.key === 'Escape') onClose(); };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, [onClose]);

  return (
    <div className="modal-backdrop" onClick={onClose}>
      <div className="modal" role="dialog" aria-modal="true" onClick={e => e.stopPropagation()}>
        <div className="modal-title">{title}</div>
        <div className="changelog">
          {releases.map(r => (
            <div key={r.version} className="changelog-release">
              {releases.length > 1 && (
                <div className="changelog-version">Version {r.version}{r.date && <span className="setting-sub"> · {r.date}</span>}</div>
              )}
              <ReleaseLines lines={r.lines} />
            </div>
          ))}
        </div>
        <button className="btn" style={{ width: '100%', marginTop: 12 }} autoFocus onClick={onClose}>Fermer</button>
      </div>
    </div>
  );
}
