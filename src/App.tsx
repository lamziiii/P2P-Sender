import { useState, useEffect, useRef } from 'react';
import { api } from './api';
import type { ChatMessage, Config, Friend, GroupsOverview, OpenChat, Transfer } from './api';
import { formatBytes } from './format';
import { GroupsTab, GroupView } from './Groups';
import { Icon } from './icons';
import { RelaySettings } from './RelaySettings';

const ACTIVE = new Set(['sending', 'receiving']);
const FINAL = new Set(['completed', 'declined', 'cancelled', 'error']);

const ADD_FRIEND_ERRORS: Record<string, string> = {
  invalid_id: 'ID invalide : il doit faire 64 caractères (0-9, a-f).',
  self: 'C’est votre propre ID.',
  no_name: 'Indiquez un pseudo.',
};

const formatSpeed = (bps: number) => (bps > 0 ? `${formatBytes(bps)}/s` : '');

const formatEta = (t: Transfer) => {
  if (!ACTIVE.has(t.status) || t.speed <= 0) return '';
  const s = Math.round((t.fileSize - t.bytes) / t.speed);
  if (s < 60) return `${s} s`;
  if (s < 3600) return `${Math.floor(s / 60)} min ${s % 60} s`;
  return `${Math.floor(s / 3600)} h ${Math.floor((s % 3600) / 60)} min`;
};

const transferColor = (status: string) => {
  if (status === 'completed') return 'var(--success)';
  if (status === 'declined' || status === 'cancelled' || status === 'error') return 'var(--danger)';
  if (ACTIVE.has(status)) return 'var(--accent)';
  return 'var(--text-secondary)';
};

const transferLabel = (t: Transfer) => {
  switch (t.status) {
    case 'completed': return 'Terminé';
    case 'declined': return 'Refusé';
    case 'cancelled': return 'Annulé';
    case 'error': return 'Erreur';
    case 'connecting': return 'Connexion P2P…';
    case 'waiting_consent': return 'En attente d’acceptation…';
    case 'paused': return 'Reconnexion…';
    case 'finishing': return 'Finalisation…';
    default: return `${t.progress}%`;
  }
};

