import { useState } from 'react';
import { api } from './api';
import type { Config, Friend, NotifySettings } from './api';
import { BackupRow } from './Backup';
import { Icon } from './icons';
import { LocalSendStatusRow } from './LocalSendSettings';
import { isMac, isMobile, MOBILE_DOWNLOADS } from './platform';
import { RelaySettings } from './RelaySettings';
import { UpdateRows } from './Update';

export type Theme = 'light' | 'dark' | 'system';

const MB = 1024 * 1024;
const SPEEDS: { value: number; label: string }[] = [
  { value: 0, label: 'Aucune' },
  { value: MB, label: '1 Mo/s' },
  { value: 5 * MB, label: '5 Mo/s' },
  { value: 10 * MB, label: '10 Mo/s' },
  { value: 50 * MB, label: '50 Mo/s' },
];

const THEMES: { value: Theme; label: string }[] = [
  { value: 'light', label: 'Clair' },
  { value: 'dark', label: 'Sombre' },
  { value: 'system', label: 'Système' },
];

function Toggle({ checked, onChange }: { checked: boolean; onChange: (checked: boolean) => void }) {
  return (
    <label className="toggle">
      <input type="checkbox" checked={checked} onChange={e => onChange(e.target.checked)} />
      <span className="toggle-track" />
    </label>
  );
}

