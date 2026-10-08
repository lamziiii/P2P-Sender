import { useEffect, useState } from 'react';
import { api } from './api';
import type { Config, RelayStatus } from './api';

// Self-hosted relay, for networks that block iroh's public relays (eduroam…).
export function RelaySettings({ config }: { config: Partial<Config> }) {
  const [url, setUrl] = useState(config.relayUrl ?? '');
  const [token, setToken] = useState(config.relayToken ?? '');
  const [status, setStatus] = useState<RelayStatus | null>(null);
  const [error, setError] = useState('');
  const [applying, setApplying] = useState(false);

  useEffect(() => {
    setUrl(config.relayUrl ?? '');
    setToken(config.relayToken ?? '');
  }, [config.relayUrl, config.relayToken]);

  useEffect(() => {
    const load = () => api.getRelayStatus().then(setStatus);
    load();
    const timer = setInterval(load, 3000);
    return () => clearInterval(timer);
  }, []);

  const norm = (u: string) => u.trim().replace(/\/+$/, '');
  const changed = norm(url) !== norm(config.relayUrl ?? '') || token.trim() !== (config.relayToken ?? '');

  const apply = async (newUrl: string, newToken: string) => {
    setError('');
    setApplying(true);
    try {
      await api.setRelay(newUrl, newToken);
    } catch (e) {
      setError(String(e));
    }
    setApplying(false);
  };

  return (
    <div className="card">
      <div className="card-label">Relais personnel</div>
      <input className="input" placeholder="https://relais.exemple.fr" value={url} onChange={e => setUrl(e.target.value)} spellCheck={false} />
      <input className="input" style={{ marginTop: 6 }} type="password" placeholder="Jeton (si le relais en demande un)" value={token} onChange={e => setToken(e.target.value)} spellCheck={false} />
      {error && <div className="error-text">{error}</div>}
      <div style={{ display: 'flex', gap: 8, marginTop: 8, alignItems: 'center' }}>
        <button className="btn btn-secondary btn-small" disabled={!changed || applying} onClick={() => apply(url, token)}>
          Appliquer et redémarrer
        </button>
        {config.relayUrl && (
          <button className="btn btn-secondary btn-small" disabled={applying} onClick={() => apply('', '')}>
            Relais publics
          </button>
        )}
      </div>
      <div className="setting-sub" style={{ marginTop: 6 }}>
        {status === null
          ? 'Connexion au relais…'
          : status.connected
            ? <span style={{ color: 'var(--success)' }}>Connecté à {status.custom ? 'votre relais' : 'un relais public'} ({new URL(status.url).host})</span>
            : <span style={{ color: 'var(--danger)' }}>Relais injoignable{status.error ? ` : ${status.error}` : ''}</span>}
      </div>
      <div className="setting-sub" style={{ marginTop: 6 }}>
        Pour les réseaux qui bloquent les relais publics (eduroam…). Vos amis doivent indiquer le même relais.
      </div>
    </div>
  );
}
