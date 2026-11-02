import { useEffect, useState } from 'react';
import { api } from './api';
import type { LocalSendStatus } from './api';

// LocalSend is always on: how this device shows on the network.
export function LocalSendStatusRow({ nickname }: {
  // The name devices see; reloaded when it changes.
  nickname?: string;
}) {
  const [status, setStatus] = useState<LocalSendStatus | null>(null);

  useEffect(() => {
    api.getLocalSendStatus().then(setStatus);
  }, [nickname]);

  return (
    <div>
      <div className="setting-label">LocalSend</div>
      <div className="setting-sub">
        {!status
          ? 'Démarrage…'
          : status.running
            ? <>
                Visible sous le nom « {status.status.alias} »{status.status.port !== 53317 ? ` (port ${status.status.port})` : ''} par l’app LocalSend et vos amis sur le même réseau.
                {status.status.error && <div style={{ color: 'var(--danger)', marginTop: 4 }}>{status.status.error}</div>}
              </>
            : <span style={{ color: 'var(--danger)' }}>{status.error ?? 'Indisponible'}</span>}
      </div>
    </div>
  );
}
