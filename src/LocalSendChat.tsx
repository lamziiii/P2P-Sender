import { useEffect, useMemo, useRef, useState } from 'react';
import type { ReactNode } from 'react';
import { api } from './api';
import type { LocalSendDevice, LocalSendMessage, NotifySettings } from './api';
import { ChatInput, PendingMark } from './chatParts';
import { chatRows } from './chatRows';
import { formatTime } from './format';
import { Icon } from './icons';
import { WindowButtons } from './PinButton';
import { RichText } from './RichText';

const MAX_TEXT = 5000;

// The chat with a LocalSend device: text sent from its LocalSend app lands
// here, and what is written here reaches it as a LocalSend text message.
export function LocalSendChatView({ fingerprint, notice, dropping, onBack, muted, onMuted }: {
  // Its messages give no notification, and to change it.
  muted: boolean;
  onMuted: (settings: NotifySettings) => void;
  // Files are dragged over the window: they go to this device.
  dropping?: boolean;
  fingerprint: string;
  // Offers shown above the chat, as in the other views.
  notice: ReactNode[];
  onBack: () => void;
}) {
  const [alias, setAlias] = useState('');
  const [messages, setMessages] = useState<LocalSendMessage[]>([]);
  const [device, setDevice] = useState<LocalSendDevice | null>(null);
  const [input, setInput] = useState('');
  const [error, setError] = useState('');
  const endRef = useRef<HTMLDivElement>(null);
  const sendingRef = useRef(false);

  useEffect(() => {
    const loadDevice = () => api.getLocalSendDevices()
      .then(list => setDevice(list.find(d => d.fingerprint === fingerprint) ?? null));
    loadDevice();
    api.getLocalSendChat(fingerprint).then(chat => {
      if (chat.alias) setAlias(chat.alias);
      // Keep what arrived while loading.
      setMessages(prev => {
        const known = new Set(chat.messages.map(m => m.id));
        return [...chat.messages, ...prev.filter(m => !known.has(m.id))];
      });
    });
    api.markLocalSendRead(fingerprint);
    const unsubscribers = [
      api.onLocalSendDevices(loadDevice),
      api.onLocalSendChat(update => {
        if (update.fingerprint !== fingerprint) return;
        if (update.alias) setAlias(update.alias);
        setMessages(prev => {
          const idx = prev.findIndex(m => m.id === update.message.id);
          if (idx === -1) return [...prev, update.message];
          const next = [...prev];
          next[idx] = update.message;
          return next;
        });
        if (!update.message.mine) api.markLocalSendRead(fingerprint);
      }),
    ];
    return () => unsubscribers.forEach(unsubscribe => unsubscribe());
  }, [fingerprint]);

  useEffect(() => {
    endRef.current?.scrollIntoView({ behavior: 'smooth' });
  }, [messages]);

  const name = device?.alias || alias || 'Appareil LocalSend';

  // Not laid out again on each key typed.
  const rows = useMemo(() => chatRows(messages, {
    ts: m => m.ts,
    sender: m => (m.mine ? 'me' : 'device'),
    key: m => m.id,
    render: (m, { cont, last }) => (
      <div key={m.id} className={`bubble-row ${m.mine ? 'mine' : ''} ${cont ? 'cont' : ''}`}>
        <div className={`bubble ${m.mine ? 'mine' : 'theirs'}`}>
          <RichText text={m.text} />
          {(last || m.status === 'pending' || m.unverified) && (
            <div className="bubble-time">
              {formatTime(m.ts)}
              {m.mine && m.status === 'pending' && <PendingMark />}
              {m.mine && m.status === 'delivered' && <span title="Reçu par l’appareil"> ✓✓</span>}
              {m.unverified && <span title="Ancienne version de LocalSend : rien ne prouve que ce message vient bien de cet appareil."> · non vérifié</span>}
            </div>
          )}
          {m.mine && m.status === 'failed' && (
            <div className="bubble-time" style={{ color: 'var(--danger)' }}>Non envoyé{m.error ? ` : ${m.error}` : ''}</div>
          )}
        </div>
      </div>
    ),
  }), [messages]);

  const send = async () => {
    const text = input.trim();
    // Enter pressed twice before the first send returns: send once.
    if (!text || sendingRef.current) return;
    sendingRef.current = true;
    setInput('');
    setError('');
    try {
      // The message comes back through onLocalSendChat too.
      const message = await api.sendLocalSendText(fingerprint, text);
      setMessages(prev => (prev.some(m => m.id === message.id) ? prev : [...prev, message]));
    } catch (e) {
      setInput(text);
      setError(String(e));
    } finally {
      sendingRef.current = false;
    }
  };

  return (
    <div className={`app ${dropping ? 'dropping' : ''}`}>
      <div className="header">
        <div className="brand">
          <button className="icon-btn" title="Retour" onClick={onBack}><Icon name="back" /></button>
          <div className="avatar small"><Icon name={device?.deviceType === 'mobile' ? 'phone' : 'monitor'} size={14} /></div>
          <div className="header-title">
            <div className="title">{name}</div>
            <div className={`subtitle ${device ? 'online' : ''}`}>
              {device
                ? `LocalSend · ${device.secure ? 'même réseau, chiffré' : 'non chiffré (ancienne version)'}`
                : 'LocalSend · introuvable sur le réseau'}
            </div>
          </div>
        </div>
        <div className="header-actions">
          <button
            className={`icon-btn ${muted ? 'active' : ''}`}
            title={muted ? 'Réactiver les notifications' : 'Désactiver les notifications'}
            onClick={async () => onMuted(await api.setChatMuted('localsend', fingerprint, !muted))}
          >
            <Icon name={muted ? 'bellOff' : 'bell'} />
          </button>
          <WindowButtons />
        </div>
      </div>

      {notice.length > 0 && <div className="content notices">{notice}</div>}

      <div className="chat-messages">
        {messages.length === 0 && (
          <div className="empty-state">
            <Icon name="chat" size={32} />
            Aucun message. Le texte envoyé depuis LocalSend sur cet appareil arrivera ici.
          </div>
        )}
        {rows}
        <div ref={endRef} />
      </div>

      <ChatInput
        value={input}
        onChange={setInput}
        onSend={send}
        placeholder={device ? 'Écrire un message…' : 'Appareil introuvable : ouvrez LocalSend dessus'}
        maxLength={MAX_TEXT}
        error={error}
      />
    </div>
  );
}
