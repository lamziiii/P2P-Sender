import { memo, useCallback, useEffect, useMemo, useRef, useState } from 'react';
import type { ReactNode } from 'react';
import { api, REACTIONS } from './api';
import type { ChatMessage, Friend } from './api';
import { ChatImage } from './ChatImage';
import { BubbleActions, ChatInput, ChatSearch, PendingMark } from './chatParts';
import { chatRows } from './chatRows';
import { folderSendError, formatTime, plural } from './format';
import { Avatar } from './FriendsTab';
import { Icon } from './icons';
import { WindowButtons } from './PinButton';
import { isMobile, useFoldersSupported, useLongPress } from './platform';
import { RichText } from './RichText';
import { jumpTo, useDismiss } from './ui';

// The typing signal is sent at most this often.
const TYPING_EVERY = 3000;

// The reactions under a bubble: one chip per emoji, with its count; ours
// stands out, and a click sets or takes back ours.
function Reactions({ reactions, myId, friendName, onReact }: {
  reactions: Record<string, string>;
  myId: string;
  friendName: string;
  onReact: (emoji: string) => void;
}) {
  const byEmoji = new Map<string, string[]>();
  for (const [author, emoji] of Object.entries(reactions)) {
    byEmoji.set(emoji, [...(byEmoji.get(emoji) ?? []), author]);
  }
  if (byEmoji.size === 0) return null;
  return (
    <div className="reactions">
      {[...byEmoji].map(([emoji, authors]) => {
        const mine = authors.includes(myId);
        const names = authors.map(a => (a === myId ? 'Vous' : friendName)).join(', ');
        return (
          <button key={emoji} className={`reaction ${mine ? 'mine' : ''}`} title={names} onClick={() => onReact(emoji)}>
            {emoji}{authors.length > 1 && <span>{authors.length}</span>}
          </button>
        );
      })}
    </div>
  );
}

// The emojis to react with, over the bubble.
function ReactionPicker({ current, onPick }: { current?: string; onPick: (emoji: string) => void }) {
  return (
    <div className="reaction-picker" role="menu" onClick={e => e.stopPropagation()}>
      {REACTIONS.map(emoji => (
        <button key={emoji} role="menuitem" className={current === emoji ? 'active' : ''} onClick={() => onPick(emoji)}>{emoji}</button>
      ))}
    </div>
  );
}

// One message of the chat. Renders again only when it, or the search,
// changes.
const FriendMessage = memo(function FriendMessage({ m, myId, friendId, friendName, cont, last, highlight, current, onShown, onReply, onReact }: {
  m: ChatMessage;
  myId: string;
  friendId: string;
  friendName: string;
  cont: boolean;
  last: boolean;
  highlight?: string;
  // The search match shown.
  current: boolean;
  onShown: () => void;
  onReply: (m: ChatMessage) => void;
  onReact: (m: ChatMessage, emoji: string | null) => void;
}) {
  const [picking, setPicking] = useState(false);
  const close = useCallback(() => setPicking(false), []);
  useDismiss(picking, close);
  // Mobile: a long press shows the actions and the reactions.
  const press = useLongPress(() => setPicking(true));
  const mine = m.from === myId;
  const ours = m.reactions?.[myId];
  const react = (emoji: string) => {
    setPicking(false);
    onReact(m, emoji);
  };
  const quote = m.reply && (
    <button
      className="bubble-quote list-button"
      title="Voir le message d’origine"
      onClick={() => jumpTo(m.reply!.id)}
    >
      <span className="quote-author">{m.reply.from === myId ? 'Vous' : friendName}</span>
      <span className="quote-text">{m.reply.text}</span>
    </button>
  );

  let bubble;
  if (m.file) {
    bubble = <ChatImage friendId={friendId} message={m} mine={mine} onShown={onShown} quote={quote} />;
  } else {
    const pending = mine && m.status === 'pending';
    bubble = (
      <div className={`bubble ${mine ? 'mine' : 'theirs'}`}>
        {quote}
        <RichText text={m.text} highlight={highlight} />
        {(last || pending) && (
          <div className="bubble-time">
            {formatTime(m.ts)}
            {mine && (pending
              ? <PendingMark />
              : m.status === 'read' ? <span title="Lu"> · Vu</span> : <span title="Livré"> ✓✓</span>)}
          </div>
        )}
      </div>
    );
  }

  return (
    <div
      id={`msg-${m.id}`}
      className={`bubble-row ${mine ? 'mine' : ''} ${cont ? 'cont' : ''} ${current ? 'current' : ''}`}
      {...press}
      onContextMenu={e => {
        // Right click or long press: the reactions.
        e.preventDefault();
        e.stopPropagation();
        setPicking(true);
      }}
    >
      <div className="bubble-stack">
        {bubble}
        {m.reactions && <Reactions reactions={m.reactions} myId={myId} friendName={friendName} onReact={react} />}
      </div>
      <BubbleActions open={picking}>
        <button
          className="icon-btn small react-toggle"
          title="Réagir"
          onClick={e => {
            e.stopPropagation();
            setPicking(!picking);
          }}
        >
          <Icon name="smile" size={14} />
        </button>
        <button className="icon-btn small" title="Répondre" onClick={() => onReply(m)}><Icon name="reply" size={14} /></button>
        {/* The text cannot be selected by touch: copied whole. */}
        {isMobile && !m.file && (
          <button className="icon-btn small" title="Copier" onClick={() => navigator.clipboard.writeText(m.text)}><Icon name="copy" size={14} /></button>
        )}
        {picking && <ReactionPicker current={ours} onPick={react} />}
      </BubbleActions>
    </div>
  );
});

