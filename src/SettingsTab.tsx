import { useState } from 'react';
import { api } from './api';
import type { Config, Friend, NotifySettings } from './api';
import type { LangChoice } from './i18n';
import { BackupRow } from './Backup';
import { langChoice, saveLangChoice, systemLang, tr } from './i18n';
import { Icon } from './icons';
import { LocalNetworkRow } from './LocalSendSettings';
import { isLinux, isMac, isMobile, isWindows, MOBILE_DOWNLOADS } from './platform';
import { RelaySettings } from './RelaySettings';
import { UpdateRows } from './Update';

export type Theme = 'light' | 'dark' | 'system';

const MB = 1024 * 1024;
const SPEEDS: { value: number; label: string }[] = [
  { value: 0, label: tr('Aucune', 'None') },
  { value: MB, label: tr('1 Mo/s', '1 MB/s') },
  { value: 5 * MB, label: tr('5 Mo/s', '5 MB/s') },
  { value: 10 * MB, label: tr('10 Mo/s', '10 MB/s') },
  { value: 50 * MB, label: tr('50 Mo/s', '50 MB/s') },
];

const THEMES: { value: Theme; label: string }[] = [
  { value: 'light', label: tr('Clair', 'Light') },
  { value: 'dark', label: tr('Sombre', 'Dark') },
  { value: 'system', label: tr('Système', 'System') },
];

