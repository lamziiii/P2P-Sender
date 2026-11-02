import { useEffect, useState } from 'react';
import { api } from './api';
import type { DropTarget, Friend, NotifySettings } from './api';
import { avatarColor, formatDate, initial } from './format';
import { Icon } from './icons';
import { LocalSendDevices } from './LocalSendDevices';
import { fitOnScreen, useFoldersSupported } from './platform';

const ADD_FRIEND_ERRORS: Record<string, string> = {
  invalid_id: 'ID invalide : il doit faire 64 caractères (0-9, a-f).',
  self: 'C’est votre propre ID.',
};

// Beyond this many friends, a search field helps find one.
const SEARCH_FROM = 10;

export type Unread = Record<string, { count: number; last: string }>;

// A friend's avatar: their initial on their color, with a dot when online.
export function Avatar({ id, name, online, small }: { id: string; name: string; online?: boolean; small?: boolean }) {
  return (
    <div className={`avatar ${small ? 'small' : ''}`} style={{ background: avatarColor(id), color: '#fff' }}>
      {initial(name)}
      {online && <span className="presence" title="En ligne" />}
    </div>
  );
}

// My ID to give, and the form to add a friend with theirs.
function AddFriend({ myId, onAdded, onClose }: {
  myId: string;
  onAdded: (friends: Friend[]) => void;
  onClose?: () => void;
}) {
  const [id, setId] = useState('');
  const [name, setName] = useState('');
  const [error, setError] = useState('');
  const [copied, setCopied] = useState(false);

  const copy = () => {
    navigator.clipboard.writeText(myId);
    setCopied(true);
    setTimeout(() => setCopied(false), 2000);
  };

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
    onAdded(res.friends);
  };

  return (
    <div className="card stack">
      <div className="section-header-row">
        <div className="card-label" style={{ marginBottom: 0 }}>Mon ID</div>
        {onClose && <button className="icon-btn small" title="Fermer" onClick={onClose}><Icon name="close" size={14} /></button>}
      </div>
      <div className="id-box-inline">
        <div className="id-value">{myId}</div>
        <button className="btn btn-secondary btn-small" onClick={copy}><Icon name="copy" size={14} />{copied ? 'Copié !' : 'Copier'}</button>
      </div>
      <div className="setting-sub">Envoyez-le à votre ami, puis ajoutez le sien ci-dessous : vous devez vous ajouter tous les deux.</div>
      <div className="card-label" style={{ margin: '4px 0 0' }}>Ajouter un ami</div>
      <input className="input" placeholder="ID de l’ami (64 caractères)" value={id} onChange={e => { setId(e.target.value); setError(''); }} />
      <input className="input" placeholder="Nom (facultatif : son pseudo sinon)" value={name} onChange={e => setName(e.target.value)} onKeyDown={e => e.key === 'Enter' && add()} />
      {error && <div className="error-text">{error}</div>}
      <button className="btn" disabled={!id.trim()} onClick={add}><Icon name="plus" size={14} />Ajouter cet ami</button>
    </div>
  );
}

// The name we give a friend; empty brings back their nickname.
function RenameDialog({ friend, onDone }: { friend: Friend; onDone: (friends?: Friend[]) => void }) {
  const [name, setName] = useState(friend.name);
  const save = async () => onDone(await api.renameFriend(friend.id, name));

  return (
    <div className="modal-backdrop" onClick={() => onDone()}>
      <div className="modal stack" role="dialog" aria-modal="true" onClick={e => e.stopPropagation()}>
        <div className="modal-title">Renommer</div>
        <input
          className="input"
          autoFocus
          maxLength={40}
          value={name}
          onChange={e => setName(e.target.value)}
          onKeyDown={e => {
            if (e.key === 'Enter') save();
            if (e.key === 'Escape') onDone();
          }}
        />
        <div className="setting-sub">Laissez vide pour afficher son propre pseudo.</div>
        <div style={{ display: 'flex', gap: 8 }}>
          <button className="btn btn-secondary" style={{ flex: 1 }} onClick={() => onDone()}>Annuler</button>
          <button className="btn" style={{ flex: 1 }} onClick={save}>Enregistrer</button>
        </div>
      </div>
    </div>
  );
}