// Sections, each a title and one card of rows; the network and advanced ones folded.
export function SettingsTab({ config, setConfig, myId, onFriends, theme, setTheme, advancedOpen, onBack }: {
  config: Partial<Config>;
  setConfig: (update: (c: Partial<Config>) => Partial<Config>) => void;
  myId: string;
  // After a friend added by their ID.
  onFriends: (friends: Friend[]) => void;
  theme: Theme;
  setTheme: (theme: Theme) => void;
  // Opened on the network settings (from the connection warning).
  advancedOpen: boolean;
  onBack: () => void;
}) {
  const [nickname, setNickname] = useState(config.nickname ?? '');
  const [saved, setSaved] = useState(false);
  const [network, setNetwork] = useState(advancedOpen);
  const [advanced, setAdvanced] = useState(false);
  const [copied, setCopied] = useState(false);
  // Mobile: false once set_background_service failed (not in this backend).
  const [backgroundOk, setBackgroundOk] = useState(true);

  // Saved on leaving the field or Enter: no button to forget.
  const saveNickname = async () => {
    const trimmed = nickname.trim();
    if (!trimmed || trimmed === config.nickname) {
      setNickname(config.nickname ?? '');
      return;
    }
    const value = await api.setNickname(trimmed);
    setNickname(value);
    setConfig(c => ({ ...c, nickname: value }));
    setSaved(true);
    setTimeout(() => setSaved(false), 2000);
  };

  const copy = () => {
    navigator.clipboard.writeText(myId).then(() => {
      setCopied(true);
      setTimeout(() => setCopied(false), 2000);
    }, () => {});
  };

  return (
    <>
      <div className="page-title">
        <button className="icon-btn" title="Retour" onClick={onBack}><Icon name="back" /></button>
        <span>Paramètres</span>
      </div>

      <div className="settings-heading">Profil</div>
      <div className="card rows">
        <div>
          <div className="setting-label">Pseudo{saved && <span className="saved-mark"> · Enregistré ✓</span>}</div>
          <input
            className="input"
            style={{ marginTop: 6 }}
            maxLength={40}
            placeholder="Votre pseudo"
            value={nickname}
            onChange={e => setNickname(e.target.value)}
            onBlur={saveNickname}
            onKeyDown={e => e.key === 'Enter' && e.currentTarget.blur()}
          />
          <div className="setting-sub" style={{ marginTop: 4 }}>Vu par vos amis, vos groupes et les appareils LocalSend.</div>
        </div>
      </div>

      <div className="settings-heading">Général</div>
      <div className="card rows">
        <div>
          <div className="setting-label">Thème</div>
          <div className="segmented" role="radiogroup" style={{ marginTop: 6 }}>
            {THEMES.map(t => (
              <button key={t.value} role="radio" aria-checked={theme === t.value} className={theme === t.value ? 'active' : ''} onClick={() => setTheme(t.value)}>
                {t.label}
              </button>
            ))}
          </div>
        </div>
        <div>
          <div className="setting-label">Dossier de téléchargement</div>
          {isMobile
            ? <div className="path-box" style={{ marginTop: 6 }}>{MOBILE_DOWNLOADS}</div>
            : (
              <div style={{ display: 'flex', gap: 8, alignItems: 'center', marginTop: 6 }}>
                <div className="path-box" title={config.downloadPath}>{config.downloadPath || 'Dossier système'}</div>
                <button className="btn btn-secondary btn-small" onClick={async () => {
                  const p = await api.selectDownloadDir();
                  if (p) setConfig(c => ({ ...c, downloadPath: p }));
                }}>Changer</button>
              </div>
            )}
        </div>
        {isMobile
          ? backgroundOk && (
            <div className="setting-row">
              <div>
                <div className="setting-label">Rester joignable en arrière-plan</div>
                <div className="setting-sub">Recevoir messages et fichiers l’application fermée, avec une notification permanente.</div>
              </div>
              <Toggle
                checked={config.backgroundService ?? false}
                onChange={async checked => {
                  try {
                    const val = await api.setBackgroundService(checked);
                    setConfig(c => ({ ...c, backgroundService: val }));
                  } catch {
                    // Not available on this version: the setting goes away.
                    setBackgroundOk(false);
                  }
                }}
              />
            </div>
          )
          : (
            <div className="setting-row">
              <div>
                <div className="setting-label">Lancer au démarrage</div>
                <div className="setting-sub">Avec l’ordinateur, {isMac ? 'dans la barre des menus' : 'dans la zone de notification'}</div>
              </div>
              <Toggle
                checked={config.autoLaunch ?? true}
                onChange={async checked => {
                  const val = await api.setAutoLaunch(checked);
                  setConfig(c => ({ ...c, autoLaunch: val }));
                }}
              />
            </div>
          )}
      </div>

      <div className="settings-heading">Discussions et notifications</div>
      <div className="card rows">
        <div className="setting-row">
          <div>
            <div className="setting-label">Confirmations de lecture</div>
            <div className="setting-sub">« Vu » et « en train d’écrire… ». Désactivées, vous ne les voyez plus non plus.</div>
          </div>
          <Toggle
            checked={config.readReceipts ?? true}
            onChange={async checked => {
              const val = await api.setReadReceipts(checked);
              setConfig(c => ({ ...c, readReceipts: val }));
            }}
          />
        </div>
        <NotificationRows config={config} setConfig={setConfig} />
      </div>

      <div className="settings-heading">Sauvegarde</div>
      <div className="card rows">
        <BackupRow />
      </div>

      <button className="section-toggle list-button" aria-expanded={network} onClick={() => setNetwork(!network)}>
        <Icon name={network ? 'chevronDown' : 'chevronRight'} size={14} />
        Réseau
        <span className="setting-sub">Vitesse, relais, LocalSend</span>
      </button>
      {network && (
        <div className="card rows">
          <div>
            <div className="setting-label">Limiter la vitesse</div>
            <div className="segmented" role="radiogroup" style={{ marginTop: 6 }}>
              {SPEEDS.map(s => (
                <button
                  key={s.value}
                  role="radio"
                  aria-checked={(config.speedLimit ?? 0) === s.value}
                  className={(config.speedLimit ?? 0) === s.value ? 'active' : ''}
                  onClick={async () => {
                    const limit = await api.setSpeedLimit(s.value || null);
                    setConfig(c => ({ ...c, speedLimit: limit ?? 0 }));
                  }}
                >
                  {s.label}
                </button>
              ))}
            </div>
            <div className="setting-sub" style={{ marginTop: 4 }}>Dans chaque sens, pour laisser de la place au reste du réseau.</div>
          </div>
          <RelaySettings config={config} />
          <LocalSendStatusRow nickname={config.nickname} />
        </div>
      )}

      <button className="section-toggle list-button" aria-expanded={advanced} onClick={() => setAdvanced(!advanced)}>
        <Icon name={advanced ? 'chevronDown' : 'chevronRight'} size={14} />
        Avancé
        <span className="setting-sub">Identifiant P2P, ajout par ID</span>
      </button>
      {advanced && (
        <div className="card rows">
          <div className="setting-row">
            <div style={{ minWidth: 0 }}>
              <div className="setting-label">Mon identifiant P2P</div>
              <div className="id-value" title={myId}>{myId}</div>
            </div>
            <button className="btn btn-secondary btn-small" onClick={copy}><Icon name="copy" size={14} />{copied ? 'Copié !' : 'Copier'}</button>
          </div>
          <AddById onFriends={onFriends} />
        </div>
      )}

      <div className="settings-heading">À propos</div>
      <div className="card rows">
        <UpdateRows config={config} setConfig={setConfig} />
      </div>
    </>
  );
}

