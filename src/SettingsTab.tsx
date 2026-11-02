import { useState } from 'react';
import { api } from './api';
import type { Config, NotifySettings } from './api';
import { BackupCard } from './Backup';
import { Icon } from './icons';
import { LocalSendSettings } from './LocalSendSettings';
import { byPlatform, isMobile, MOBILE_DOWNLOADS } from './platform';
import { RelaySettings } from './RelaySettings';

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

export function SettingsTab({ config, setConfig, myId, theme, setTheme, advancedOpen, onBack }: {
  config: Partial<Config>;
  setConfig: (update: (c: Partial<Config>) => Partial<Config>) => void;
  myId: string;
  theme: Theme;
  setTheme: (theme: Theme) => void;
  // Opened on the network settings (from the connection warning).
  advancedOpen: boolean;
  onBack: () => void;
}) {
  const [nickname, setNickname] = useState(config.nickname ?? '');
  const [saved, setSaved] = useState(false);
  const [advanced, setAdvanced] = useState(advancedOpen);
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
    navigator.clipboard.writeText(myId);
    setCopied(true);
    setTimeout(() => setCopied(false), 2000);
  };

  return (
    <>
      <div className="page-title">
        <button className="icon-btn" title="Retour" onClick={onBack}><Icon name="back" /></button>
        <span>Paramètres</span>
      </div>

      <div className="card">
        <div className="card-label">Mon pseudo{saved && <span className="saved-mark"> · Enregistré ✓</span>}</div>
        <input
          className="input"
          maxLength={40}
          placeholder="Votre pseudo"
          value={nickname}
          onChange={e => setNickname(e.target.value)}
          onBlur={saveNickname}
          onKeyDown={e => e.key === 'Enter' && e.currentTarget.blur()}
        />
        <div className="setting-sub" style={{ marginTop: 6 }}>Vu par vos amis, les membres de vos groupes et les appareils LocalSend.</div>
      </div>

      <div className="card">
        <div className="card-label">Dossier de téléchargement</div>
        {isMobile ? (
          <>
            <div className="path-box">{MOBILE_DOWNLOADS}</div>
            <div className="setting-sub" style={{ marginTop: 6 }}>Les fichiers reçus arrivent dans ce dossier de votre appareil.</div>
          </>
        ) : (
          <div style={{ display: 'flex', gap: 8, alignItems: 'center' }}>
            <div className="path-box" title={config.downloadPath}>{config.downloadPath || 'Dossier système'}</div>
            <button className="btn btn-secondary btn-small" onClick={async () => {
              const p = await api.selectDownloadDir();
              if (p) setConfig(c => ({ ...c, downloadPath: p }));
            }}>Changer</button>
          </div>
        )}
      </div>

      <div className="card">
        <div className="card-label">Thème</div>
        <div className="segmented" role="radiogroup">
          {THEMES.map(t => (
            <button key={t.value} role="radio" aria-checked={theme === t.value} className={theme === t.value ? 'active' : ''} onClick={() => setTheme(t.value)}>
              {t.label}
            </button>
          ))}
        </div>
        {theme === 'system' && <div className="setting-sub" style={{ marginTop: 6 }}>{byPlatform('Suit le thème de Windows.', 'Suit le thème de l’appareil.')}</div>}
      </div>

      {isMobile ? (
        backgroundOk && (
          <div className="card">
            <div className="setting-row">
              <div>
                <div className="setting-label">Rester joignable en arrière-plan</div>
                <div className="setting-sub">Recevoir messages et fichiers l’application fermée. Android affiche alors une notification permanente.</div>
              </div>
              <label className="toggle">
                <input
                  type="checkbox"
                  checked={config.backgroundService ?? false}
                  onChange={async (e) => {
                    try {
                      const val = await api.setBackgroundService(e.target.checked);
                      setConfig(c => ({ ...c, backgroundService: val }));
                    } catch {
                      // Not available on this version: the setting goes away.
                      setBackgroundOk(false);
                    }
                  }}
                />
                <span className="toggle-track" />
              </label>
            </div>
          </div>
        )
      ) : (
        <div className="card">
          <div className="setting-row">
            <div>
              <div className="setting-label">Lancement au démarrage</div>
              <div className="setting-sub">Démarrer avec l’ordinateur, dans la zone de notification</div>
            </div>
            <label className="toggle">
              <input
                type="checkbox"
                checked={config.autoLaunch ?? true}
                onChange={async (e) => {
                  const val = await api.setAutoLaunch(e.target.checked);
                  setConfig(c => ({ ...c, autoLaunch: val }));
                }}
              />
              <span className="toggle-track" />
            </label>
          </div>
        </div>
      )}

      <div className="card">
        <div className="setting-row">
          <div>
            <div className="setting-label">Confirmations de lecture</div>
            <div className="setting-sub">« Vu » et « en train d’écrire… » avec vos amis. Désactivées, vous ne les voyez plus non plus.</div>
          </div>
          <label className="toggle">
            <input
              type="checkbox"
              checked={config.readReceipts ?? true}
              onChange={async (e) => {
                const val = await api.setReadReceipts(e.target.checked);
                setConfig(c => ({ ...c, readReceipts: val }));
              }}
            />
            <span className="toggle-track" />
          </label>
        </div>
      </div>

      <NotificationsCard config={config} setConfig={setConfig} />

      <BackupCard />

      <button className="section-toggle list-button" aria-expanded={advanced} onClick={() => setAdvanced(!advanced)}>
        <Icon name={advanced ? 'chevronDown' : 'chevronRight'} size={14} />
        Avancé
        <span className="setting-sub">Vitesse, LocalSend, relais, identifiant</span>
      </button>

      {advanced && (
        <>
          <div className="card">
            <div className="card-label">Limiter la vitesse</div>
            <div className="segmented" role="radiogroup">
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
            <div className="setting-sub" style={{ marginTop: 6 }}>Pour chaque sens (envoi, réception), pour laisser de la place au reste du réseau.</div>
          </div>

          <LocalSendSettings
            enabled={config.localsend ?? false}
            nickname={config.nickname}
            onChange={localsend => setConfig(c => ({ ...c, localsend }))}
          />

          <RelaySettings config={config} />

          <div className="card">
            <div className="section-header-row">
              <div className="card-label" style={{ marginBottom: 0 }}>Mon identifiant P2P</div>
              <button className="btn btn-secondary btn-small" onClick={copy}><Icon name="copy" size={14} />{copied ? 'Copié !' : 'Copier'}</button>
            </div>
            <div className="id-full" style={{ marginTop: 6 }}>{myId}</div>
          </div>
        </>
      )}
    </>
  );
}

