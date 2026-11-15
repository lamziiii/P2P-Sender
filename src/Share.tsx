import { useEffect, useState } from 'react';
import type { ReactNode } from 'react';
import { api } from './api';
import type { DropTarget, Friend, GroupSummary, LocalSendDevice, Shared } from './api';
import { plural } from './format';
import { Avatar } from './FriendsTab';
import { tr } from './i18n';
import { Icon } from './icons';

// Beyond this many choices, a search field helps find one.
const SEARCH_FROM = 8;

// What is sent, in a few words.
function describe(shared: Shared): string {
  if (shared.text !== undefined && shared.items.length === 0) return tr('ce texte', 'this text');
  const [first] = shared.items;
  if (shared.items.length === 1) return tr(`« ${first.name} »`, `“${first.name}”`);
  const dirs = shared.items.filter(i => i.dir).length;
  if (dirs === 0) return plural(shared.items.length, tr('fichier', 'file'), tr('fichiers', 'files'));
  if (dirs === shared.items.length) return plural(dirs, tr('dossier', 'folder'), tr('dossiers', 'folders'));
  return plural(shared.items.length, tr('élément', 'item'), tr('éléments', 'items'));
}

// "Envoyer avec P2P Share" (or Android's share menu): where it goes, among
// the friends, the groups and the LocalSend devices nearby.
export function ShareDialog({ shared, friends, groups, onPick, onClose }: {
  shared: Shared;
  friends: Friend[];
  groups: GroupSummary[];
  onPick: (target: DropTarget) => void;
  onClose: () => void;
}) {
  const [devices, setDevices] = useState<LocalSendDevice[]>([]);
  const [query, setQuery] = useState('');

  useEffect(() => {
    const load = () => api.getLocalSendDevices().then(setDevices, () => {});
    load();
    api.scanLocalSend();
    const unsubscribe = api.onLocalSendDevices(load);
    const onKey = (e: KeyboardEvent) => e.key === 'Escape' && onClose();
    window.addEventListener('keydown', onKey);
    return () => {
      unsubscribe();
      window.removeEventListener('keydown', onKey);
    };
  }, [onClose]);

  // A group takes files (its shared files) or a message, once synced.
  const choices: { target: DropTarget; icon: ReactNode; sub: string }[] = [
    ...friends.filter(f => !f.legacy).map(f => ({
      target: { kind: 'friend' as const, id: f.id, name: f.name },
      icon: <Avatar id={f.id} name={f.name} online={f.online} />,
      sub: f.online ? tr('En ligne', 'Online') : tr('Hors ligne', 'Offline'),
    })),
    ...groups.filter(g => !g.removed && !g.syncing).map(g => ({
      target: { kind: 'group' as const, id: g.id, name: g.name || tr('Groupe', 'Group') },
      icon: <div className="avatar"><Icon name="users" size={16} /></div>,
      sub: shared.text !== undefined && shared.items.length === 0
        ? plural(g.members, tr('membre', 'member'), tr('membres', 'members'))
        : tr('Ajouté aux fichiers du groupe', 'Added to the group’s files'),
    })),
    ...devices.filter(d => !d.friend).map(d => ({
      target: { kind: 'localsend' as const, id: d.fingerprint, name: d.alias },
      icon: <div className="avatar"><Icon name={d.deviceType === 'mobile' ? 'phone' : 'monitor'} /></div>,
      sub: `LocalSend${d.deviceModel ? ` · ${d.deviceModel}` : ''}`,
    })),
  ];
  const q = query.trim().toLowerCase();
  const shown = q ? choices.filter(c => c.target.name.toLowerCase().includes(q)) : choices;

  return (
    <div className="modal-backdrop" onClick={onClose}>
      <div className="modal stack" role="dialog" aria-modal="true" onClick={e => e.stopPropagation()}>
        <div className="modal-title">{tr(`Envoyer ${describe(shared)} à…`, `Send ${describe(shared)} to…`)}</div>
        {shared.text !== undefined && shared.items.length === 0 && (
          <div className="share-text">{shared.text}</div>
        )}
        {choices.length > SEARCH_FROM && (
          <input className="input" autoFocus placeholder={tr('Rechercher', 'Search')} value={query} onChange={e => setQuery(e.target.value)} />
        )}
        {choices.length === 0
          ? <div className="modal-text">{tr('Ajoutez d’abord un ami, ou ouvrez LocalSend sur un appareil du même réseau.', 'Add a friend first, or open LocalSend on a device on the same network.')}</div>
          : (
            <div className="list share-list">
              {shown.map(c => (
                <button key={`${c.target.kind}-${c.target.id}`} className="list-item list-button" onClick={() => onPick(c.target)}>
                  {c.icon}
                  <div className="item-body">
                    <div className="item-title">{c.target.name}</div>
                    <div className="item-sub ellipsis">{c.sub}</div>
                  </div>
                </button>
              ))}
              {shown.length === 0 && <div className="list-item item-sub">{tr('Aucun résultat.', 'No results.')}</div>}
            </div>
          )}
        <button className="btn btn-secondary" onClick={onClose}>{tr('Annuler', 'Cancel')}</button>
      </div>
    </div>
  );
}
