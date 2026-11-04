import { useEffect, useRef, useState } from 'react';
import { api } from './api';
import type { LocalSendChat, LocalSendDevice, NotifySettings } from './api';
import { formatDate } from './format';
import { Icon } from './icons';
import { byPlatform, fitOnScreen } from './platform';

// How long a search shows as in progress: devices answer within a second or two.
const SCAN_TIME = 3000;

// A device in the list: one on the network, or one we chatted with that is
// not around right now.
type Row = { fingerprint: string; name: string; device?: LocalSendDevice; chat?: LocalSendChat };

// Devices running LocalSend on the same network, to send files and text to,
// and the devices we have chatted with that are not around right now. Their
// rows work like the friends': a click opens the chat, the rest is in the menu.
export function LocalSendDevices({ dropTarget, onSent, onOpenChat, muted, onMuted }: {
  // The device files dragged over the window would go to.
  dropTarget: string | null;
  onSent: () => void;
  onOpenChat: (fingerprint: string) => void;
  // Devices whose messages give no notification, and to change it.
  muted: string[];
  onMuted: (settings: NotifySettings) => void;
}) {
  const [devices, setDevices] = useState<LocalSendDevice[]>([]);
  const [chats, setChats] = useState<LocalSendChat[]>([]);
  // A search starts with the component.
  const [scanning, setScanning] = useState(true);
  const [error, setError] = useState('');
  // The device whose menu is open, and where (on the window: the list clips its rows).
  const [menu, setMenu] = useState<{ fingerprint: string; top: number; right: number } | null>(null);
  // The chat whose deletion waits for confirmation.
  const [deleting, setDeleting] = useState<Row | null>(null);
  const scanTimer = useRef<ReturnType<typeof setTimeout>>(undefined);

  // A new search restarts the delay: the previous one must not end it early.
  const startScan = () => {
    api.scanLocalSend();
    clearTimeout(scanTimer.current);
    scanTimer.current = setTimeout(() => setScanning(false), SCAN_TIME);
  };

  const scan = () => {
    setScanning(true);
    startScan();
  };

  const loadChats = () => api.getLocalSendChats().then(setChats);

  useEffect(() => {
    const load = () => api.getLocalSendDevices().then(setDevices);
    load();
    loadChats();
    startScan();
    const unsubscribers = [api.onLocalSendDevices(load), api.onLocalSendChat(loadChats)];
    return () => {
      clearTimeout(scanTimer.current);
      unsubscribers.forEach(unsubscribe => unsubscribe());
    };
  }, []);

  useEffect(() => {
    if (!menu) return;
    const close = () => setMenu(null);
    const onKey = (e: KeyboardEvent) => e.key === 'Escape' && close();
    window.addEventListener('click', close);
    window.addEventListener('keydown', onKey);
    return () => {
      window.removeEventListener('click', close);
      window.removeEventListener('keydown', onKey);
    };
  }, [menu]);

  const send = async (fingerprint: string) => {
    setError('');
    try {
      if (await api.sendToLocalSend(fingerprint) > 0) onSent();
    } catch (e) {
      setError(String(e));
    }
  };

  const remove = async (fingerprint: string) => {
    setDeleting(null);
    await api.deleteLocalSendChat(fingerprint).catch(() => {});
    loadChats();
  };

  const chatOf = (fingerprint: string) => chats.find(c => c.fingerprint === fingerprint);
  // A friend's P2P Share is in the friends already (an earlier chat with it
  // stays reachable below).
  const shown: Row[] = devices
    .filter(d => !d.friend)
    .map(d => ({ fingerprint: d.fingerprint, name: d.alias, device: d, chat: chatOf(d.fingerprint) }));
  const present = new Set(shown.map(r => r.fingerprint));
  const absent: Row[] = chats
    .filter(c => !present.has(c.fingerprint))
    .map(c => ({ fingerprint: c.fingerprint, name: c.alias || 'Appareil LocalSend', chat: c }));

  const row = (r: Row) => {
    const { device, chat } = r;
    const count = chat?.unread ?? 0;
    const isMuted = muted.includes(r.fingerprint);
    const last = chat?.lastText;
    const preview = device
      ? last ?? device.deviceModel ?? 'LocalSend'
      : `Hors réseau${last ? ` · ${last}` : ''}`;
    return (
      <div
        key={r.fingerprint}
        role="button"
        tabIndex={0}
        className={`list-item list-row ${dropTarget === r.fingerprint ? 'drop-target' : ''}`}
        data-drop-kind={device ? 'localsend' : undefined}
        data-drop-id={r.fingerprint}
        data-drop-name={r.name}
        onClick={() => onOpenChat(r.fingerprint)}
        onKeyDown={e => e.key === 'Enter' && onOpenChat(r.fingerprint)}
      >
        <div className="avatar"><Icon name={!device ? 'chat' : device.deviceType === 'mobile' ? 'phone' : 'monitor'} /></div>
        <div className="item-body">
          <div className="row-top">
            <span className={`item-title ${count > 0 ? 'unread' : ''}`}>
              {r.name}
              {isMuted && <span className="muted-mark" title="Notifications désactivées"><Icon name="bellOff" size={12} /></span>}
            </span>
            {chat && chat.lastTs > 0 && <span className="item-time">{formatDate(chat.lastTs)}</span>}
          </div>
          <div className="row-top">
            <span className={`item-sub ellipsis ${count > 0 ? 'unread' : ''}`}>
              {preview}
              {device && !device.secure && <span style={{ color: 'var(--danger)' }} title="Ancienne version de LocalSend : les fichiers passent en clair sur le réseau, et rien ne prouve que c’est bien cet appareil."> · non chiffré</span>}
            </span>
            {count > 0 && <span className="badge" title="Messages non lus">{count}</span>}
          </div>
        </div>
        <div className="item-actions" onClick={e => e.stopPropagation()}>
          {device && (
            <button className="icon-btn" title="Envoyer des fichiers" onClick={() => send(r.fingerprint)}><Icon name="fileUp" /></button>
          )}
          <div className="menu-anchor">
            <button
              className="icon-btn"
              title="Plus d’options"
              onClick={e => {
                e.stopPropagation();
                const b = e.currentTarget.getBoundingClientRect();
                setMenu(menu?.fingerprint === r.fingerprint ? null : { fingerprint: r.fingerprint, top: b.bottom + 4, right: window.innerWidth - b.right });
              }}
            >
              <Icon name="more" />
            </button>
            {menu?.fingerprint === r.fingerprint && (
              <div className="menu-popover" role="menu" ref={fitOnScreen} style={{ top: menu.top, right: menu.right }}>
                <button
                  role="menuitemcheckbox"
                  aria-checked={isMuted}
                  onClick={async () => { setMenu(null); onMuted(await api.setChatMuted('localsend', r.fingerprint, !isMuted)); }}
                >
                  <span className="menu-check">{isMuted && <Icon name="check" size={14} />}</span>
                  Désactiver les notifications
                </button>
                {chat && (
                  <button role="menuitem" className="danger" onClick={() => { setMenu(null); setDeleting(r); }}>
                    <Icon name="trash" size={14} />Supprimer la discussion
                  </button>
                )}
              </div>
            )}
          </div>
        </div>
      </div>
    );
  };

  return (
    <>
      <div className="section-header section-header-row">
        <span>À proximité (LocalSend){shown.length > 0 ? ` · ${shown.length}` : ''}</span>
        <button className="icon-btn small" title="Rechercher à nouveau" disabled={scanning} onClick={scan}>
          <Icon name="sync" size={14} />
        </button>
      </div>
      {shown.length > 0 && <div className="list">{shown.map(row)}</div>}
      {shown.length === 0 && (
        <div className="setting-sub" style={{ padding: '0 2px' }}>
          {scanning ? 'Recherche…' : 'Aucun appareil. Ouvrez LocalSend sur l’appareil (même Wi-Fi), puis relancez la recherche.'}
        </div>
      )}
      {absent.length > 0 && <div className="list">{absent.map(row)}</div>}
      {error && <div className="error-text">{error}</div>}

      {deleting && (
        <div className="modal-backdrop" onClick={() => setDeleting(null)}>
          <div className="modal" role="dialog" aria-modal="true" onClick={e => e.stopPropagation()}>
            <div className="modal-title">Supprimer la discussion avec {deleting.name} ?</div>
            <div className="modal-text">
              Les messages sont effacés de cet {byPlatform('ordinateur', 'appareil')}. Ceux de l’app LocalSend, sur l’autre appareil, n’y sont pas effacés.
            </div>
            <div style={{ display: 'flex', gap: 8 }}>
              <button className="btn btn-secondary" style={{ flex: 1 }} autoFocus onClick={() => setDeleting(null)}>Annuler</button>
              <button className="btn btn-danger" style={{ flex: 1 }} onClick={() => remove(deleting.fingerprint)}>Supprimer</button>
            </div>
          </div>
        </div>
      )}
    </>
  );
}