// Which system notifications show. Each chat and group can also be made
// silent from its own menu.
function NotificationsCard({ config, setConfig }: {
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
  const row = (label: string, sub: string, checked: boolean, change: Partial<NotifySettings>, disabled = false) => (
    <div className={`setting-row ${disabled ? 'disabled' : ''}`}>
      <div>
        <div className="setting-label">{label}</div>
        <div className="setting-sub">{sub}</div>
      </div>
      <label className="toggle">
        <input type="checkbox" checked={checked} disabled={disabled} onChange={() => set(change)} />
        <span className="toggle-track" />
      </label>
    </div>
  );
  return (
    <div className="card stack">
      {row('Notifications', 'Les messages et fichiers arrivent toujours, seules les alertes changent.', n.enabled, { enabled: !n.enabled })}
      {row('Messages', 'Vos amis et les appareils LocalSend', n.messages, { messages: !n.messages }, !n.enabled)}
      {row('Fichiers', 'Fichiers et dossiers proposés ou reçus', n.files, { files: !n.files }, !n.enabled)}
      {row('Groupes', 'Messages, mentions et invitations', n.groups, { groups: !n.groups }, !n.enabled)}
      <div className="setting-sub">Pour faire taire une seule discussion : menu ⋯ d’un ami, cloche d’un appareil LocalSend, réglages d’un groupe.</div>
    </div>
  );
}
