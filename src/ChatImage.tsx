import { memo, useEffect, useState } from 'react';
import type { ReactNode } from 'react';
import { api } from './api';
import type { ChatMessage, Transfer, TransferStatus } from './api';
import { formatBytes } from './format';
import { useTransfer } from './transferStore';

// Previews already made, by friend and message: the chat is reopened often.
// The least recently used go beyond this many.
const MAX_PREVIEWS = 60;
const previews = new Map<string, string | null>();

const getPreview = (key: string) => {
  if (!previews.has(key)) return undefined;
  const url = previews.get(key)!;
  // Used again: moved to the end, last to go.
  previews.delete(key);
  previews.set(key, url);
  return url;
};

const keepPreview = (key: string, url: string | null) => {
  previews.delete(key);
  previews.set(key, url);
  while (previews.size > MAX_PREVIEWS) previews.delete(previews.keys().next().value!);
};

const time = (ts: number) => new Date(ts).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' });

const outLabel = (status: TransferStatus | undefined, t: Transfer | undefined) => {
  switch (status) {
    case 'completed': return ' ✓✓';
    case 'declined': return ' · Refusée';
    case 'cancelled': return ' · Annulée';
    case 'error': return ' · Non envoyée';
    case 'sending': return ` · ${t?.progress ?? 0} %`;
    case 'paused': return ' · Reconnexion…';
    case 'finishing': return ' · Envoi…';
    case undefined: return '';
    default: return ' · En attente';
  }
};

// An image sent or offered in a friend's chat (its bubble). Its state comes from the
// transfer while it is in the list, else from what the chat kept of it.
// Renders on its own transfer's updates only.
export const ChatImage = memo(function ChatImage({ friendId, message, mine, onShown, quote }: {
  friendId: string;
  message: ChatMessage;
  mine: boolean;
  onShown: () => void;
  // The message it answers.
  quote?: ReactNode;
}) {
  const transfer = useTransfer(message.id);
  const file = message.file!;
  const status = transfer?.status ?? file.state;
  // Ours is on disk all along; theirs once received.
  const visible = mine || status === 'completed';
  const key = `${friendId}/${message.id}`;
  const [preview, setPreview] = useState<string | null | undefined>(() => getPreview(key));

  useEffect(() => {
    if (!visible || previews.has(key)) return;
    let live = true;
    api.chatImage(friendId, message.id).then(url => {
      keepPreview(key, url);
      if (live) setPreview(url);
    });
    return () => { live = false; };
  }, [visible, key, friendId, message.id]);

  const open = () => api.openChatFile(friendId, message.id);

  let body;
  if (visible && preview) {
    body = <img className="chat-image" src={preview} alt={file.name} title="Ouvrir" onClick={open} onLoad={onShown} />;
  } else if (visible) {
    body = (
      <button className="image-placeholder list-button" onClick={open} title="Ouvrir">
        {preview === undefined ? 'Chargement…' : `${file.name} · aperçu indisponible`}
      </button>
    );
  } else {
    let state;
    switch (status) {
      case 'pending':
        state = transfer?.queued ? <div className="item-sub">En file d’attente</div> : (
          <div style={{ display: 'flex', gap: 6, marginTop: 6 }}>
            <button className="btn btn-small" style={{ flex: 1 }} onClick={() => api.respondToFileOffer(message.id, true)}>Accepter</button>
            <button className="btn btn-secondary btn-small" style={{ flex: 1 }} onClick={() => api.respondToFileOffer(message.id, false)}>Refuser</button>
          </div>
        );
        break;
      case 'receiving':
      case 'paused':
      case 'finishing':
        state = (
          <div className="progress-track" style={{ marginTop: 6 }}>
            <div className="progress-fill" style={{ width: `${transfer?.progress ?? 0}%`, background: 'var(--accent)' }} />
          </div>
        );
        break;
      case 'declined':
        state = <div className="item-sub">Refusée</div>;
        break;
      default:
        state = <div className="item-sub">Non reçue</div>;
    }
    body = (
      <div className="image-placeholder">
        <div className="ellipsis" title={file.name}>Image · {formatBytes(file.size)}</div>
        {state}
      </div>
    );
  }

  return (
    <div className={`bubble image-bubble ${mine ? 'mine' : 'theirs'}`}>
      {quote}
      {body}
      <div className="bubble-time">
        {time(message.ts)}
        {mine && outLabel(status, transfer)}
      </div>
    </div>
  );
});