function App() {
  const [activeTab, setActiveTab] = useState('friends');
  const [friends, setFriends] = useState<Friend[]>([]);
  const [myId, setMyId] = useState('Chargement...');
  const [newFriendId, setNewFriendId] = useState('');
  const [newFriendName, setNewFriendName] = useState('');
  const [addError, setAddError] = useState('');
  const [transfers, setTransfers] = useState<Transfer[]>([]);
  const [config, setConfig] = useState<Partial<Config>>({});
  const [chatFriendId, setChatFriendId] = useState<string | null>(null);
  const [chatMessages, setChatMessages] = useState<ChatMessage[]>([]);
  const [chatInput, setChatInput] = useState('');
  const [chatError, setChatError] = useState('');
  const [overview, setOverview] = useState<GroupsOverview>({ groups: [], invites: [] });
  const [groupId, setGroupId] = useState<string | null>(null);
  const [nickname, setNickname] = useState('');
  const [theme, setTheme] = useState<'light' | 'dark'>(
    () => (localStorage.getItem('theme') as 'light' | 'dark') ?? 'dark'
  );
  const messagesEndRef = useRef<HTMLDivElement>(null);
  const chatFriendIdRef = useRef<string | null>(null);

  const chatFriend = friends.find(f => f.id === chatFriendId) ?? null;
  const offers = transfers.filter(t => t.dir === 'in' && t.status === 'pending');
  const transferList = transfers.filter(t => t.status !== 'pending');
  const groupBadge = overview.invites.length + overview.groups.reduce((n, g) => n + g.unread, 0);
  const runningCount = transferList.filter(t => !FINAL.has(t.status)).length;
  const friendName = (id: string) => friends.find(f => f.id === id)?.name;

  useEffect(() => {
    document.documentElement.setAttribute('data-theme', theme === 'dark' ? 'dark' : '');
    localStorage.setItem('theme', theme);
  }, [theme]);

  const openChatWith = async (friendId: string) => {
    chatFriendIdRef.current = friendId;
    setChatFriendId(friendId);
    setChatMessages(await api.getMessages(friendId));
    setActiveTab('chat');
  };

  const openGroup = (gid: string) => {
    chatFriendIdRef.current = null;
    setGroupId(gid);
    setActiveTab('group');
  };

  const openFromOutside = (target: OpenChat | null) => {
    if (target?.kind === 'group') openGroup(target.id);
    else if (target) openChatWith(target.id);
  };

  useEffect(() => {
    api.getMyId().then(setMyId);
    api.getFriends().then(setFriends);
    api.getConfig().then(c => { setConfig(c); setNickname(c.nickname); });
    const loadGroups = () => api.getGroups().then(setOverview);
    loadGroups();
    api.getTransfers().then(list => setTransfers(prev => {
      const known = new Set(prev.map(t => t.id));
      return [...prev, ...list.filter(t => !known.has(t.id))];
    }));

    const unsubscribers = [
      api.onTransferUpdate(data => {
        setTransfers(prev => {
          const idx = prev.findIndex(t => t.id === data.id);
          if (idx === -1) return [data, ...prev];
          const next = [...prev];
          next[idx] = data;
          return next;
        });
      }),
      api.onSwitchTab(tab => setActiveTab(tab)),
      api.onNewMessage(({ friendId, message }) => {
        if (chatFriendIdRef.current === friendId) setChatMessages(prev => [...prev, message]);
      }),
      api.onMessageStatus(({ friendId, id, status }) => {
        if (chatFriendIdRef.current !== friendId) return;
        setChatMessages(prev => prev.map(m => (m.id === id ? { ...m, status } : m)));
      }),
      api.onOpenChat(openFromOutside),
      api.onGroupUpdate(loadGroups),
      api.onFriendStatus(({ friendId, online }) => {
        setFriends(prev => prev.map(f => (f.id === friendId ? { ...f, online } : f)));
        loadGroups();
      }),
    ];
    api.takeOpenChat().then(openFromOutside);
    return () => unsubscribers.forEach(unsubscribe => unsubscribe());
    // Subscribed once: the handlers only use state setters and refs.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  useEffect(() => {
    messagesEndRef.current?.scrollIntoView({ behavior: 'smooth' });
  }, [chatMessages]);

  const showChatError = (text: string) => {
    setChatError(text);
    setTimeout(() => setChatError(''), 3000);
  };

  const sendChatMessage = async () => {
    const text = chatInput.trim();
    if (!text || !chatFriendId) return;
    const entry = await api.sendMessage(chatFriendId, text);
    if ('error' in entry) {
      showChatError(entry.error === 'too_long' ? 'Message trop long (5000 caractères max).' : 'Message non envoyé.');
      return;
    }
    setChatMessages(prev => [...prev, entry]);
    setChatInput('');
  };

  const addFriend = async () => {
    if (!newFriendId.trim() || !newFriendName.trim()) return;
    const res = await api.addFriend(newFriendId, newFriendName);
    if ('error' in res) {
      setAddError(ADD_FRIEND_ERRORS[res.error] ?? 'Impossible d’ajouter cet ami.');
      return;
    }
    setAddError('');
    setFriends(res.friends);
    setNewFriendId('');
    setNewFriendName('');
  };

  const removeFriend = async (id: string, e: React.MouseEvent) => {
    e.stopPropagation();
    setFriends(await api.removeFriend(id));
  };

  const sendFile = async (friendId: string) => {
    const count = await api.selectAndSendFile(friendId);
    if (count > 0) setActiveTab('transfers');
  };

  const clearFinished = () => {
    api.clearFinished();
    setTransfers(prev => prev.filter(t => !FINAL.has(t.status)));
  };

  const onlineCount = friends.filter(f => f.online).length;
  const onlineLabel = onlineCount === 0 ? 'Aucun ami en ligne' : `${onlineCount} ami${onlineCount > 1 ? 's' : ''} en ligne`;

  /* ─── Group View ─── */
  if (activeTab === 'group' && groupId) {
    return <GroupView key={groupId} gid={groupId} friends={friends} transfers={transfers} onBack={() => setActiveTab('groups')} />;
  }

  /* ─── Chat View ─── */
  if (activeTab === 'chat' && chatFriend) {
    return (
      <div className="app">
        <div className="header">
          <div className="brand">
            <button className="icon-btn" title="Retour" onClick={() => setActiveTab('friends')}><Icon name="back" /></button>
            <div className="avatar small">{chatFriend.name[0]?.toUpperCase()}</div>
            <div className="header-title">
              <div className="title">{chatFriend.name}</div>
              <div className={`subtitle ${chatFriend.online ? 'online' : ''}`}>
                {chatFriend.online ? 'En ligne · chiffré de bout en bout' : 'Hors ligne · envoi à sa reconnexion'}
              </div>
            </div>
          </div>
          <button className="icon-btn" title="Masquer" onClick={() => api.hideWindow()}><Icon name="close" /></button>
        </div>

        <div className="chat-messages">
          {chatMessages.length === 0 && (
            <div className="empty-state">
              <Icon name="chat" size={32} />
              Aucun message. Dites bonjour !
            </div>
          )}
          {chatMessages.map(m => {
            const mine = m.from === myId;
            return (
              <div key={m.id} className={`bubble-row ${mine ? 'mine' : ''}`}>
                <div className={`bubble ${mine ? 'mine' : 'theirs'}`}>
                  {m.text}
                  <div className="bubble-time">
                    {new Date(m.ts).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' })}
                    {mine && (
                      <span title={m.status === 'pending' ? 'En attente de livraison' : 'Livré'}>
                        {m.status === 'pending' ? ' · En attente' : ' ✓✓'}
                      </span>
                    )}
                  </div>
                </div>
              </div>
            );
          })}
          <div ref={messagesEndRef} />
        </div>

        <div className="chat-input-row">
          {chatError && <div className="chat-error">{chatError}</div>}
          <input
            className="input"
            placeholder="Écrire un message…"
            value={chatInput}
            maxLength={5000}
            onChange={e => setChatInput(e.target.value)}
            onKeyDown={e => e.key === 'Enter' && sendChatMessage()}
            style={{ flex: 1 }}
          />
          <button className="btn btn-icon" title="Envoyer" onClick={sendChatMessage}><Icon name="send" /></button>
        </div>
      </div>
    );
  }

  /* ─── Main View ─── */
  return (
    <div className="app">
      {/* Header */}
      <div className="header">
        <div className="brand">
          <div className="brand-icon"><Icon name="sync" size={18} /></div>
          <div className="header-title">
            <div className="title">P2P Share</div>
            <div className="subtitle">{onlineLabel}</div>
          </div>
        </div>
        <div className="header-actions">
          <button className={`icon-btn ${activeTab === 'settings' ? 'active' : ''}`} title="Paramètres" onClick={() => setActiveTab('settings')}><Icon name="settings" /></button>
          <button className="icon-btn" title="Masquer" onClick={() => api.hideWindow()}><Icon name="close" /></button>
        </div>
      </div>

      {/* Nav */}
      {activeTab !== 'settings' && (
        <div className="pivot">
          <button className={`pivot-btn ${activeTab === 'friends' ? 'active' : ''}`} onClick={() => setActiveTab('friends')}>
            Amis
          </button>
          <button className={`pivot-btn ${activeTab === 'groups' ? 'active' : ''}`} onClick={() => setActiveTab('groups')}>
            Groupes{groupBadge > 0 && <span className="badge">{groupBadge}</span>}
          </button>
          <button className={`pivot-btn ${activeTab === 'transfers' ? 'active' : ''}`} onClick={() => setActiveTab('transfers')}>
            Transferts{runningCount > 0 && <span className="badge">{runningCount}</span>}
          </button>
        </div>
      )}

      <div className="content">

        {/* ── File Offer Banners ── */}
        {offers.map(offer => (
          <div key={offer.id} className="offer-banner">
            <div className="file-icon"><Icon name="file" size={18} /></div>
            <div style={{ flex: 1, minWidth: 0 }}>
              <div className="offer-title">Fichier entrant</div>
              <div className="offer-desc">
                <strong>{friendName(offer.friendId) ?? 'Un ami'}</strong> vous envoie{' '}
                <strong>"{offer.fileName}"</strong>{' '}
                ({formatBytes(offer.fileSize)})
              </div>
              <div style={{ display: 'flex', gap: 8 }}>
                <button className="btn" style={{ flex: 1 }} onClick={() => { api.respondToFileOffer(offer.id, true); setActiveTab('transfers'); }}>Accepter</button>
                <button className="btn btn-secondary" style={{ flex: 1 }} onClick={() => api.respondToFileOffer(offer.id, false)}>Refuser</button>
              </div>
            </div>
          </div>
        ))}

        {/* ── Friends Tab ── */}
        {activeTab === 'friends' && (
          <>
            {/* My ID */}
            <div className="card id-box">
              <div style={{ minWidth: 0 }}>
                <div className="card-label">Mon ID</div>
                <div className="id-value">{myId}</div>
              </div>
              <button className="btn btn-secondary btn-small" onClick={() => navigator.clipboard.writeText(myId)}><Icon name="copy" size={14} />Copier</button>
            </div>

            {/* Friend List */}
            {friends.length > 0 && <div className="section-header">Amis ({friends.length})</div>}
            {friends.length > 0 && (
              <div className="list">
                {friends.map(f => (
                  <div key={f.id} className="list-item">
                    <div className="avatar">{f.name[0]?.toUpperCase()}</div>
                    <div className="item-body">
                      <div className="item-title">{f.name}</div>
                      {f.legacy ? (
                        <div className="item-sub danger" title="Cet ID vient d’une ancienne version. Demandez son nouvel ID et ajoutez-le à nouveau.">
                          ID obsolète
                        </div>
                      ) : (
                        <div className={`item-sub ${f.online ? 'online' : ''}`}>
                          {f.online ? 'En ligne' : 'Hors ligne'}
                        </div>
                      )}
                    </div>
                    <div className="item-actions">
                      <button className="icon-btn" title="Chat" disabled={f.legacy} onClick={() => openChatWith(f.id)}><Icon name="chat" /></button>
                      <button className="icon-btn" title="Envoyer des fichiers" disabled={f.legacy} onClick={() => sendFile(f.id)}><Icon name="fileUp" /></button>
                      <button className="icon-btn danger" title="Supprimer" onClick={(e) => removeFriend(f.id, e)}><Icon name="trash" /></button>
                    </div>
                  </div>
                ))}
              </div>
            )}
            {friends.length === 0 && (
              <div className="empty-state">
                <Icon name="users" size={32} />
                Aucun ami. Ajoutez quelqu'un ci-dessous.
              </div>
            )}

            {/* Add Friend */}
            <div className="section-header" style={{ marginTop: 6 }}>Ajouter un ami</div>
            <input className="input" placeholder="ID de l'ami (64 caractères)" value={newFriendId} onChange={e => { setNewFriendId(e.target.value); setAddError(''); }} />
            <input className="input" placeholder="Pseudo (ex: Alice)" value={newFriendName} onChange={e => setNewFriendName(e.target.value)} onKeyDown={e => e.key === 'Enter' && addFriend()} />
            {addError && <div className="error-text">{addError}</div>}
            <button className="btn" style={{ width: '100%' }} onClick={addFriend}><Icon name="plus" size={14} />Ajouter cet ami</button>
          </>
        )}

        {/* ── Groups Tab ── */}
        {activeTab === 'groups' && <GroupsTab overview={overview} friends={friends} onOpen={openGroup} />}

        {/* ── Transfers Tab ── */}
        {activeTab === 'transfers' && (
          <>
            {transferList.length === 0 && (
              <div className="empty-state">
                <Icon name="transfers" size={32} />
                Aucun transfert en cours.
              </div>
            )}
            {transferList.length > runningCount && (
              <button className="btn-link" onClick={clearFinished}>Effacer les transferts terminés</button>
            )}
            {transferList.length > 0 && (
              <div className="list">
                {transferList.map(t => (
                  <div key={t.id} className="list-item transfer-item">
                    <div className="file-icon"><Icon name="file" size={18} /></div>
                    <div className="item-body">
                      <div className="transfer-header">
                        <span className="item-title transfer-name" title={t.fileName}>{t.fileName}</span>
                        <span className="transfer-status" style={{ color: transferColor(t.status) }}>
                          {transferLabel(t)}
                        </span>
                      </div>
                      <div className="progress-track">
                        <div
                          className="progress-fill"
                          style={{
                            width: `${FINAL.has(t.status) && t.status !== 'completed' ? 100 : t.progress}%`,
                            background: t.status === 'completed' ? 'var(--success)' : FINAL.has(t.status) ? 'var(--danger)' : 'var(--accent)'
                          }}
                        />
                      </div>
                      <div className="transfer-meta">
                        <span className="transfer-dir">
                          <Icon name={t.dir === 'out' ? 'up' : 'down'} size={12} />
                          {t.dir === 'out' ? 'Envoi' : 'Réception'}
                          {friendName(t.friendId) ? ` · ${friendName(t.friendId)}` : ''}
                          {' · '}
                          {ACTIVE.has(t.status) || t.status === 'paused' ? `${formatBytes(t.bytes)} / ` : ''}{formatBytes(t.fileSize)}
                        </span>
                        {ACTIVE.has(t.status) && t.speed > 0 && (
                          <span style={{ whiteSpace: 'nowrap' }}>
                            {formatSpeed(t.speed)} · {formatEta(t)}
                          </span>
                        )}
                      </div>
                      {t.error && <div className="error-text" style={{ marginTop: 4 }}>{t.error}</div>}
                      {(!FINAL.has(t.status) || (t.status === 'completed' && t.dir === 'in')) && (
                        <div style={{ display: 'flex', justifyContent: 'flex-end', gap: 6, marginTop: 8 }}>
                          {t.status === 'completed' && t.dir === 'in' && (
                            <button className="btn btn-secondary btn-small" onClick={() => api.showInFolder(t.id)}><Icon name="folder" size={14} />Afficher</button>
                          )}
                          {!FINAL.has(t.status) && (
                            <button className="btn btn-secondary btn-small" onClick={() => api.cancelTransfer(t.id)}>Annuler</button>
                          )}
                        </div>
                      )}
                    </div>
                  </div>
                ))}
              </div>
            )}
          </>
        )}

        {/* ── Settings Tab ── */}
        {activeTab === 'settings' && (
          <>
            <div className="page-title">
              <button className="icon-btn" title="Retour" onClick={() => setActiveTab('friends')}><Icon name="back" /></button>
              <span>Paramètres</span>
            </div>

            <div className="card">
              <div className="card-label">Mon pseudo</div>
              <div style={{ display: 'flex', gap: 8, alignItems: 'center' }}>
                <input className="input" maxLength={40} placeholder="Nom affiché dans les groupes" value={nickname} onChange={e => setNickname(e.target.value)} />
                <button className="btn btn-secondary btn-small" style={{ height: 34 }} disabled={!nickname.trim() || nickname.trim() === config.nickname} onClick={async () => {
                  const saved = await api.setNickname(nickname);
                  setNickname(saved);
                  setConfig({ ...config, nickname: saved });
                }}>Enregistrer</button>
              </div>
              <div className="setting-sub" style={{ marginTop: 6 }}>Vu par les membres de vos groupes qui ne sont pas vos amis.</div>
            </div>

            <div className="card">
              <div className="card-label">Dossier de téléchargement</div>
              <div style={{ display: 'flex', gap: 8, alignItems: 'center' }}>
                <div className="path-box" title={config.downloadPath}>{config.downloadPath || 'Dossier système'}</div>
                <button className="btn btn-secondary btn-small" onClick={async () => {
                  const p = await api.selectDownloadDir();
                  if (p) setConfig({ ...config, downloadPath: p });
                }}>Changer</button>
              </div>
            </div>

            <RelaySettings config={config} />

            <div className="card">
              <div className="setting-row">
                <div>
                  <div className="setting-label">Lancement au démarrage</div>
                  <div className="setting-sub">Démarrer automatiquement avec l’ordinateur</div>
                </div>
                <label className="toggle">
                  <input
                    type="checkbox"
                    checked={config.autoLaunch ?? true}
                    onChange={async (e) => {
                      const val = await api.setAutoLaunch(e.target.checked);
                      setConfig({ ...config, autoLaunch: val });
                    }}
                  />
                  <span className="toggle-track" />
                </label>
              </div>
            </div>

            <div className="card">
              <div className="setting-row">
                <div>
                  <div className="setting-label">Mode sombre</div>
                  <div className="setting-sub">Thème foncé / thème clair</div>
                </div>
                <label className="toggle">
                  <input
                    type="checkbox"
                    checked={theme === 'dark'}
                    onChange={e => setTheme(e.target.checked ? 'dark' : 'light')}
                  />
                  <span className="toggle-track" />
                </label>
              </div>
            </div>

            <div className="card">
              <div className="card-label">Mon identifiant P2P</div>
              <div className="id-full">{myId}</div>
            </div>
          </>
        )}
      </div>
    </div>
  );
}

export default App;
