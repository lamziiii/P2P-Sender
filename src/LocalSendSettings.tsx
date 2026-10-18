import { useEffect, useState } from 'react';
import { api } from './api';
import type { LocalSendStatus } from './api';

// Receive files from the LocalSend app (phones…) on the same network.
export function LocalSendSettings({ enabled, nickname, onChange }: {
  enabled: boolean;
  // The name devices see; reloaded when it changes.
  nickname?: string;
  onChange: (enabled: boolean) => void;
}) {
  const [status, setStatus] = useState<LocalSendStatus | null>(null);
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    api.getLocalSendStatus().then(setStatus);
  }, [nickname]);

  const toggle = async (enable: boolean) => {
    setBusy(true);
    onChange(enable);
    setStatus(await api.setLocalSend(enable));
    setBusy(false);
  };

  return (
    <div className="card">
      <div className="setting-row">
        <div>
          <div className="setting-label">Compatibilité LocalSend</div>
          <div className="setting-sub">Échanger des fichiers avec l’app LocalSend (même Wi-Fi)</div>
        </div>
        <label className="toggle">
          <input type="checkbox" checked={enabled} disabled={busy} onChange={e => toggle(e.target.checked)} />
          <span className="toggle-track" />
        </label>
      </div>
      {enabled && status && (
        <div className="setting-sub" style={{ marginTop: 6 }}>
          {status.running
            ? <>
                <span style={{ color: 'var(--success)' }}>
                  Visible sous le nom « {status.status.alias} »{status.status.port !== 53317 ? ` (port ${status.status.port})` : ''}
                </span>
                {status.status.error && <div style={{ color: 'var(--danger)', marginTop: 4 }}>{status.status.error}</div>}
              </>
            : <span style={{ color: 'var(--danger)' }}>{status.error ?? 'Démarrage…'}</span>}
        </div>
      )}
      {enabled && (
        <div className="setting-sub" style={{ marginTop: 6 }}>
          Le nom affiché est votre pseudo. Chaque fichier reçu vous est proposé avant d’être enregistré. Les appareils trouvés apparaissent dans l’onglet Amis.
        </div>
      )}
    </div>
  );
}