const ADD_FRIEND_ERRORS: Record<string, string> = {
  invalid_id: 'ID invalide : il doit faire 64 caractères (0-9, a-f).',
  self: 'C’est votre propre ID.',
};

// For a friend on 3.4.5 or older, without invitation links: both add the
// other's ID.
function AddById({ onFriends }: { onFriends: (friends: Friend[]) => void }) {
  const [id, setId] = useState('');
  const [name, setName] = useState('');
  const [error, setError] = useState('');
  const [added, setAdded] = useState(false);

  const add = async () => {
    if (!id.trim()) return;
    // The name may stay empty: the friend's nickname is then used.
    const res = await api.addFriend(id, name);
    if ('error' in res) {
      setError(ADD_FRIEND_ERRORS[res.error] ?? 'Impossible d’ajouter cet ami.');
      return;
    }
    setId('');
    setName('');
    setError('');
    setAdded(true);
    setTimeout(() => setAdded(false), 2000);
    onFriends(res.friends);
  };

  return (
    <div className="stack">
      <div>
        <div className="setting-label">Ajouter un ami par son ID{added && <span className="saved-mark"> · Ajouté ✓</span>}</div>
        <div className="setting-sub">Pour un ami resté en version 3.4.5 ou avant, qui n’a pas les liens d’invitation : échangez vos ID, et ajoutez-vous tous les deux.</div>
      </div>
      <input className="input" placeholder="ID de l’ami (64 caractères)" value={id} onChange={e => { setId(e.target.value); setError(''); }} />
      <input className="input" placeholder="Nom (facultatif : son pseudo sinon)" maxLength={40} value={name} onChange={e => setName(e.target.value)} onKeyDown={e => e.key === 'Enter' && add()} />
      {error && <div className="error-text">{error}</div>}
      <button className="btn btn-secondary" disabled={!id.trim()} onClick={add}><Icon name="plus" size={14} />Ajouter cet ami</button>
    </div>
  );
}

// Which system notifications show. Each chat and group can also be made
// silent from its own menu.
function NotificationRows({ config, setConfig }: {
  config: Partial<Config>;
  setConfig: (update: (c: Partial<Config>) => Partial<Config>) => void;
}) {
  const n = config.notifications ?? {
    enabled: true, messages: true, files: true, groups: true, mutedFriends: [], mutedLocalsend: [],
  };
  const set = async (change: Partial<NotifySettings>) => {
    const next = { ...n, ...change };
    const saved = await api.setNotifications(next.enabled, next.messages, next.files, next.groups);
    setConfig(c => ({ ...c, notifications: saved }));
  };
  const row = (label: string, sub: string, checked: boolean, change: Partial<NotifySettings>, nested = false) => (
    <div className={`setting-row ${nested ? 'nested' : ''}`}>
      <div>
        <div className="setting-label">{label}</div>
        <div className="setting-sub">{sub}</div>
      </div>
      <Toggle checked={checked} onChange={() => set(change)} />
    </div>
  );
  return (
    <>
      {row('Notifications', 'Les messages et fichiers arrivent toujours, seules les alertes changent.', n.enabled, { enabled: !n.enabled })}
      {n.enabled && (
        <>
          {row('Messages', 'Vos amis et les appareils LocalSend', n.messages, { messages: !n.messages }, true)}
          {row('Fichiers', 'Fichiers et dossiers proposés ou reçus', n.files, { files: !n.files }, true)}
          {row('Groupes', 'Messages, mentions et invitations', n.groups, { groups: !n.groups }, true)}
          <div className="setting-sub nested">Pour une seule discussion : menu ⋯ d’un ami, cloche d’un appareil LocalSend, réglages d’un groupe.</div>
        </>
      )}
    </>
  );
}
