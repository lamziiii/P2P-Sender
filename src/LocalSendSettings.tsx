import { useEffect, useState } from 'react';
import { api } from './api';
import type { LocalSendStatus } from './api';
import { tr } from './i18n';

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
    text = tr(
      'Désactivé : vous n’êtes pas visible sur le réseau local et LocalSend est arrêté. Vos amis restent joignables par Internet.',
      'Off: you are not visible on the local network and LocalSend is stopped. Your friends stay reachable over the internet.',
    );
  } else if (!status) {
    text = tr('Démarrage…', 'Starting…');
  } else if (status.running) {
    text = (
      <>
        {tr(
          `Visible sous le nom « ${status.status.alias} »${status.status.port !== 53317 ? ` (port ${status.status.port})` : ''} par l’app LocalSend et vos amis sur le même réseau, qui vous trouvent même sans Internet. Vos fichiers entre amis y passent plus vite.`,
          `Visible as “${status.status.alias}”${status.status.port !== 53317 ? ` (port ${status.status.port})` : ''} to the LocalSend app and your friends on the same network, who find you even without internet. Your files between friends go faster there.`,
        )}
        {status.status.error && <div style={{ color: 'var(--danger)', marginTop: 4 }}>{status.status.error}</div>}
      </>
    );
  } else {
    text = <span style={{ color: 'var(--danger)' }}>{status.error ?? tr('Indisponible', 'Unavailable')}</span>;
  }

  return (
    <div className="setting-row">
      <div>
        <div className="setting-label">{tr('Réseau local', 'Local network')}</div>
        <div className="setting-sub">{text}</div>
        <div className="setting-sub" style={{ marginTop: 4 }}>{tr('L’app redémarre pour appliquer le changement.', 'The app restarts to apply the change.')}</div>
        {error && <div className="error-text">{error}</div>}
      </div>
      <button className="btn btn-secondary btn-small" disabled={applying} onClick={toggle}>
        {enabled ? tr('Désactiver', 'Turn off') : tr('Activer', 'Turn on')}
      </button>
    </div>
  );
}
