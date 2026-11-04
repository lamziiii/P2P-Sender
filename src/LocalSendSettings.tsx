import { useEffect, useState } from 'react';
import { api } from './api';
import type { LocalSendStatus } from './api';

// The app's use of the local network: LocalSend (how this device shows
// there, which also finds friends without the internet) and the fast path
// between friends. Turning it on or off restarts the app.
export function LocalNetworkRow({ nickname, enabled }: {
  // The name devices see; reloaded when it changes.
  nickname?: string;
  enabled: boolean;
}) {
  const [status, setStatus] = useState<LocalSendStatus | null>(null);
  const [applying, setApplying] = useState(false);
  const [error, setError] = useState('');

  useEffect(() => {
    api.getLocalSendStatus().then(setStatus);
  }, [nickname]);

  const toggle = async () => {
    setError('');
    setApplying(true);
    try {
      await api.setLocalNetwork(!enabled);
    } catch (e) {
      setError(String(e));
    }
    setApplying(false);
  };

  let text;
  if (!enabled) {
    text = 'Désactivé : vous n’êtes pas visible sur le réseau local et LocalSend est arrêté. Vos amis restent joignables par Internet.';
  } else if (!status) {
    text = 'Démarrage…';
  } else if (status.running) {
    text = (
      <>
        Visible sous le nom « {status.status.alias} »{status.status.port !== 53317 ? ` (port ${status.status.port})` : ''} par l’app LocalSend et vos amis sur le même réseau, qui vous trouvent même sans Internet. Vos fichiers entre amis y passent plus vite.
        {status.status.error && <div style={{ color: 'var(--danger)', marginTop: 4 }}>{status.status.error}</div>}
      </>
    );
  } else {
    text = <span style={{ color: 'var(--danger)' }}>{status.error ?? 'Indisponible'}</span>;
  }

  return (
    <div className="setting-row">
      <div>
        <div className="setting-label">Réseau local</div>
        <div className="setting-sub">{text}</div>
        <div className="setting-sub" style={{ marginTop: 4 }}>L’app redémarre pour appliquer le changement.</div>
        {error && <div className="error-text">{error}</div>}
      </div>
      <button className="btn btn-secondary btn-small" disabled={applying} onClick={toggle}>
        {enabled ? 'Désactiver' : 'Activer'}
      </button>
    </div>
  );
}
