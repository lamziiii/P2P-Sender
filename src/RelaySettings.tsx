import { useState } from 'react';
import { api } from './api';
import type { Config } from './api';
import { useConnection } from './relay';

// Self-hosted relay, for networks that block iroh's public relays (eduroam…).
export function RelaySettings({ config }: { config: Partial<Config> }) {
  const [url, setUrl] = useState(config.relayUrl ?? '');
  const [token, setToken] = useState(config.relayToken ?? '');
  const { status, connection } = useConnection();
  const [error, setError] = useState('');
  const [applying, setApplying] = useState(false);

  // Follow the saved setting when it loads or changes.
  const [saved, setSaved] = useState([config.relayUrl, config.relayToken]);
  if (saved[0] !== config.relayUrl || saved[1] !== config.relayToken) {
    setSaved([config.relayUrl, config.relayToken]);
    setUrl(config.relayUrl ?? '');
    setToken(config.relayToken ?? '');
  }

  const norm = (u: string) => u.trim().replace(/\/+$/, '');
  const host = (u: string) => {
    try {
      return new URL(u).host;
    } catch {
      return u;
    }
  };
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
    <div>
      <div className="setting-label" style={{ marginBottom: 6 }}>Relais personnel</div>
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
          ? connection === 'blocked'
            ? <span style={{ color: 'var(--danger)' }}>Aucun relais ne répond</span>
            : 'Connexion au relais…'
          : status.connected
            ? config.relayUrl && !status.custom
              ? <span style={{ color: 'var(--warning)' }}>Votre relais ne répond pas : relais public en attendant ({host(status.url)})</span>
              : <span style={{ color: 'var(--success)' }}>Connecté à {status.custom ? 'votre relais' : 'un relais public'} ({host(status.url)})</span>
            : <span style={{ color: 'var(--danger)' }}>Relais injoignable{status.error ? ` : ${status.error}` : ''}</span>}
      </div>
      <div className="setting-sub" style={{ marginTop: 6 }}>
        Votre relais passe en premier : vos amis qui l’ont aussi échangent avec vous par lui, plus vite que par les relais publics. S’il ne répond pas, les relais publics prennent le relais. Sur un réseau qui bloque les relais publics (eduroam…), l’app les joint à travers votre relais : vos amis qui ne l’ont pas restent joignables.
      </div>
    </div>
  );
}
