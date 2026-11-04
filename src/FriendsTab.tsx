import { useEffect, useState } from 'react';
import { api } from './api';
import type { DropTarget, Friend, InviteLink, NotifySettings } from './api';
import { avatarColor, formatDate, initial } from './format';
import { Icon } from './icons';
import { InviteFriend } from './Invite';
import { LocalSendDevices } from './LocalSendDevices';
import { fitOnScreen, useFoldersSupported } from './platform';

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

export function FriendsTab({ friends, unread, dropTarget, localNetwork, onFriends, onOpenInvite, onOpenChat, onSendFile, onSendFolder, onRemove, onLocalSendSent, onOpenLocalSendChat, mutedFriends, mutedLocalsend, onMuted }: {
  friends: Friend[];
  unread: Unread;
  // Where files dragged over the window would go.
  dropTarget: DropTarget | null;
  // Off in the settings: no LocalSend devices.
  localNetwork: boolean;
  onFriends: (friends: Friend[]) => void;
  // A link pasted: asked about like one that opened the app.
  onOpenInvite: (invite: InviteLink) => void;
  onOpenChat: (id: string) => void;
  onSendFile: (id: string) => void;
  onSendFolder: (id: string) => void;
  onRemove: (friend: Friend) => void;
  onLocalSendSent: () => void;
  // Friends and LocalSend devices whose messages give no notification, and to change it.
  mutedFriends: string[];
  mutedLocalsend: string[];
  onMuted: (settings: NotifySettings) => void;
  onOpenLocalSendChat: (fingerprint: string) => void;
}) {
  const [adding, setAdding] = useState(false);
  const [search, setSearch] = useState('');
  // The friend whose menu is open (sending or more options), and where (on
  // the window: the list clips its rows).
  const [menu, setMenu] = useState<{ id: string; kind: 'send' | 'more'; top: number; right: number } | null>(null);
  // A second click on the same button closes its menu.
  const openMenu = (id: string, kind: 'send' | 'more', button: HTMLElement) => {
    const r = button.getBoundingClientRect();
    setMenu(menu?.id === id && menu.kind === kind ? null : { id, kind, top: r.bottom + 4, right: window.innerWidth - r.right });
  };
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
          <button className={`icon-btn small ${adding ? 'active' : ''}`} title="Inviter un ami" onClick={() => setAdding(!adding)}>
            <Icon name="plus" size={14} />
          </button>
        )}
      </div>

      {(adding || friends.length === 0) && (
        <InviteFriend
          onOpenLink={invite => { setAdding(false); onOpenInvite(invite); }}
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
            let preview = f.legacy
              ? 'Ancien ID : demandez-lui un lien d’invitation'
              : unread[f.id]?.last ?? (f.lastText ? `${f.lastMine ? 'Vous : ' : ''}${f.lastText}` : f.online ? 'En ligne' : 'Aucun message');
            // Added from their link: they must be online once to let us in.
            // Short, to fit a phone's row.
            if (f.invite === 'pending') preview = 'En attente de sa connexion';
            if (f.invite === 'refused') preview = 'Lien refusé : demandez-en un nouveau';
            const warn = f.legacy || f.invite === 'refused';
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
                    <span className={`item-sub ellipsis ${warn ? 'danger' : ''} ${count > 0 ? 'unread' : ''}`}>{preview}</span>
                    {count > 0 && <span className="badge" title="Messages non lus">{count}</span>}
                  </div>
                </div>
                <div className="item-actions" onClick={e => e.stopPropagation()}>
                  {!f.legacy && (
                    <div className="menu-anchor">
                      <button
                        className={`icon-btn ${menu?.id === f.id && menu.kind === 'send' ? 'active' : ''}`}
                        title={folders ? 'Envoyer un fichier ou un dossier' : 'Envoyer un fichier'}
                        onClick={e => {
                          e.stopPropagation();
                          // Files only: no menu to choose.
                          if (!folders) {
                            setMenu(null);
                            onSendFile(f.id);
                            return;
                          }
                          openMenu(f.id, 'send', e.currentTarget);
                        }}
                      >
                        <Icon name="fileUp" />
                      </button>
                      {menu?.id === f.id && menu.kind === 'send' && (
                        <div className="menu-popover" role="menu" ref={fitOnScreen} style={{ top: menu.top, right: menu.right }}>
                          <button role="menuitem" onClick={() => { setMenu(null); onSendFile(f.id); }}><Icon name="file" size={14} />Fichier</button>
                          <button role="menuitem" onClick={() => { setMenu(null); onSendFolder(f.id); }}><Icon name="folder" size={14} />Dossier</button>
                        </div>
                      )}
                    </div>
                  )}
                  <div className="menu-anchor">
                    <button
                      className="icon-btn"
                      title="Plus d’options"
                      onClick={e => {
                        e.stopPropagation();
                        openMenu(f.id, 'more', e.currentTarget);
                      }}
                    >
                      <Icon name="more" />
                    </button>
                    {menu?.id === f.id && menu.kind === 'more' && (
                      <div className="menu-popover" role="menu" ref={fitOnScreen} style={{ top: menu.top, right: menu.right }}>
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

      {localNetwork && (
        <LocalSendDevices
          dropTarget={dropTarget?.kind === 'localsend' ? dropTarget.id : null}
          onSent={onLocalSendSent}
          onOpenChat={onOpenLocalSendChat}
          muted={mutedLocalsend}
          onMuted={onMuted}
        />
      )}

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
