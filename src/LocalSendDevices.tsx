import { useEffect, useRef, useState } from 'react';
import { api } from './api';
import type { LocalSendChat, LocalSendDevice } from './api';
import { Icon } from './icons';

// How long a search shows as in progress: devices answer within a second or two.
const SCAN_TIME = 3000;

// Devices running LocalSend on the same network, to send files and text to,
// and the devices we have chatted with that are not around right now.
export function LocalSendDevices({ onSent, onOpenChat }: {
  onSent: () => void;
  onOpenChat: (fingerprint: string) => void;
}) {
  const [devices, setDevices] = useState<LocalSendDevice[]>([]);
  const [chats, setChats] = useState<LocalSendChat[]>([]);
  // A search starts with the component.
  const [scanning, setScanning] = useState(true);
  const [error, setError] = useState('');
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

  useEffect(() => {
    const load = () => api.getLocalSendDevices().then(setDevices);
    const loadChats = () => api.getLocalSendChats().then(setChats);
    load();
    loadChats();
    startScan();
    const unsubscribers = [api.onLocalSendDevices(load), api.onLocalSendChat(loadChats)];
    return () => {
      clearTimeout(scanTimer.current);
      unsubscribers.forEach(unsubscribe => unsubscribe());
    };
  }, []);

  const send = async (device: LocalSendDevice) => {
    setError('');
    try {
      if (await api.sendToLocalSend(device.fingerprint) > 0) onSent();
    } catch (e) {
      setError(String(e));
    }
  };

  const unread = (fingerprint: string) => chats.find(c => c.fingerprint === fingerprint)?.unread ?? 0;
  const present = new Set(devices.map(d => d.fingerprint));
  const absent = chats.filter(c => !present.has(c.fingerprint));

  const chatButton = (fingerprint: string) => (
    <button className="icon-btn" title="Chat" onClick={() => onOpenChat(fingerprint)} style={{ position: 'relative' }}>
      <Icon name="chat" />
      {unread(fingerprint) > 0 && (
        <span className="badge" style={{ position: 'absolute', top: -4, right: -4 }}>{unread(fingerprint)}</span>
      )}
    </button>
  );

  return (
    <>
      <div className="section-header section-header-row">
        <span>Appareils LocalSend{devices.length > 0 ? ` (${devices.length})` : ''}</span>
        <button className="icon-btn small" title="Rechercher à nouveau" disabled={scanning} onClick={scan}>
          <Icon name="sync" size={14} />
        </button>
      </div>
      {devices.length > 0 && (
        <div className="list">
          {devices.map(d => (
            <div key={d.fingerprint} className="list-item">
              <div className="avatar"><Icon name={d.deviceType === 'mobile' ? 'phone' : 'monitor'} /></div>
              <div className="item-body">
                <div className="item-title">{d.alias}</div>
                <div className="item-sub">
                  {d.deviceModel ?? 'LocalSend'}
                  {!d.secure && <span style={{ color: 'var(--danger)' }} title="Ancienne version de LocalSend : les fichiers passent en clair sur le réseau, et rien ne prouve que c’est bien cet appareil."> · non chiffré</span>}
                </div>
              </div>
              <div className="item-actions">
                {chatButton(d.fingerprint)}
                <button className="icon-btn" title="Envoyer des fichiers" onClick={() => send(d)}><Icon name="fileUp" /></button>
              </div>
            </div>
          ))}
        </div>
      )}
      {devices.length === 0 && (
        <div className="setting-sub" style={{ padding: '0 2px' }}>
          {scanning ? 'Recherche…' : 'Aucun appareil. Ouvrez LocalSend sur l’appareil (même Wi-Fi), puis relancez la recherche.'}
        </div>
      )}
      {absent.length > 0 && (
        <div className="list">
          {absent.map(c => (
            <div key={c.fingerprint} className="list-item">
              <div className="avatar"><Icon name="chat" /></div>
              <div className="item-body">
                <div className="item-title">{c.alias || 'Appareil LocalSend'}</div>
                <div className="item-sub ellipsis">Hors réseau · {c.lastText}</div>
              </div>
              <div className="item-actions">{chatButton(c.fingerprint)}</div>
            </div>
          ))}
        </div>
      )}
      {error && <div className="error-text">{error}</div>}
    </>
  );
}