// Each language in its own words.
const LANGUAGES: { value: LangChoice; label: string }[] = [
  { value: '', label: tr('Système', 'System') },
  { value: 'fr', label: 'Français' },
  { value: 'en', label: 'English' },
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
        <button className="icon-btn" title={tr('Retour', 'Back')} onClick={onBack}><Icon name="back" /></button>
        <span>{tr('Paramètres', 'Settings')}</span>
      </div>

      <div className="settings-heading">{tr('Profil', 'Profile')}</div>
      <div className="card rows">
        <div>
          <div className="setting-label">{tr('Pseudo', 'Nickname')}{saved && <span className="saved-mark">{tr(' · Enregistré ✓', ' · Saved ✓')}</span>}</div>
          <input
            className="input"
            style={{ marginTop: 6 }}
            maxLength={40}
            placeholder={tr('Votre pseudo', 'Your nickname')}
            value={nickname}
            onChange={e => setNickname(e.target.value)}
            onBlur={saveNickname}
            onKeyDown={e => e.key === 'Enter' && e.currentTarget.blur()}
          />
          <div className="setting-sub" style={{ marginTop: 4 }}>{tr('Vu par vos amis, vos groupes et les appareils LocalSend.', 'Seen by your friends, your groups and LocalSend devices.')}</div>
        </div>
      </div>

      <div className="settings-heading">{tr('Général', 'General')}</div>
      <div className="card rows">
        <div>
          <div className="setting-label">{tr('Langue', 'Language')}</div>
          <div className="segmented" role="radiogroup" style={{ marginTop: 6 }}>
            {LANGUAGES.map(l => (
              <button
                key={l.value || 'system'}
                role="radio"
                aria-checked={langChoice === l.value}
                className={langChoice === l.value ? 'active' : ''}
                onClick={() => {
                  if (l.value === langChoice) return;
                  saveLangChoice(l.value);
                  // Told before reloading: the backend's texts follow at once.
                  api.setLanguage(l.value || systemLang);
                  window.location.reload();
                }}
              >
                {l.label}
              </button>
            ))}
          </div>
        </div>
        <div>
          <div className="setting-label">{tr('Thème', 'Theme')}</div>
          <div className="segmented" role="radiogroup" style={{ marginTop: 6 }}>
            {THEMES.map(t => (
              <button key={t.value} role="radio" aria-checked={theme === t.value} className={theme === t.value ? 'active' : ''} onClick={() => setTheme(t.value)}>
                {t.label}
              </button>
            ))}
          </div>
        </div>
        <div>
          <div className="setting-label">{tr('Dossier de téléchargement', 'Download folder')}</div>
          {isMobile
            ? <div className="path-box" style={{ marginTop: 6 }}>{MOBILE_DOWNLOADS}</div>
            : (
              <div style={{ display: 'flex', gap: 8, alignItems: 'center', marginTop: 6 }}>
                <div className="path-box" title={config.downloadPath}>{config.downloadPath || tr('Dossier système', 'System folder')}</div>
                <button className="btn btn-secondary btn-small" onClick={async () => {
                  const p = await api.selectDownloadDir();
                  if (p) setConfig(c => ({ ...c, downloadPath: p }));
                }}>{tr('Changer', 'Change')}</button>
              </div>
            )}
        </div>
        {isMobile
          ? backgroundOk && (
            <div className="setting-row">
              <div>
                <div className="setting-label">{tr('Rester joignable en arrière-plan', 'Stay reachable in the background')}</div>
                <div className="setting-sub">{tr('Recevoir messages et fichiers l’application fermée, avec une notification permanente.', 'Receive messages and files with the app closed, with an ongoing notification.')}</div>
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
                <div className="setting-label">{tr('Lancer au démarrage', 'Start with the computer')}</div>
                <div className="setting-sub">
                  {isMac
                    ? tr('Avec l’ordinateur, dans la barre des menus', 'In the menu bar')
                    : tr('Avec l’ordinateur, dans la zone de notification', 'In the notification area')}
                </div>
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
        {(isWindows || isLinux) && (
          <div className="setting-row">
            <div>
              <div className="setting-label">{tr('Envoyer avec P2P Share', 'Send with P2P Share')}</div>
              <div className="setting-sub">
                {isWindows
                  ? tr(
                    'Dans le clic droit sur un fichier ou un dossier de l’Explorateur (sous « Afficher plus d’options » sur Windows 11).',
                    'In the right-click menu of a file or folder in File Explorer (under “Show more options” on Windows 11).',
                  )
                  : tr('Dans « Ouvrir avec » de votre gestionnaire de fichiers.', 'In “Open with” of your file manager.')}
              </div>
            </div>
            <Toggle
              checked={config.sendMenu ?? true}
              onChange={async checked => {
                const val = await api.setSendMenu(checked);
                setConfig(c => ({ ...c, sendMenu: val }));
              }}
            />
          </div>
        )}
        {isMac && (
          <div>
            <div className="setting-label">{tr('Envoyer avec P2P Share', 'Send with P2P Share')}</div>
            <div className="setting-sub">{tr('Dans le Finder, clic droit sur un fichier ou un dossier, puis « Ouvrir avec » → P2P Share.', 'In the Finder, right-click a file or a folder, then “Open With” → P2P Share.')}</div>
          </div>
        )}
      </div>

      <div className="settings-heading">{tr('Discussions et notifications', 'Chats and notifications')}</div>
      <div className="card rows">
        <div className="setting-row">
          <div>
            <div className="setting-label">{tr('Confirmations de lecture', 'Read receipts')}</div>
            <div className="setting-sub">{tr('« Vu » et « en train d’écrire… ». Désactivées, vous ne les voyez plus non plus.', '“Seen” and “typing…”. Turned off, you no longer see them either.')}</div>
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

      <button className="section-toggle list-button" aria-expanded={network} onClick={() => setNetwork(!network)}>
        <Icon name={network ? 'chevronDown' : 'chevronRight'} size={14} />
        {tr('Réseau', 'Network')}
        <span className="setting-sub">{tr('Vitesse, relais, réseau local', 'Speed, relay, local network')}</span>
      </button>
      {network && (
        <div className="card rows">
          <div>
            <div className="setting-label">{tr('Limiter la vitesse', 'Limit the speed')}</div>
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
            <div className="setting-sub" style={{ marginTop: 4 }}>{tr('Dans chaque sens, pour laisser de la place au reste du réseau.', 'Each way, to leave room for the rest of the network.')}</div>
          </div>
          <RelaySettings config={config} />
          <LocalNetworkRow nickname={config.nickname} enabled={config.localNetwork ?? true} />
        </div>
      )}

      <button className="section-toggle list-button" aria-expanded={advanced} onClick={() => setAdvanced(!advanced)}>
        <Icon name={advanced ? 'chevronDown' : 'chevronRight'} size={14} />
        {tr('Avancé', 'Advanced')}
        <span className="setting-sub">{tr('Sauvegarde, identifiant P2P, ajout par ID', 'Backup, P2P ID, adding by ID')}</span>
      </button>
      {advanced && (
        <div className="card rows">
          <BackupRow />
          <div className="setting-row">
            <div style={{ minWidth: 0 }}>
              <div className="setting-label">{tr('Mon identifiant P2P', 'My P2P ID')}</div>
              <div className="id-value" title={myId}>{myId}</div>
            </div>
            <button className="btn btn-secondary btn-small" onClick={copy}><Icon name="copy" size={14} />{copied ? tr('Copié !', 'Copied!') : tr('Copier', 'Copy')}</button>
          </div>
          <AddById onFriends={onFriends} />
        </div>
      )}

      <div className="settings-heading">{tr('À propos', 'About')}</div>
      <div className="card rows">
        <UpdateRows config={config} setConfig={setConfig} />
      </div>
    </>
  );
}

const ADD_FRIEND_ERRORS: Record<string, string> = tr({
  invalid_id: 'ID invalide : il doit faire 64 caractères (0-9, a-f).',
  self: 'C’est votre propre ID.',
}, {
  invalid_id: 'Invalid ID: it must be 64 characters (0-9, a-f).',
  self: 'This is your own ID.',
});

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
      setError(ADD_FRIEND_ERRORS[res.error] ?? tr('Impossible d’ajouter cet ami.', 'Could not add this friend.'));
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
        <div className="setting-label">{tr('Ajouter un ami par son ID', 'Add a friend by their ID')}{added && <span className="saved-mark">{tr(' · Ajouté ✓', ' · Added ✓')}</span>}</div>
        <div className="setting-sub">{tr('Pour un ami resté en version 3.4.5 ou avant, qui n’a pas les liens d’invitation : échangez vos ID, et ajoutez-vous tous les deux.', 'For a friend still on version 3.4.5 or earlier, without invitation links: swap your IDs, and both add each other.')}</div>
      </div>
      <input className="input" placeholder={tr('ID de l’ami (64 caractères)', 'Friend’s ID (64 characters)')} value={id} onChange={e => { setId(e.target.value); setError(''); }} />
      <input className="input" placeholder={tr('Nom (facultatif : son pseudo sinon)', 'Name (optional: their nickname otherwise)')} maxLength={40} value={name} onChange={e => setName(e.target.value)} onKeyDown={e => e.key === 'Enter' && add()} />
      {error && <div className="error-text">{error}</div>}
      <button className="btn btn-secondary" disabled={!id.trim()} onClick={add}><Icon name="plus" size={14} />{tr('Ajouter cet ami', 'Add this friend')}</button>
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
      {row('Notifications', tr('Les messages et fichiers arrivent toujours, seules les alertes changent.', 'Messages and files always arrive, only the alerts change.'), n.enabled, { enabled: !n.enabled })}
      {n.enabled && (
        <>
          {row('Messages', tr('Vos amis et les appareils LocalSend', 'Your friends and LocalSend devices'), n.messages, { messages: !n.messages }, true)}
          {row(tr('Fichiers', 'Files'), tr('Fichiers et dossiers proposés ou reçus', 'Files and folders offered or received'), n.files, { files: !n.files }, true)}
          {row(tr('Groupes', 'Groups'), tr('Messages, mentions et invitations', 'Messages, mentions and invitations'), n.groups, { groups: !n.groups }, true)}
          <div className="setting-sub nested">
            {tr('Pour une seule discussion : menu ⋯ d’un ami ou d’un appareil LocalSend, réglages d’un groupe.', 'For one chat only: the ⋯ menu of a friend or a LocalSend device, a group’s settings.')}
          </div>
        </>
      )}
    </>
  );
}