// The chat laid out: days and runs of messages.
const FriendChatRows = memo(function FriendChatRows(props: {
  messages: ChatMessage[];
  myId: string;
  friendId: string;
  friendName: string;
  highlight?: string;
  currentId?: string;
  onShown: () => void;
  onReply: (m: ChatMessage) => void;
  onReact: (m: ChatMessage, emoji: string | null) => void;
}) {
  const { messages, highlight, currentId, ...rest } = props;
  return chatRows(messages, {
    ts: m => m.ts,
    sender: m => m.from,
    key: m => m.id,
    render: (m, { cont, last }) => {
      // Only the bubbles holding the searched text render again with it.
      const found = !!highlight && !m.file && m.text.toLowerCase().includes(highlight.toLowerCase());
      return (
        <FriendMessage
          key={m.id}
          m={m}
          cont={cont}
          last={last}
          highlight={found ? highlight : undefined}
          current={m.id === currentId}
          {...rest}
        />
      );
    },
  });
});

export function FriendChatView({ friend, myId, messages, typing, notices, offers, strip, dropping, onBack, onSent, onReactions, onToast }: {
  friend: Friend;
  myId: string;
  messages: ChatMessage[];
  // The friend is typing.
  typing: boolean;
  // Banners of other chats (offers, messages).
  notices: ReactNode[];
  // This friend's file offers, at the end of the chat.
  offers: ReactNode[];
  // The running transfers.
  strip: ReactNode;
  // Files are dragged over the window.
  dropping: boolean;
  onBack: () => void;
  onSent: (message: ChatMessage) => void;
  onReactions: (id: string, reactions: Record<string, string>) => void;
  onToast: (text: string) => void;
}) {
  const [input, setInput] = useState('');
  const [error, setError] = useState('');
  const [reply, setReply] = useState<ChatMessage | null>(null);
  // null: no search; the match shown, counted from the most recent.
  const [query, setQuery] = useState<string | null>(null);
  const [back, setBack] = useState(0);
  const [attachMenu, setAttachMenu] = useState<{ left: number; bottom: number } | null>(null);
  const closeAttach = useCallback(() => setAttachMenu(null), []);
  useDismiss(attachMenu !== null, closeAttach);
  const endRef = useRef<HTMLDivElement>(null);
  const inputRef = useRef<HTMLTextAreaElement>(null);
  const sendingRef = useRef(false);
  const typingAt = useRef(0);
  const errorTimer = useRef<ReturnType<typeof setTimeout>>(undefined);
  const friendId = friend.id;
  // False once the backend said it cannot send folders (Android).
  const folders = useFoldersSupported();

  useEffect(() => () => clearTimeout(errorTimer.current), []);

  const flash = (text: string) => {
    setError(text);
    clearTimeout(errorTimer.current);
    errorTimer.current = setTimeout(() => setError(''), 3000);
  };

  const search = query?.trim().toLowerCase() ?? '';
  const matches = useMemo(
    () => (search ? messages.filter(m => !m.file && m.text.toLowerCase().includes(search)).map(m => m.id) : []),
    [messages, search],
  );
  const index = matches.length > 0 ? Math.max(0, matches.length - 1 - Math.min(back, matches.length - 1)) : -1;
  const currentId = index >= 0 ? matches[index] : undefined;

  useEffect(() => {
    if (currentId) jumpTo(currentId);
  }, [currentId]);

  // To the end on a new message or offer (not on a status or a reaction),
  // unless searching.
  const lastId = messages.at(-1)?.id;
  useEffect(() => {
    if (query === null) endRef.current?.scrollIntoView({ behavior: 'smooth' });
    // The search closing does not scroll.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [lastId, offers.length]);

  const scrollToEnd = useCallback(() => endRef.current?.scrollIntoView(), []);

  const startReply = useCallback((m: ChatMessage) => {
    setReply(m);
    inputRef.current?.focus();
  }, []);

  const react = useCallback(async (m: ChatMessage, emoji: string | null) => {
    const res = await api.reactMessage(friendId, m.id, emoji);
    if ('reactions' in res) onReactions(m.id, res.reactions);
    else onToast('Réaction impossible sur ce message.');
  }, [friendId, onReactions, onToast]);

  const send = async () => {
    const text = input.trim();
    // Enter pressed twice before the first send returns: send once.
    if (!text || sendingRef.current) return;
    sendingRef.current = true;
    const answering = reply;
    setInput('');
    setReply(null);
    try {
      const entry = await api.sendMessage(friendId, text, answering?.id);
      if ('error' in entry) {
        setInput(text);
        setReply(answering);
        flash(entry.error === 'too_long' ? 'Message trop long (5000 caractères max).' : 'Message non envoyé.');
        return;
      }
      onSent(entry);
    } finally {
      sendingRef.current = false;
    }
  };

  const type = (value: string) => {
    setInput(value);
    const now = Date.now();
    if (value.trim() && now - typingAt.current >= TYPING_EVERY) {
      typingAt.current = now;
      api.chatTyping(friendId);
    }
  };

  const attachFiles = async () => {
    const count = await api.selectAndSendFile(friendId);
    if (count > 0) onToast(`${plural(count, 'fichier envoyé', 'fichiers envoyés')} à ${friend.name}`);
  };

  const attachFolder = async () => {
    const res = await api.sendFolder(friendId);
    if ('error' in res) onToast(folderSendError(res.error));
    else if (res.sent > 0) onToast(`Dossier envoyé à ${friend.name} (${plural(res.sent, 'fichier', 'fichiers')}) : suivez l’envoi dans l’onglet Transferts.`);
  };

  const replyName = reply ? (reply.from === myId ? 'vous-même' : friend.name) : '';

  return (
    <div className={`app ${dropping ? 'dropping' : ''}`}>
      <div className="header">
        <div className="brand">
          <button className="icon-btn" title="Retour" onClick={onBack}><Icon name="back" /></button>
          <Avatar id={friend.id} name={friend.name} online={friend.online} small />
          <div className="header-title">
            <div className="title">{friend.name}</div>
            {typing
              ? <div className="subtitle typing">En train d’écrire…</div>
              : (
                <div className={`subtitle ${friend.online ? 'online' : ''}`}>
                  {friend.online ? 'En ligne · chiffré de bout en bout' : 'Hors ligne · vos messages partiront à sa reconnexion'}
                </div>
              )}
          </div>
        </div>
        <div className="header-actions">
          <button
            className={`icon-btn ${query !== null ? 'active' : ''}`}
            title="Rechercher dans la discussion"
            onClick={() => { setQuery(query === null ? '' : null); setBack(0); }}
          >
            <Icon name="search" />
          </button>
          <WindowButtons />
        </div>
      </div>
      {query !== null && (
        <ChatSearch
          query={query}
          onQuery={q => { setQuery(q); setBack(0); }}
          index={index}
          count={matches.length}
          onStep={step => setBack(b => Math.max(0, Math.min(matches.length - 1, b - step)))}
          onClose={() => setQuery(null)}
        />
      )}
      {strip}

      {notices.length > 0 && <div className="content notices">{notices}</div>}

      <div className="chat-messages">
        {messages.length === 0 && offers.length === 0 && (
          <div className="empty-state">
            <Icon name="chat" size={32} />
            {isMobile ? 'Aucun message. Dites bonjour !' : 'Aucun message. Dites bonjour, ou glissez un fichier ou un dossier ici !'}
          </div>
        )}
        <FriendChatRows
          messages={messages}
          myId={myId}
          friendId={friendId}
          friendName={friend.name}
          highlight={search || undefined}
          currentId={currentId}
          onShown={scrollToEnd}
          onReply={startReply}
          onReact={react}
        />
        {offers}
        <div ref={endRef} />
      </div>

      {reply && (
        <div className="reply-bar">
          <Icon name="reply" size={14} />
          <div className="item-body">
            <div className="quote-author">Réponse à {replyName}</div>
            <div className="item-sub ellipsis">{reply.file ? reply.file.name : reply.text}</div>
          </div>
          <button className="icon-btn small" title="Annuler la réponse" onClick={() => setReply(null)}><Icon name="close" size={14} /></button>
        </div>
      )}

      <ChatInput
        value={input}
        onChange={type}
        onSend={send}
        placeholder="Écrire un message…"
        maxLength={5000}
        error={error}
        inputRef={inputRef}
        onKey={e => {
          if (e.key !== 'Escape' || !reply) return false;
          e.stopPropagation();
          setReply(null);
          return true;
        }}
        before={
          <button
            className={`icon-btn attach ${attachMenu ? 'active' : ''}`}
            title={folders ? 'Envoyer un fichier ou un dossier' : 'Envoyer un fichier'}
            onClick={e => {
              e.stopPropagation();
              // Files only: no menu to choose.
              if (!folders) {
                attachFiles();
                return;
              }
              const r = e.currentTarget.getBoundingClientRect();
              setAttachMenu(attachMenu ? null : { left: r.left, bottom: window.innerHeight - r.top + 4 });
            }}
          >
            <Icon name="paperclip" />
          </button>
        }
      />
      {attachMenu && (
        <div className="menu-popover" role="menu" style={{ left: attachMenu.left, bottom: attachMenu.bottom }}>
          <button role="menuitem" onClick={() => { setAttachMenu(null); attachFiles(); }}><Icon name="file" size={14} />Envoyer un fichier</button>
          <button role="menuitem" onClick={() => { setAttachMenu(null); attachFolder(); }}><Icon name="folder" size={14} />Envoyer un dossier</button>
        </div>
      )}
    </div>
  );
}
