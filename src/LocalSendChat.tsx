import { useEffect, useMemo, useRef, useState } from 'react';
import type { ReactNode } from 'react';
import { api } from './api';
import type { LocalSendDevice, LocalSendMessage } from './api';
import { ChatInput, PendingMark } from './chatParts';
import { chatRows } from './chatRows';
import { formatTime, plural } from './format';
import { tr } from './i18n';
import { Icon } from './icons';
import { WindowButtons } from './PinButton';
import { isMobile } from './platform';
import { RichText } from './RichText';

const MAX_TEXT = 5000;

// The chat with a LocalSend device: text sent from its LocalSend app lands
// here, and what is written here reaches it as a LocalSend text message.
// Notifications and deletion are in the device's menu, as for a friend.
export function LocalSendChatView({ fingerprint, notice, dropping, onBack, onToast, onPasteFiles }: {
  // Files pasted in the input.
  onPasteFiles: (files: File[]) => void;
  // Files are dragged over the window: they go to this device.
  dropping?: boolean;
  fingerprint: string;
  // Offers shown above the chat, as in the other views.
  notice: ReactNode[];
  onBack: () => void;
  onToast: (text: string) => void;
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

  const name = device?.alias || alias || tr('Appareil LocalSend', 'LocalSend device');

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
              {m.mine && m.status === 'delivered' && <span title={tr('Reçu par l’appareil', 'Received by the device')}> ✓✓</span>}
              {m.unverified && <span title={tr('Ancienne version de LocalSend : rien ne prouve que ce message vient bien de cet appareil.', 'Older LocalSend: nothing proves this message comes from this device.')}>{tr(' · non vérifié', ' · unverified')}</span>}
            </div>
          )}
          {m.mine && m.status === 'failed' && (
            <div className="bubble-time" style={{ color: 'var(--danger)' }}>{tr('Non envoyé', 'Not sent')}{m.error ? `${tr(' : ', ': ')}${m.error}` : ''}</div>
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

  // Files only: LocalSend gets no folders from here.
  const attachFiles = async () => {
    setError('');
    try {
      const count = await api.sendToLocalSend(fingerprint);
      if (count > 0) {
        onToast(tr(
          `${plural(count, 'fichier envoyé', 'fichiers envoyés')} à ${name} : suivez l’envoi dans l’onglet Transferts.`,
          `${plural(count, 'file sent', 'files sent')} to ${name}: follow it in the Transfers tab.`,
        ));
      }
    } catch (e) {
      setError(String(e));
    }
  };

  return (
    <div className={`app ${dropping ? 'dropping' : ''}`}>
      <div className="header">
        <div className="brand">
          <button className="icon-btn" title={tr('Retour', 'Back')} onClick={onBack}><Icon name="back" /></button>
          <div className="avatar small"><Icon name={device?.deviceType === 'mobile' ? 'phone' : 'monitor'} size={14} /></div>
          <div className="header-title">
            <div className="title">{name}</div>
            <div className={`subtitle ${device ? 'online' : ''}`}>
              {device
                ? `LocalSend · ${device.secure ? tr('même réseau, chiffré', 'same network, encrypted') : tr('non chiffré (ancienne version)', 'not encrypted (older version)')}`
                : tr('LocalSend · introuvable sur le réseau', 'LocalSend · not found on the network')}
            </div>
          </div>
        </div>
        <div className="header-actions">
          <WindowButtons />
        </div>
      </div>

      {notice.length > 0 && <div className="content notices">{notice}</div>}

      <div className="chat-messages">
        {messages.length === 0 && (
          <div className="empty-state">
            <Icon name="chat" size={32} />
            {tr('Aucun message. Le texte envoyé depuis LocalSend sur cet appareil arrivera ici.', 'No messages. Text sent from LocalSend on this device will arrive here.')}
            {!isMobile && tr(' Glissez un fichier ici pour le lui envoyer.', ' Drop a file here to send it.')}
          </div>
        )}
        {rows}
        <div ref={endRef} />
      </div>

      <ChatInput
        value={input}
        onChange={setInput}
        onSend={send}
        placeholder={device ? tr('Écrire un message…', 'Write a message…') : tr('Appareil introuvable : ouvrez LocalSend dessus', 'Device not found: open LocalSend on it')}
        maxLength={MAX_TEXT}
        error={error}
        onPasteFiles={device ? onPasteFiles : undefined}
        before={
          <button
            className="icon-btn attach"
            title={device ? tr('Envoyer un fichier', 'Send a file') : tr('Appareil introuvable sur le réseau', 'Device not found on the network')}
            disabled={!device}
            onClick={attachFiles}
          >
            <Icon name="paperclip" />
          </button>
        }
      />
    </div>
  );
}
