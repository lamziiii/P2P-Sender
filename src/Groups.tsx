import { memo, useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { api } from './api';
import type { Friend, GroupDetail, GroupFile, GroupMember, GroupMessage, GroupPin, GroupsOverview } from './api';
import { BubbleActions, ChatInput } from './chatParts';
import { chatRows } from './chatRows';
import { avatarColor, formatBytes, formatDate, formatTime, initial } from './format';
import { Icon } from './icons';
import { WindowButtons } from './PinButton';
import { isMobile, useLongPress } from './platform';
import { RichText } from './RichText';
import { transferStore, useTransfers } from './transferStore';
import { jumpTo, useDismiss } from './ui';

const MAX_MEMBERS = 32;
const FINAL = new Set(['completed', 'declined', 'cancelled', 'error']);
// Mention suggestions shown at once.
const MAX_SUGGESTIONS = 6;
// What mentions everyone (the backend's words).
const EVERYONE = ['tous', 'all'];

const GROUP_ERRORS: Record<string, string> = {
  no_name: 'Donnez un nom au groupe.',
  too_many: `Un groupe compte au plus ${MAX_MEMBERS} membres.`,
  not_member: 'Vous ne faites plus partie de ce groupe.',
  too_long: 'Message trop long (5000 caractères max).',
  syncing: 'Synchronisation avec les membres en cours, réessayez dans un instant.',
};

const PIN_ERRORS: Record<string, string> = {
  ...GROUP_ERRORS,
  too_many: 'Au plus 50 éléments épinglés : désépinglez-en un d’abord.',
  unknown: 'Cet élément n’existe plus.',
  not_sent: 'Ce message n’est pas encore parti chez les membres.',
};

/* ─── Friend picker ─── */

function FriendPicker({ friends, selected, onChange }: {
  friends: Friend[];
  selected: string[];
  onChange: (ids: string[]) => void;
}) {
  if (friends.length === 0) {
    return <div className="setting-sub">Aucun ami à ajouter.</div>;
  }
  const toggle = (id: string) =>
    onChange(selected.includes(id) ? selected.filter(x => x !== id) : [...selected, id]);
  return (
    <div className="list">
      {friends.map(f => (
        <label key={f.id} className="list-item check-row">
          <input type="checkbox" checked={selected.includes(f.id)} onChange={() => toggle(f.id)} />
          <div className="avatar small" style={{ background: avatarColor(f.id), color: '#fff' }}>{initial(f.name)}</div>
          <div className="item-body">
            <div className="item-title">{f.name}</div>
          </div>
        </label>
      ))}
    </div>
  );
}

/* ─── Groups tab ─── */

export function GroupsTab({ overview, friends, dropTarget, onOpen }: {
  overview: GroupsOverview;
  friends: Friend[];
  // The group files dragged over the window would go to.
  dropTarget: string | null;
  onOpen: (gid: string) => void;
}) {
  const [creating, setCreating] = useState(false);
  const [name, setName] = useState('');
  const [members, setMembers] = useState<string[]>([]);
  const [error, setError] = useState('');
  const available = friends.filter(f => !f.legacy);

  const respond = (gid: string, accept: boolean) => {
    setError('');
    api.respondToGroupInvite(gid, accept)
      .then(() => { if (accept) onOpen(gid); })
      .catch(() => setError(accept ? 'Impossible de rejoindre le groupe.' : 'Impossible de refuser l’invitation.'));
  };

  const create = async () => {
    const res = await api.createGroup(name, members);
    if ('error' in res) {
      setError(GROUP_ERRORS[res.error] ?? 'Impossible de créer le groupe.');
      return;
    }
    setCreating(false);
    setName('');
    setMembers([]);
    setError('');
    onOpen(res.id);
  };

  return (
    <>
      {overview.invites.map(inv => (
        <div key={inv.gid} className="offer-banner">
          <div className="file-icon"><Icon name="users" size={18} /></div>
          <div style={{ flex: 1, minWidth: 0 }}>
            <div className="offer-title">Invitation</div>
            <div className="offer-desc">
              <strong>{inv.fromName}</strong> vous invite dans <strong>« {inv.name || 'Groupe'} »</strong>
            </div>
            <div style={{ display: 'flex', gap: 8 }}>
              <button className="btn" style={{ flex: 1 }} onClick={() => respond(inv.gid, true)}>Rejoindre</button>
              <button className="btn btn-secondary" style={{ flex: 1 }} onClick={() => respond(inv.gid, false)}>Refuser</button>
            </div>
          </div>
        </div>
      ))}
      {error && !creating && <div className="error-text">{error}</div>}

      {overview.groups.length > 0 && <div className="section-header">Groupes ({overview.groups.length})</div>}
      {overview.groups.length > 0 && (
        <div className="list">
          {overview.groups.map(g => (
            <button
              key={g.id}
              className={`list-item list-button ${dropTarget === g.id ? 'drop-target' : ''}`}
              data-drop-kind={g.removed ? undefined : 'group'}
              data-drop-id={g.id}
              data-drop-name={g.name}
              onClick={() => onOpen(g.id)}
            >
              <div className="avatar group" style={{ background: avatarColor(g.id), color: '#fff' }}>{initial(g.name)}</div>
              <div className="item-body">
                <div className="transfer-header" style={{ marginBottom: 0 }}>
                  <span className="item-title title-with-icon">
                    <span className="ellipsis">{g.name}</span>
                    {g.muted && <span className="muted-icon" title="Notifications désactivées"><Icon name="bellOff" size={12} /></span>}
                  </span>
                  {g.lastTs > 0 && <span className="item-time">{formatDate(g.lastTs)}</span>}
                </div>
                <div className="transfer-header" style={{ marginBottom: 0 }}>
                  <span className={`item-sub ellipsis ${g.removed ? 'danger' : ''}`}>
                    {g.removed ? 'Vous avez été retiré'
                      : g.syncing ? 'Synchronisation…'
                        : g.lastMessage ?? `${g.members} membres · ${g.online} en ligne`}
                  </span>
                  <span className="badges">
                    {g.unreadMentions > 0 && <span className="badge" title="Vous êtes mentionné">@</span>}
                    {g.unread > 0 && <span className={`badge ${g.muted ? 'muted' : ''}`}>{g.unread}</span>}
                  </span>
                </div>
              </div>
            </button>
          ))}
        </div>
      )}
      {overview.groups.length === 0 && overview.invites.length === 0 && !creating && (
        <div className="empty-state">
          <Icon name="users" size={32} />
          Aucun groupe. Créez-en un avec vos amis.
        </div>
      )}

      {!creating ? (
        <button className="btn" style={{ width: '100%', marginTop: 6 }} disabled={available.length === 0} onClick={() => { setCreating(true); setError(''); }}>
          <Icon name="plus" size={14} />Créer un groupe
        </button>
      ) : (
        <div className="card stack">
          <div className="card-label" style={{ marginBottom: 0 }}>Nouveau groupe</div>
          <input className="input" placeholder="Nom du groupe" maxLength={60} value={name} onChange={e => { setName(e.target.value); setError(''); }} />
          <div className="setting-sub">Inviter des amis</div>
          <FriendPicker friends={available} selected={members} onChange={setMembers} />
          {error && <div className="error-text">{error}</div>}
          <div style={{ display: 'flex', gap: 8 }}>
            <button className="btn" style={{ flex: 1 }} disabled={!name.trim()} onClick={create}>Créer</button>
            <button className="btn btn-secondary" style={{ flex: 1 }} onClick={() => { setCreating(false); setError(''); }}>Annuler</button>
          </div>
        </div>
      )}
      {available.length === 0 && (
        <div className="setting-sub" style={{ textAlign: 'center' }}>Ajoutez d’abord des amis pour créer un groupe.</div>
      )}
    </>
  );
}

/* ─── Group chat ─── */

// One message of the group. Renders again only when it changes.
const GroupBubble = memo(function GroupBubble({ m, cont, last, mentions, onPin }: {
  m: GroupMessage;
  cont: boolean;
  last: boolean;
  mentions: string[];
  onPin: (target: string, pinned: boolean) => void;
}) {
  // Mobile: the actions, shown on hover elsewhere, open on a long press.
  const [open, setOpen] = useState(false);
  const close = useCallback(() => setOpen(false), []);
  useDismiss(open, close);
  const press = useLongPress(() => setOpen(true));
  return (
    <div id={`msg-${m.id}`} className={`bubble-row ${m.mine ? 'mine' : ''} ${cont ? 'cont' : ''}`} {...press}>
      <div className="bubble-stack">
        <div className={`bubble ${m.mine ? 'mine' : 'theirs'} ${m.mentionsMe ? 'mentioned' : ''}`}>
          {!m.mine && !cont && <div className="bubble-author" style={{ color: avatarColor(m.author) }}>{m.authorName}</div>}
          <RichText text={m.text} mentions={mentions} />
          {(last || m.pinned || (m.mine && m.receivedBy < m.total)) && (
            <div className="bubble-time">
              {m.pinned && <span className="pinned-mark" title="Épinglé"><Icon name="pin" size={10} /></span>}
              {formatTime(m.ts)}
              {m.mine && m.total > 0 && (
                <span title="Membres qui ont reçu le message">
                  {m.receivedBy >= m.total ? ' · ✓✓ Reçu par tous' : ` · Reçu par ${m.receivedBy}/${m.total}`}
                </span>
              )}
            </div>
          )}
        </div>
      </div>
      <BubbleActions open={open}>
        <button className="icon-btn small" title={m.pinned ? 'Désépingler' : 'Épingler'} onClick={() => onPin(m.id, m.pinned)}>
          <Icon name="pin" size={14} />
        </button>
        {/* The text cannot be selected by touch: copied whole. */}
        {isMobile && (
          <button className="icon-btn small" title="Copier" onClick={() => navigator.clipboard.writeText(m.text)}><Icon name="copy" size={14} /></button>
        )}
      </BubbleActions>
    </div>
  );
});

// The latest pin over the chat; a click shows them all.
function PinBar({ pins, onOpen, onUnpin }: {
  pins: GroupPin[];
  onOpen: (pin: GroupPin) => void;
  onUnpin: (target: string) => void;
}) {
  const [open, setOpen] = useState(false);
  const [latest] = pins;
  const what = (p: GroupPin) => (p.kind === 'file' ? `Fichier « ${p.text} »` : p.text);
  return (
    <div className="pin-bar">
      <button className="pin-head list-button" aria-expanded={open} onClick={() => setOpen(!open)}>
        <Icon name="pin" size={14} />
        <div className="item-body">
          <div className="pin-title">{pins.length > 1 ? `${pins.length} éléments épinglés` : 'Épinglé'}</div>
          <div className="item-sub ellipsis">{latest.authorName} : {what(latest)}</div>
        </div>
        <Icon name={open ? 'chevronUp' : 'chevronDown'} size={14} />
      </button>
      {open && (
        <div className="pin-list">
          {pins.map(p => (
            <div key={p.target} className="pin-item">
              <button className="list-button item-body" title={p.kind === 'file' ? 'Voir dans les fichiers' : 'Voir le message'} onClick={() => { setOpen(false); onOpen(p); }}>
                <div className="ellipsis">{what(p)}</div>
                <div className="item-sub ellipsis">{p.authorName} · {formatDate(p.ts)} · épinglé par {p.pinnedByName}</div>
              </button>
              <button className="icon-btn small" title="Désépingler" onClick={() => onUnpin(p.target)}><Icon name="close" size={14} /></button>
            </div>
          ))}
        </div>
      )}
    </div>
  );
}

type Suggestion = { id: string; name: string; handle: string };

// The members matching what follows "@" (and everyone), by name or handle.
const suggestions = (members: GroupMember[], query: string): Suggestion[] => {
  const q = query.toLowerCase();
  const all = [
    ...members.filter(m => !m.me).map(m => ({ id: m.id, name: m.name, handle: m.handle })),
    { id: '', name: 'Tout le monde', handle: 'tous' },
  ];
  return all
    .filter(s => s.handle.toLowerCase().startsWith(q) || s.name.toLowerCase().includes(q))
    .slice(0, MAX_SUGGESTIONS);
};

/* ─── Group view ─── */

type Pane = 'chat' | 'files' | 'settings';

export function GroupView({ gid, friends, notice, dropping, onChange, onBack }: {
  gid: string;
  friends: Friend[];
  // Shown above the group (incoming file offers, unread messages).
  notice?: React.ReactNode[];
  // Files are dragged over the window: they go to the shared files.
  dropping?: boolean;
  // The group list may have changed (read, left): reload it.
  onChange?: () => void;
  onBack: () => void;
}) {
  const [pane, setPane] = useState<Pane>('chat');
  const [detail, setDetail] = useState<GroupDetail | null>(null);
  const [messages, setMessages] = useState<GroupMessage[]>([]);
  const [files, setFiles] = useState<GroupFile[]>([]);
  const [input, setInput] = useState('');
  const [error, setError] = useState('');
  const [adding, setAdding] = useState(false);
  // "@" typed: where, what follows it, and the suggestion selected.
  const [mention, setMention] = useState<{ start: number; query: string; index: number } | null>(null);
  const endRef = useRef<HTMLDivElement>(null);
  const inputRef = useRef<HTMLTextAreaElement>(null);
  const paneRef = useRef<Pane>('chat');
  const refreshId = useRef(0);
  const sendingRef = useRef(false);
  const flashTimer = useRef<ReturnType<typeof setTimeout>>(undefined);
  // Transfers of this group's files seen finished: the list is reloaded once each.
  const finished = useRef(new Set<string>());
  const onChangeRef = useRef(onChange);
  useEffect(() => { onChangeRef.current = onChange; });

  useEffect(() => () => clearTimeout(flashTimer.current), []);

  const markRead = useCallback(() => {
    api.markGroupRead(gid).then(() => onChangeRef.current?.(), () => {});
  }, [gid]);

  const refresh = useCallback(async () => {
    // Updates come in bursts: only the latest answer is applied.
    const id = ++refreshId.current;
    const [d, m, f] = await Promise.all([api.getGroup(gid), api.getGroupMessages(gid), api.getGroupFiles(gid)]);
    if (id !== refreshId.current) return;
    setDetail(d);
    setMessages(m);
    setFiles(f);
    if (paneRef.current === 'chat') markRead();
  }, [gid, markRead]);

  useEffect(() => {
    // Initial load, then refresh on every change of this group.
    refresh();
    const unsubscribers = [
      api.onGroupUpdate(({ gid: changed }) => { if (changed === gid) refresh(); }),
      api.onFriendStatus(() => refresh()),
    ];
    return () => unsubscribers.forEach(u => u());
  }, [gid, refresh]);

  useEffect(() => {
    paneRef.current = pane;
    if (pane === 'chat') markRead();
  }, [pane, markRead]);

  useEffect(() => {
    // A download ended: the file is now local (or can be asked again).
    // Followed without rendering the view on each progress update.
    const check = () => {
      const transfers = transferStore.get();
      const ended = files.some(f => {
        const t = f.transferId ? transfers.find(x => x.id === f.transferId) : undefined;
        if (!t || !FINAL.has(t.status) || finished.current.has(t.id)) return false;
        finished.current.add(t.id);
        return true;
      });
      if (ended) refresh();
    };
    check();
    return transferStore.subscribe(check);
  }, [files, refresh]);

  // To the end on a new message, not when one is pinned.
  const lastId = messages.at(-1)?.id;
  useEffect(() => {
    if (pane === 'chat') endRef.current?.scrollIntoView({ behavior: 'smooth' });
  }, [lastId, pane]);

  const flash = useCallback((text: string) => {
    setError(text);
    clearTimeout(flashTimer.current);
    flashTimer.current = setTimeout(() => setError(''), 3000);
  }, []);

  const send = async () => {
    const text = input.trim();
    // Enter pressed twice before the first send returns: send once.
    if (!text || sendingRef.current) return;
    sendingRef.current = true;
    setInput('');
    setMention(null);
    try {
      const res = await api.sendGroupMessage(gid, text);
      if ('error' in res) {
        setInput(text);
        flash(GROUP_ERRORS[res.error] ?? 'Message non envoyé.');
      }
    } finally {
      sendingRef.current = false;
    }
  };

  const addFiles = async () => {
    setAdding(true);
    try {
      await api.addGroupFiles(gid);
    } catch {
      flash('Impossible d’ajouter ces fichiers.');
    } finally {
      setAdding(false);
    }
  };

  const download = async (fileId: string) => {
    const res = await api.downloadGroupFile(gid, fileId);
    if ('error' in res && res.error !== 'local') flash(GROUP_ERRORS[res.error] ?? res.error);
    refresh();
  };

  const togglePin = useCallback(async (target: string, pinned: boolean) => {
    const res = await (pinned ? api.unpinGroupItem(gid, target) : api.pinGroupItem(gid, target));
    if ('error' in res) flash(PIN_ERRORS[res.error] ?? 'Impossible d’épingler.');
    refresh();
  }, [gid, flash, refresh]);

  const openPin = (pin: GroupPin) => {
    // The file list or the chat must be shown before the item is found.
    setPane(pin.kind === 'file' ? 'files' : 'chat');
    requestAnimationFrame(() => {
      if (!jumpTo(pin.target) && pin.kind === 'message') flash('Ce message n’est plus dans l’historique.');
    });
  };

  const members = useMemo(() => detail?.members ?? [], [detail]);
  // Every member's handle can be mentioned, and everyone.
  const handles = useMemo(() => [...members.map(m => m.handle), ...EVERYONE], [members]);

  // Not laid out again on each key typed.
  const rows = useMemo(() => chatRows(messages, {
    ts: m => m.ts,
    sender: m => m.author,
    key: m => m.id,
    render: (m, { cont, last }) => <GroupBubble key={m.id} m={m} cont={cont} last={last} mentions={handles} onPin={togglePin} />,
  }), [messages, handles, togglePin]);

  // What follows an "@" at the start of a word, up to the caret.
  const type = (value: string) => {
    setInput(value);
    const caret = inputRef.current?.selectionStart ?? value.length;
    const before = value.slice(0, caret);
    const at = before.lastIndexOf('@');
    const query = before.slice(at + 1);
    if (at >= 0 && (at === 0 || /\s/.test(before[at - 1])) && !query.includes('\n') && query.length <= 40) {
      setMention(prev => ({ start: at, query, index: prev?.start === at ? prev.index : 0 }));
    } else setMention(null);
  };

  const shown = mention ? suggestions(members, mention.query) : [];
  const selected = mention ? Math.min(mention.index, shown.length - 1) : 0;

  const pick = (s: Suggestion) => {
    if (!mention) return;
    const el = inputRef.current;
    const caret = el?.selectionStart ?? input.length;
    const insert = `@${s.handle} `;
    setInput(input.slice(0, mention.start) + insert + input.slice(caret));
    setMention(null);
    const at = mention.start + insert.length;
    requestAnimationFrame(() => {
      el?.focus();
      el?.setSelectionRange(at, at);
    });
  };

  const name = detail?.name ?? 'Groupe';
  const online = members.filter(m => m.online && !m.me).length;
  const inactive = !detail || detail.removed || detail.syncing;
  const pins = detail?.pins ?? [];

  return (
    <div className={`app ${dropping && !inactive ? 'dropping' : ''}`}>
      <div className="header">
        <div className="brand">
          <button className="icon-btn" title="Retour" onClick={onBack}><Icon name="back" /></button>
          <div className="avatar small group" style={{ background: avatarColor(gid), color: '#fff' }}>{initial(name)}</div>
          <div className="header-title">
            <div className="title title-with-icon">
              <span className="ellipsis">{name}</span>
              {detail?.muted && <span className="muted-icon" title="Notifications désactivées, sauf les mentions"><Icon name="bellOff" size={12} /></span>}
            </div>
            <div className={`subtitle ${online > 0 ? 'online' : ''}`}>
              {detail?.syncing ? 'Synchronisation…' : `${members.length} membres · ${online} en ligne`}
            </div>
          </div>
        </div>
        <div className="header-actions">
          <button className={`icon-btn ${pane === 'settings' ? 'active' : ''}`} title="Membres et réglages" onClick={() => setPane(pane === 'settings' ? 'chat' : 'settings')}><Icon name="settings" /></button>
          <WindowButtons />
        </div>
      </div>

      {pane !== 'settings' && (
        <div className="pivot">
          <button className={`pivot-btn ${pane === 'chat' ? 'active' : ''}`} onClick={() => setPane('chat')}>Discussion</button>
          <button className={`pivot-btn ${pane === 'files' ? 'active' : ''}`} onClick={() => setPane('files')}>
            Fichiers{files.length > 0 && <span className="badge muted">{files.length}</span>}
          </button>
        </div>
      )}

      {notice && notice.length > 0 && <div className="content notices">{notice}</div>}

      {detail?.removed && (
        <div className="group-banner danger">
          Vous avez été retiré de ce groupe.
          <button className="btn-link" onClick={() => { api.forgetGroup(gid).catch(() => {}).then(() => onChangeRef.current?.()); onBack(); }}>Supprimer de la liste</button>
        </div>
      )}
      {detail?.syncing && <div className="group-banner">Récupération du groupe auprès de vos amis…</div>}

      {pane === 'chat' && (
        <>
          {pins.length > 0 && <PinBar pins={pins} onOpen={openPin} onUnpin={target => togglePin(target, true)} />}
          <div className="chat-messages">
            {messages.length === 0 && (
              <div className="empty-state">
                <Icon name="chat" size={32} />
                Aucun message. Lancez la discussion !
              </div>
            )}
            {rows}
            <div ref={endRef} />
          </div>
          <ChatInput
            value={input}
            onChange={type}
            onSend={send}
            placeholder={inactive ? 'Discussion indisponible' : 'Écrire au groupe… (@ pour mentionner)'}
            disabled={inactive}
            maxLength={5000}
            error={error}
            inputRef={inputRef}
            onKey={e => {
              if (!mention || shown.length === 0) return false;
              if (e.key === 'ArrowDown' || e.key === 'ArrowUp') {
                const step = e.key === 'ArrowDown' ? 1 : -1;
                setMention({ ...mention, index: (selected + step + shown.length) % shown.length });
              } else if (e.key === 'Enter' || e.key === 'Tab') {
                pick(shown[selected]);
              } else if (e.key === 'Escape') {
                e.stopPropagation();
                setMention(null);
              } else return false;
              e.preventDefault();
              return true;
            }}
            popup={mention && shown.length > 0 && (
              <div className="mention-popup" role="listbox">
                {shown.map((s, i) => (
                  <button
                    key={s.id || s.handle}
                    role="option"
                    aria-selected={i === selected}
                    className={i === selected ? 'active' : ''}
                    // The field keeps the focus.
                    onMouseDown={e => e.preventDefault()}
                    onClick={() => pick(s)}
                  >
                    {s.id
                      ? <div className="avatar small" style={{ background: avatarColor(s.id), color: '#fff' }}>{initial(s.name)}</div>
                      : <div className="avatar small"><Icon name="users" size={14} /></div>}
                    <span className="ellipsis">{s.name}</span>
                    <span className="item-sub ellipsis">@{s.handle}</span>
                  </button>
                ))}
              </div>
            )}
          />
        </>
      )}

      {pane === 'files' && (
        <div className="content drop-area">
          {error && <div className="error-text">{error}</div>}
          <button className="btn" style={{ width: '100%' }} disabled={inactive || adding} onClick={addFiles}>
            <Icon name="fileUp" size={14} />{adding ? 'Ajout en cours…' : 'Ajouter des fichiers'}
          </button>
          <div className="setting-sub">
            Les fichiers restent chez celui qui les partage : chacun télécharge ce qu’il veut, depuis n’importe quel membre qui l’a.
            {!isMobile && ' Glissez ici des fichiers ou des dossiers pour les ajouter.'}
          </div>
          {files.length === 0 && (
            <div className="empty-state">
              <Icon name="folder" size={32} />
              Le dépôt est vide.
            </div>
          )}
          {files.length > 0 && <GroupFileList gid={gid} files={files} inactive={inactive} onDownload={download} onPin={togglePin} />}
        </div>
      )}

      {pane === 'settings' && detail && (
        <GroupSettings detail={detail} friends={friends} onDone={() => setPane('chat')} onLeft={onBack} onChanged={refresh} />
      )}
    </div>
  );
}

/* ─── Group files ─── */

// The repository's files, with their downloads: renders on transfer updates,
// not the rest of the view.
function GroupFileList({ gid, files, inactive, onDownload, onPin }: {
  gid: string;
  files: GroupFile[];
  inactive: boolean;
  onDownload: (fileId: string) => void;
  onPin: (target: string, pinned: boolean) => void;
}) {
  const transfers = useTransfers();
  return (
    <div className="list">
      {files.map(f => {
        const linked = f.transferId ? transfers.find(x => x.id === f.transferId) : undefined;
        // Only a running download is shown; an ended one leaves its status.
        const t = linked && !FINAL.has(linked.status) ? linked : undefined;
        const failed = !f.local && linked && linked.status !== 'completed' && FINAL.has(linked.status) ? linked : undefined;
        return (
          // The id lets a pin bring it into view.
          <div key={f.id} id={`msg-${f.id}`} className="list-item transfer-item">
            <div className="file-icon"><Icon name="file" size={18} /></div>
            <div className="item-body">
              <div className="item-title title-with-icon" title={f.dir ? `${f.dir}/${f.name}` : f.name}>
                <span className="ellipsis">{f.name}</span>
                {f.pinned && <span className="muted-icon" title="Épinglé"><Icon name="pin" size={12} /></span>}
              </div>
              {f.dir && <div className="item-sub ellipsis file-dir"><Icon name="folder" size={11} /> {f.dir}</div>}
              <div className="item-sub ellipsis">
                {formatBytes(f.size)} · {f.mine ? 'vous' : f.authorName} · {formatDate(f.ts)}
              </div>
              {t && (
                <>
                  <div className="progress-track" style={{ marginTop: 6 }}>
                    <div className="progress-fill" style={{ width: `${t.progress}%`, background: 'var(--accent)' }} />
                  </div>
                  <div className="item-sub">
                    {t.status === 'paused' ? (t.error ?? 'En attente…') : `${t.progress}% · ${formatBytes(t.bytes)} / ${formatBytes(t.fileSize)}`}
                  </div>
                </>
              )}
              {failed && (
                <div className="error-text" style={{ marginTop: 4 }}>
                  {failed.status === 'cancelled' ? 'Téléchargement annulé.' : failed.error ?? 'Échec du téléchargement.'}
                </div>
              )}
              <div style={{ display: 'flex', justifyContent: 'flex-end', gap: 6, marginTop: 8 }}>
                {f.local && (
                  <button className="btn btn-secondary btn-small" onClick={() => api.showGroupFile(gid, f.id)}><Icon name="folder" size={14} />Afficher</button>
                )}
                {!f.local && !t && (
                  <button className="btn btn-small" disabled={inactive} onClick={() => onDownload(f.id)}><Icon name="down" size={14} />Télécharger</button>
                )}
                {t && (
                  <button className="btn btn-secondary btn-small" onClick={() => api.cancelTransfer(t.id)}>Annuler</button>
                )}
                <button className={`icon-btn ${f.pinned ? 'active' : ''}`} title={f.pinned ? 'Désépingler' : 'Épingler'} disabled={inactive} onClick={() => onPin(f.id, f.pinned)}>
                  <Icon name="pin" />
                </button>
                {f.mine && (
                  <button className="icon-btn danger" title="Retirer du dépôt" onClick={() => api.deleteGroupFile(gid, f.id)}><Icon name="trash" /></button>
                )}
              </div>
            </div>
          </div>
        );
      })}
    </div>
  );
}

/* ─── Group settings ─── */

function GroupSettings({ detail, friends, onDone, onLeft, onChanged }: {
  detail: GroupDetail;
  friends: Friend[];
  onDone: () => void;
  onLeft: () => void;
  onChanged: () => void;
}) {
  const [name, setName] = useState(detail.name);
  const [adding, setAdding] = useState<string[]>([]);
  const [confirmLeave, setConfirmLeave] = useState(false);
  const [error, setError] = useState('');
  // Renaming or leaving failed.
  const [actionError, setActionError] = useState('');
  const gid = detail.id;
  const active = !detail.removed && !detail.syncing;
  const memberIds = new Set(detail.members.map(m => m.id));
  const candidates = friends.filter(f => !f.legacy && !memberIds.has(f.id));

  const addMembers = async () => {
    const res = await api.addGroupMembers(gid, adding);
    if ('error' in res) return setError(GROUP_ERRORS[res.error] ?? 'Impossible d’ajouter ces amis.');
    setAdding([]);
    setError('');
  };

  const rename = () => {
    setActionError('');
    api.renameGroup(gid, name).catch(() => setActionError('Impossible de renommer le groupe.'));
  };

  const leave = () => {
    setActionError('');
    api.leaveGroup(gid).then(onLeft, () => setActionError(active ? 'Impossible de quitter le groupe.' : 'Impossible de supprimer le groupe de la liste.'));
  };

  const setMuted = (muted: boolean) => {
    api.setGroupMuted(gid, muted).then(onChanged, () => setActionError('Impossible de changer les notifications.'));
  };

  return (
    <div className="content">
      <div className="page-title">
        <button className="icon-btn" title="Retour" onClick={onDone}><Icon name="back" /></button>
        <span>Membres et réglages</span>
      </div>
      {actionError && <div className="error-text">{actionError}</div>}

      {active && (
        <div className="card stack">
          <div className="card-label" style={{ marginBottom: 0 }}>Nom du groupe</div>
          <div style={{ display: 'flex', gap: 8 }}>
            <input className="input" maxLength={60} value={name} onChange={e => setName(e.target.value)} />
            <button className="btn btn-secondary btn-small" style={{ height: 34 }} disabled={!name.trim() || name.trim() === detail.name} onClick={rename}>Renommer</button>
          </div>
        </div>
      )}

      {!detail.removed && (
        <div className="card">
          <div className="setting-row">
            <div>
              <div className="setting-label">Désactiver les notifications</div>
              <div className="setting-sub">Seuls les messages qui mentionnent votre pseudo vous notifient.</div>
            </div>
            <label className="toggle">
              <input type="checkbox" checked={detail.muted} onChange={e => setMuted(e.target.checked)} />
              <span className="toggle-track" />
            </label>
          </div>
        </div>
      )}

      <div className="section-header">Membres ({detail.members.length}/{MAX_MEMBERS})</div>
      <div className="list">
        {detail.members.map(m => (
          <div key={m.id} className="list-item">
            <div className="avatar small" style={{ background: avatarColor(m.id), color: '#fff' }}>{initial(m.name)}</div>
            <div className="item-body">
              <div className="item-title">{m.name}{m.me && ' (vous)'}</div>
              <div className={`item-sub ${m.online ? 'online' : ''}`}>
                {m.me ? 'Vous' : m.online ? 'En ligne' : 'Hors ligne'}{!m.me && m.friend && ' · ami'}
              </div>
            </div>
            {active && m.removable && (
              <button className="btn btn-secondary btn-small" onClick={() => api.removeGroupMember(gid, m.id)}>Retirer</button>
            )}
          </div>
        ))}
      </div>

      {active && (
        <>
          <div className="section-header">Inviter des amis</div>
          <FriendPicker friends={candidates} selected={adding} onChange={setAdding} />
          {error && <div className="error-text">{error}</div>}
          {candidates.length > 0 && (
            <button className="btn" disabled={adding.length === 0} onClick={addMembers}><Icon name="plus" size={14} />Inviter</button>
          )}
        </>
      )}

      <div className="section-header">Quitter</div>
      {!confirmLeave ? (
        <button className="btn btn-danger" onClick={() => setConfirmLeave(true)}>
          {active ? 'Quitter le groupe' : 'Supprimer de la liste'}
        </button>
      ) : (
        <div className="card stack">
          <div className="setting-sub">
            {active ? 'Vous ne recevrez plus les messages ni les fichiers de ce groupe.' : 'Le groupe disparaîtra de votre liste.'}
          </div>
          <div style={{ display: 'flex', gap: 8 }}>
            <button className="btn btn-danger" style={{ flex: 1 }} onClick={leave}>Confirmer</button>
            <button className="btn btn-secondary" style={{ flex: 1 }} onClick={() => setConfirmLeave(false)}>Annuler</button>
          </div>
        </div>
      )}
    </div>
  );
}
