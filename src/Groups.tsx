import { useCallback, useEffect, useRef, useState } from 'react';
import { api } from './api';
import type { Friend, GroupDetail, GroupFile, GroupMessage, GroupsOverview, Transfer } from './api';
import { formatBytes, formatDate, formatTime } from './format';
import { Icon } from './icons';

const MAX_MEMBERS = 32;

const GROUP_ERRORS: Record<string, string> = {
  no_name: 'Donnez un nom au groupe.',
  too_many: `Un groupe compte au plus ${MAX_MEMBERS} membres.`,
  not_member: 'Vous ne faites plus partie de ce groupe.',
  too_long: 'Message trop long (5000 caractères max).',
};

const initial = (name: string) => name.trim()[0]?.toUpperCase() ?? '?';

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
          <div className="avatar small">{initial(f.name)}</div>
          <div className="item-body">
            <div className="item-title">{f.name}</div>
          </div>
        </label>
      ))}
    </div>
  );
}

/* ─── Groups tab ─── */

export function GroupsTab({ overview, friends, onOpen }: {
  overview: GroupsOverview;
  friends: Friend[];
  onOpen: (gid: string) => void;
}) {
  const [creating, setCreating] = useState(false);
  const [name, setName] = useState('');
  const [members, setMembers] = useState<string[]>([]);
  const [error, setError] = useState('');
  const available = friends.filter(f => !f.legacy);

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
              <button className="btn" style={{ flex: 1 }} onClick={() => { api.respondToGroupInvite(inv.gid, true); onOpen(inv.gid); }}>Rejoindre</button>
              <button className="btn btn-secondary" style={{ flex: 1 }} onClick={() => api.respondToGroupInvite(inv.gid, false)}>Refuser</button>
            </div>
          </div>
        </div>
      ))}

      {overview.groups.length > 0 && <div className="section-header">Groupes ({overview.groups.length})</div>}
      {overview.groups.length > 0 && (
        <div className="list">
          {overview.groups.map(g => (
            <button key={g.id} className="list-item list-button" onClick={() => onOpen(g.id)}>
              <div className="avatar group">{initial(g.name)}</div>
              <div className="item-body">
                <div className="transfer-header" style={{ marginBottom: 0 }}>
                  <span className="item-title">{g.name}</span>
                  {g.lastTs > 0 && <span className="item-time">{formatDate(g.lastTs)}</span>}
                </div>
                <div className="transfer-header" style={{ marginBottom: 0 }}>
                  <span className={`item-sub ellipsis ${g.removed ? 'danger' : ''}`}>
                    {g.removed ? 'Vous avez été retiré'
                      : g.syncing ? 'Synchronisation…'
                        : g.lastMessage ?? `${g.members} membres · ${g.online} en ligne`}
                  </span>
                  {g.unread > 0 && <span className="badge">{g.unread}</span>}
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
        <button className="btn" style={{ width: '100%', marginTop: 6 }} disabled={available.length === 0} onClick={() => setCreating(true)}>
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

/* ─── Group view ─── */

type Pane = 'chat' | 'files' | 'settings';

export function GroupView({ gid, friends, transfers, onBack }: {
  gid: string;
  friends: Friend[];
  transfers: Transfer[];
  onBack: () => void;
}) {
  const [pane, setPane] = useState<Pane>('chat');
  const [detail, setDetail] = useState<GroupDetail | null>(null);
  const [messages, setMessages] = useState<GroupMessage[]>([]);
  const [files, setFiles] = useState<GroupFile[]>([]);
  const [input, setInput] = useState('');
  const [error, setError] = useState('');
  const [adding, setAdding] = useState(false);
  const endRef = useRef<HTMLDivElement>(null);
  const paneRef = useRef<Pane>('chat');

  const refresh = useCallback(async () => {
    const [d, m, f] = await Promise.all([api.getGroup(gid), api.getGroupMessages(gid), api.getGroupFiles(gid)]);
    setDetail(d);
    setMessages(m);
    setFiles(f);
    if (paneRef.current === 'chat') api.markGroupRead(gid);
  }, [gid]);

  useEffect(() => {
    // Initial load, then refresh on every change of this group.
    // eslint-disable-next-line react-hooks/set-state-in-effect
    refresh();
    const unsubscribers = [
      api.onGroupUpdate(({ gid: changed }) => { if (changed === gid) refresh(); }),
      api.onFriendStatus(() => refresh()),
    ];
    return () => unsubscribers.forEach(u => u());
  }, [gid, refresh]);

  useEffect(() => {
    paneRef.current = pane;
    if (pane === 'chat') api.markGroupRead(gid);
  }, [pane, gid]);

  useEffect(() => {
    if (pane === 'chat') endRef.current?.scrollIntoView({ behavior: 'smooth' });
  }, [messages, pane]);

  const flash = (text: string) => {
    setError(text);
    setTimeout(() => setError(''), 3000);
  };

  const send = async () => {
    const text = input.trim();
    if (!text) return;
    const res = await api.sendGroupMessage(gid, text);
    if ('error' in res) return flash(GROUP_ERRORS[res.error] ?? 'Message non envoyé.');
    setInput('');
  };

  const addFiles = async () => {
    setAdding(true);
    try {
      await api.addGroupFiles(gid);
    } finally {
      setAdding(false);
    }
  };

  const download = async (fileId: string) => {
    const res = await api.downloadGroupFile(gid, fileId);
    if ('error' in res && res.error !== 'local') flash(GROUP_ERRORS[res.error] ?? res.error);
    refresh();
  };

  const name = detail?.name ?? 'Groupe';
  const members = detail?.members ?? [];
  const online = members.filter(m => m.online && !m.me).length;
  const inactive = !detail || detail.removed || detail.syncing;

  return (
    <div className="app">
      <div className="header">
        <div className="brand">
          <button className="icon-btn" title="Retour" onClick={onBack}><Icon name="back" /></button>
          <div className="avatar small group">{initial(name)}</div>
          <div className="header-title">
            <div className="title">{name}</div>
            <div className={`subtitle ${online > 0 ? 'online' : ''}`}>
              {detail?.syncing ? 'Synchronisation…' : `${members.length} membres · ${online} en ligne`}
            </div>
          </div>
        </div>
        <div className="header-actions">
          <button className={`icon-btn ${pane === 'settings' ? 'active' : ''}`} title="Membres et réglages" onClick={() => setPane(pane === 'settings' ? 'chat' : 'settings')}><Icon name="settings" /></button>
          <button className="icon-btn" title="Masquer" onClick={() => api.hideWindow()}><Icon name="close" /></button>
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

      {detail?.removed && (
        <div className="group-banner danger">
          Vous avez été retiré de ce groupe.
          <button className="btn-link" onClick={() => { api.forgetGroup(gid); onBack(); }}>Supprimer de la liste</button>
        </div>
      )}
      {detail?.syncing && <div className="group-banner">Récupération du groupe auprès de vos amis…</div>}

      {pane === 'chat' && (
        <>
          <div className="chat-messages">
            {messages.length === 0 && (
              <div className="empty-state">
                <Icon name="chat" size={32} />
                Aucun message. Lancez la discussion !
              </div>
            )}
            {messages.map((m, i) => {
              const showAuthor = !m.mine && messages[i - 1]?.author !== m.author;
              return (
                <div key={m.id} className={`bubble-row ${m.mine ? 'mine' : ''}`}>
                  <div className={`bubble ${m.mine ? 'mine' : 'theirs'}`}>
                    {showAuthor && <div className="bubble-author">{m.authorName}</div>}
                    {m.text}
                    <div className="bubble-time">
                      {formatTime(m.ts)}
                      {m.mine && m.total > 0 && (
                        <span title="Membres qui ont reçu le message">
                          {m.receivedBy >= m.total ? ' · ✓✓ Reçu par tous' : ` · Reçu par ${m.receivedBy}/${m.total}`}
                        </span>
                      )}
                    </div>
                  </div>
                </div>
              );
            })}
            <div ref={endRef} />
          </div>
          <div className="chat-input-row">
            {error && <div className="chat-error">{error}</div>}
            <input
              className="input"
              placeholder={inactive ? 'Discussion indisponible' : 'Écrire au groupe…'}
              value={input}
              maxLength={5000}
              disabled={inactive}
              onChange={e => setInput(e.target.value)}
              onKeyDown={e => e.key === 'Enter' && send()}
              style={{ flex: 1 }}
            />
            <button className="btn btn-icon" title="Envoyer" disabled={inactive} onClick={send}><Icon name="send" /></button>
          </div>
        </>
      )}

      {pane === 'files' && (
        <div className="content">
          {error && <div className="error-text">{error}</div>}
          <button className="btn" style={{ width: '100%' }} disabled={inactive || adding} onClick={addFiles}>
            <Icon name="fileUp" size={14} />{adding ? 'Ajout en cours…' : 'Ajouter des fichiers'}
          </button>
          <div className="setting-sub">
            Les fichiers restent chez celui qui les partage : chacun télécharge ce qu’il veut, depuis n’importe quel membre qui l’a.
          </div>
          {files.length === 0 && (
            <div className="empty-state">
              <Icon name="folder" size={32} />
              Le dépôt est vide.
            </div>
          )}
          {files.length > 0 && (
            <div className="list">
              {files.map(f => {
                const t = f.transferId ? transfers.find(x => x.id === f.transferId) : undefined;
                return (
                  <div key={f.id} className="list-item transfer-item">
                    <div className="file-icon"><Icon name="file" size={18} /></div>
                    <div className="item-body">
                      <div className="item-title" title={f.name}>{f.name}</div>
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
                      <div style={{ display: 'flex', justifyContent: 'flex-end', gap: 6, marginTop: 8 }}>
                        {f.local && (
                          <button className="btn btn-secondary btn-small" onClick={() => api.showGroupFile(gid, f.id)}><Icon name="folder" size={14} />Afficher</button>
                        )}
                        {!f.local && !t && (
                          <button className="btn btn-small" disabled={inactive} onClick={() => download(f.id)}><Icon name="down" size={14} />Télécharger</button>
                        )}
                        {t && (
                          <button className="btn btn-secondary btn-small" onClick={() => api.cancelTransfer(t.id)}>Annuler</button>
                        )}
                        {f.mine && (
                          <button className="icon-btn danger" title="Retirer du dépôt" onClick={() => api.deleteGroupFile(gid, f.id)}><Icon name="trash" /></button>
                        )}
                      </div>
                    </div>
                  </div>
                );
              })}
            </div>
          )}
        </div>
      )}

      {pane === 'settings' && detail && (
        <GroupSettings detail={detail} friends={friends} onDone={() => setPane('chat')} onLeft={onBack} />
      )}
    </div>
  );
}

/* ─── Group settings ─── */

function GroupSettings({ detail, friends, onDone, onLeft }: {
  detail: GroupDetail;
  friends: Friend[];
  onDone: () => void;
  onLeft: () => void;
}) {
  const [name, setName] = useState(detail.name);
  const [adding, setAdding] = useState<string[]>([]);
  const [confirmLeave, setConfirmLeave] = useState(false);
  const [error, setError] = useState('');
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

  return (
    <div className="content">
      <div className="page-title">
        <button className="icon-btn" title="Retour" onClick={onDone}><Icon name="back" /></button>
        <span>Membres et réglages</span>
      </div>

      {active && (
        <div className="card stack">
          <div className="card-label" style={{ marginBottom: 0 }}>Nom du groupe</div>
          <div style={{ display: 'flex', gap: 8 }}>
            <input className="input" maxLength={60} value={name} onChange={e => setName(e.target.value)} />
            <button className="btn btn-secondary btn-small" style={{ height: 34 }} disabled={!name.trim() || name.trim() === detail.name} onClick={() => api.renameGroup(gid, name)}>Renommer</button>
          </div>
        </div>
      )}

      <div className="section-header">Membres ({detail.members.length}/{MAX_MEMBERS})</div>
      <div className="list">
        {detail.members.map(m => (
          <div key={m.id} className="list-item">
            <div className="avatar small">{initial(m.name)}</div>
            <div className="item-body">
              <div className="item-title">{m.name}{m.me && ' (vous)'}</div>
              <div className={`item-sub ${m.online ? 'online' : ''}`}>
                {m.me ? 'Vous' : m.online ? 'En ligne' : 'Hors ligne'}{!m.me && m.friend && ' · ami'}
              </div>
            </div>
            {active && !m.me && (
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
            <button className="btn btn-danger" style={{ flex: 1 }} onClick={() => { api.leaveGroup(gid); onLeft(); }}>Confirmer</button>
            <button className="btn btn-secondary" style={{ flex: 1 }} onClick={() => setConfirmLeave(false)}>Annuler</button>
          </div>
        </div>
      )}
    </div>
  );
}
