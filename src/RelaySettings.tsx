import { useState } from 'react';
import { api } from './api';
import type { Config } from './api';
import { tr } from './i18n';
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
      <div className="setting-label" style={{ marginBottom: 6 }}>{tr('Relais personnel', 'Personal relay')}</div>
      <input className="input" placeholder={tr('https://relais.exemple.fr', 'https://relay.example.com')} value={url} onChange={e => setUrl(e.target.value)} spellCheck={false} />
      <input className="input" style={{ marginTop: 6 }} type="password" placeholder={tr('Jeton (si le relais en demande un)', 'Token (if the relay asks for one)')} value={token} onChange={e => setToken(e.target.value)} spellCheck={false} />
      {error && <div className="error-text">{error}</div>}
      <div style={{ display: 'flex', gap: 8, marginTop: 8, alignItems: 'center' }}>
        <button className="btn btn-secondary btn-small" disabled={!changed || applying} onClick={() => apply(url, token)}>
          {tr('Appliquer et redémarrer', 'Apply and restart')}
        </button>
        {config.relayUrl && (
          <button className="btn btn-secondary btn-small" disabled={applying} onClick={() => apply('', '')}>
            {tr('Relais publics', 'Public relays')}
          </button>
        )}
      </div>
      <div className="setting-sub" style={{ marginTop: 6 }}>
        {status === null
          ? connection === 'blocked'
            ? <span style={{ color: 'var(--danger)' }}>{tr('Aucun relais ne répond', 'No relay answers')}</span>
            : tr('Connexion au relais…', 'Connecting to the relay…')
          : status.connected
            ? config.relayUrl && !status.custom
              ? <span style={{ color: 'var(--warning)' }}>{tr('Votre relais ne répond pas : relais public en attendant', 'Your relay does not answer: a public relay meanwhile')} ({host(status.url)})</span>
              : <span style={{ color: 'var(--success)' }}>
                {status.custom ? tr('Connecté à votre relais', 'Connected to your relay') : tr('Connecté à un relais public', 'Connected to a public relay')} ({host(status.url)})
              </span>
            : <span style={{ color: 'var(--danger)' }}>{tr('Relais injoignable', 'Relay unreachable')}{status.error ? `${tr(' : ', ': ')}${status.error}` : ''}</span>}
      </div>
      <div className="setting-sub" style={{ marginTop: 6 }}>
        {tr(
          'Votre relais passe en premier : vos amis qui l’ont aussi échangent avec vous par lui, plus vite que par les relais publics. S’il ne répond pas, les relais publics prennent le relais. Sur un réseau qui bloque les relais publics (eduroam…), l’app les joint à travers votre relais : vos amis qui ne l’ont pas restent joignables.',
          'Your relay comes first: your friends who have it too exchange with you through it, faster than through the public relays. If it does not answer, the public relays take over. On a network that blocks the public relays (eduroam…), the app reaches them through your relay: your friends who do not have it stay reachable.',
        )}
      </div>
    </div>
  );
}