export function FriendsTab({ friends, unread, dropTarget, myId, onFriends, onOpenChat, onSendFile, onSendFolder, onRemove, onLocalSendSent, onOpenLocalSendChat, mutedFriends, onMuted }: {
  friends: Friend[];
  unread: Unread;
  // Where files dragged over the window would go.
  dropTarget: DropTarget | null;
  myId: string;
  onFriends: (friends: Friend[]) => void;
  onOpenChat: (id: string) => void;
  onSendFile: (id: string) => void;
  onSendFolder: (id: string) => void;
  onRemove: (friend: Friend) => void;
  onLocalSendSent: () => void;
  // Friends whose messages give no notification, and to change it.
  mutedFriends: string[];
  onMuted: (settings: NotifySettings) => void;
  onOpenLocalSendChat: (fingerprint: string) => void;
}) {
  const [adding, setAdding] = useState(false);
  const [search, setSearch] = useState('');
  // The friend whose menu is open, and where (on the window: the list clips its rows).
  const [menu, setMenu] = useState<{ id: string; top: number; right: number } | null>(null);
  const [renaming, setRenaming] = useState<Friend | null>(null);
  // False once the backend said it cannot send folders (Android).
  const folders = useFoldersSupported();

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

  const unreadOf = (f: Friend) => unread[f.id]?.count ?? 0;
  // Unread first, then the latest chats, then who is online.
  const sorted = [...friends].sort((a, b) =>
    (unreadOf(b) > 0 ? 1 : 0) - (unreadOf(a) > 0 ? 1 : 0)
    || b.lastTs - a.lastTs
    || Number(b.online) - Number(a.online)
    || a.name.localeCompare(b.name));
  const query = search.trim().toLowerCase();
  const shown = query ? sorted.filter(f => f.name.toLowerCase().includes(query)) : sorted;

  return (
    <>
      <div className="section-header section-header-row">
        <span>Amis{friends.length > 0 ? ` (${friends.length})` : ''}</span>
        {friends.length > 0 && (
          <button className={`icon-btn small ${adding ? 'active' : ''}`} title="Ajouter un ami · mon ID" onClick={() => setAdding(!adding)}>
            <Icon name="plus" size={14} />
          </button>
        )}
      </div>

      {(adding || friends.length === 0) && (
        <AddFriend
          myId={myId}
          onAdded={list => { onFriends(list); setAdding(false); }}
          onClose={friends.length > 0 ? () => setAdding(false) : undefined}
        />
      )}

      {friends.length > SEARCH_FROM && (
        <div className="search-box">
          <Icon name="search" size={14} />
          <input className="input" placeholder="Rechercher un ami" value={search} onChange={e => setSearch(e.target.value)} />
        </div>
      )}

      {shown.length > 0 && (
        <div className="list">
          {shown.map(f => {
            const count = unreadOf(f);
            const preview = f.legacy
              ? 'ID obsolète : demandez-lui son nouvel ID'
              : unread[f.id]?.last ?? (f.lastText ? `${f.lastMine ? 'Vous : ' : ''}${f.lastText}` : f.online ? 'En ligne' : 'Aucun message');
            return (
              <div
                key={f.id}
                role="button"
                tabIndex={0}
                className={`list-item list-row ${dropTarget?.kind === 'friend' && dropTarget.id === f.id ? 'drop-target' : ''}`}
                data-drop-kind={f.legacy ? undefined : 'friend'}
                data-drop-id={f.id}
                data-drop-name={f.name}
                onClick={() => !f.legacy && onOpenChat(f.id)}
                onKeyDown={e => e.key === 'Enter' && !f.legacy && onOpenChat(f.id)}
              >
                <Avatar id={f.id} name={f.name} online={f.online} />
                <div className="item-body">
                  <div className="row-top">
                    <span className={`item-title ${count > 0 ? 'unread' : ''}`}>
                      {f.name}
                      {mutedFriends.includes(f.id) && <span className="muted-mark" title="Notifications désactivées"><Icon name="bellOff" size={12} /></span>}
                    </span>
                    {f.lastTs > 0 && <span className="item-time">{formatDate(f.lastTs)}</span>}
                  </div>
                  <div className="row-top">
                    <span className={`item-sub ellipsis ${f.legacy ? 'danger' : ''} ${count > 0 ? 'unread' : ''}`}>{preview}</span>
                    {count > 0 && <span className="badge" title="Messages non lus">{count}</span>}
                  </div>
                </div>
                <div className="item-actions" onClick={e => e.stopPropagation()}>
                  {!f.legacy && (
                    <button className="icon-btn" title="Envoyer un fichier" onClick={() => onSendFile(f.id)}><Icon name="fileUp" /></button>
                  )}
                  <div className="menu-anchor">
                    <button
                      className="icon-btn"
                      title="Plus d’options"
                      onClick={e => {
                        e.stopPropagation();
                        const r = e.currentTarget.getBoundingClientRect();
                        setMenu(menu?.id === f.id ? null : { id: f.id, top: r.bottom + 4, right: window.innerWidth - r.right });
                      }}
                    >
                      <Icon name="more" />
                    </button>
                    {menu?.id === f.id && (
                      <div className="menu-popover" role="menu" ref={fitOnScreen} style={{ top: menu.top, right: menu.right }}>
                        {!f.legacy && folders && (
                          <button role="menuitem" onClick={() => { setMenu(null); onSendFolder(f.id); }}><Icon name="folder" size={14} />Envoyer un dossier</button>
                        )}
                        {!f.legacy && (
                          <button
                            role="menuitemcheckbox"
                            aria-checked={f.autoAccept}
                            title="Ses fichiers et dossiers arrivent sans vous demander. Les programmes demandent toujours votre accord."
                            onClick={async () => { setMenu(null); onFriends(await api.setFriendAutoAccept(f.id, !f.autoAccept)); }}
                          >
                            <span className="menu-check">{f.autoAccept && <Icon name="check" size={14} />}</span>
                            <span>
                              Accepter ses fichiers automatiquement
                              <span className="menu-hint">Les programmes demandent toujours.</span>
                            </span>
                          </button>
                        )}
                        {!f.legacy && (
                          <button
                            role="menuitemcheckbox"
                            aria-checked={mutedFriends.includes(f.id)}
                            onClick={async () => { setMenu(null); onMuted(await api.setChatMuted('friend', f.id, !mutedFriends.includes(f.id))); }}
                          >
                            <span className="menu-check">{mutedFriends.includes(f.id) && <Icon name="check" size={14} />}</span>
                            Désactiver les notifications
                          </button>
                        )}
                        <button role="menuitem" onClick={() => { setMenu(null); setRenaming(f); }}><Icon name="edit" size={14} />Renommer</button>
                        <button role="menuitem" className="danger" onClick={() => { setMenu(null); onRemove(f); }}><Icon name="trash" size={14} />Supprimer</button>
                      </div>
                    )}
                  </div>
                </div>
              </div>
            );
          })}
        </div>
      )}
      {query && shown.length === 0 && <div className="setting-sub" style={{ textAlign: 'center' }}>Aucun ami ne correspond.</div>}

      <LocalSendDevices
        dropTarget={dropTarget?.kind === 'localsend' ? dropTarget.id : null}
        onSent={onLocalSendSent}
        onOpenChat={onOpenLocalSendChat}
      />

      {renaming && (
        <RenameDialog
          friend={renaming}
          onDone={list => {
            setRenaming(null);
            if (list) onFriends(list);
          }}
        />
      )}
    </>
  );
}
