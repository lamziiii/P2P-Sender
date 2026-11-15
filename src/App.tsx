import { useCallback, useState, useEffect, useRef } from 'react';
import type { ReactNode } from 'react';
import { api } from './api';
import type { ChatMessage, Config, DropTarget, Friend, GroupsOverview, InviteLink, OpenChat, Shared, Transfer } from './api';
import { ChangelogDialog } from './Changelog';
import { DndButton } from './Dnd';
import { FINAL, folderErrors, folderSendError, formatBytes, plural, RELAY_HINT, ROUTE_LABELS } from './format';
import { FriendChatView } from './FriendChat';
import { FriendsTab } from './FriendsTab';
import type { Unread } from './FriendsTab';
import { GroupsTab, GroupView } from './Groups';
import { lang, locale, tr } from './i18n';
import { Icon } from './icons';
import { InviteDialog } from './Invite';
import { WindowButtons } from './PinButton';
import { LocalSendChatView } from './LocalSendChat';
import { byPlatform, isMobile, useBackButton } from './platform';
import { useConnection } from './relay';
import { newsToShow, RELEASES } from './releaseNotes';
import { SettingsTab } from './SettingsTab';
import { ShareDialog } from './Share';
import type { Theme } from './SettingsTab';
import { TransfersTab } from './TransfersTab';
import { UpdateBanner } from './Update';
import { transferStore, useOffers, useRunning, useTransfers } from './transferStore';
import { Welcome } from './Welcome';

// Shown in the chat with a preview (same list as the backend's).
const IMAGE_EXT = /\.(jpe?g|png|gif|webp|bmp)$/i;
// Longest last message shown in the friend list (as the backend cuts it).
const PREVIEW_CHARS = 120;
// How long "typing" shows without news from the friend.
const TYPING_SHOWN = 5000;
// Transfer updates shown together within that many ms.
const UPDATE_BATCH = 100;
// Largest file sent by pasting it: it goes through memory.
const MAX_PASTE = 500 * 1024 * 1024;

// Running transfers, seen from every tab but their own. Renders on its own
// when their progress changes.
function TransferStrip({ onOpen }: { onOpen: () => void }) {
  const { count, active, progress, route } = useRunning();
  if (count === 0) return null;
  return (
    <button className="transfer-strip list-button" title={route === 'relay' ? `${tr('Voir les transferts.', 'See the transfers.')} ${RELAY_HINT}` : tr('Voir les transferts', 'See the transfers')} onClick={onOpen}>
      <span>
        {plural(count, tr('transfert en cours', 'transfer in progress'), tr('transferts en cours', 'transfers in progress'))}{active ? ` · ${progress} %` : ''}
        {active && route ? ` · ${ROUTE_LABELS[route]}` : ''}
      </span>
      <div className="progress-track"><div className="progress-fill" style={{ width: `${progress}%`, background: 'var(--accent)' }} /></div>
    </button>
  );
}

function RunningBadge() {
  const { count } = useRunning();
  return count > 0 ? <span className="badge">{count}</span> : null;
}

// The removed friend's transfers that would be cancelled.
function RemovedTransfers({ friendId }: { friendId: string }) {
  const n = useTransfers().filter(t => t.friendId === friendId && !FINAL.has(t.status)).length;
  return n > 0
    ? <> {plural(n, tr('transfert en cours sera annulé', 'transfer in progress will be cancelled'), tr('transferts en cours seront annulés', 'transfers in progress will be cancelled'))}.</>
    : null;
}

// A chat's last message, as the friend list shows it.
const withLastMessage = (f: Friend, m: ChatMessage, myId: string): Friend => ({
  ...f,
  lastText: m.file ? 'Image' : Array.from(m.text).slice(0, PREVIEW_CHARS).join(''),
  lastTs: m.ts,
  lastMine: m.from === myId,
});

function App() {
  const [activeTab, setActiveTab] = useState('friends');
  const [friends, setFriends] = useState<Friend[]>([]);
  const [myId, setMyId] = useState('Chargement...');
  const [config, setConfig] = useState<Partial<Config>>({});
  const [chatFriendId, setChatFriendId] = useState<string | null>(null);
  const [chatMessages, setChatMessages] = useState<ChatMessage[]>([]);
  // Friends typing to us right now.
  const [typing, setTyping] = useState<Record<string, true>>({});
  const typingTimers = useRef(new Map<string, ReturnType<typeof setTimeout>>());
  const [overview, setOverview] = useState<GroupsOverview>({ groups: [], invites: [] });
  const [groupId, setGroupId] = useState<string | null>(null);
  const [nickname, setNickname] = useState('');
  const [lsChat, setLsChat] = useState<string | null>(null);
  // Friend messages received while their chat was not open, by friend.
  const [unread, setUnread] = useState<Unread>({});
  // First launch: the welcome comes before everything else.
  const [welcome, setWelcome] = useState(false);
  // Just updated: what the new version brings, shown once.
  const [news, setNews] = useState(false);
  // Friend whose removal waits for confirmation.
  const [removing, setRemoving] = useState<Friend | null>(null);
  // An invitation link opened (or pasted): asked whether to add whoever made it.
  const [invite, setInvite] = useState<InviteLink | null>(null);
  // Given to the app from outside ("Envoyer avec P2P Share"): asked where it goes.
  const [sharing, setSharing] = useState<Shared | null>(null);
  const closeShare = useCallback(() => setSharing(null), []);
  // Files dragged over the window, and where they would go.
  const [dragging, setDragging] = useState(false);
  const [dropTarget, setDropTarget] = useState<DropTarget | null>(null);
  const [toast, setToast] = useState('');
  // Settings opened from the network warning show the advanced part.
  const [settingsAdvanced, setSettingsAdvanced] = useState(false);
  const toastTimer = useRef<ReturnType<typeof setTimeout>>(undefined);
  // What the event handlers need of the current view.
  const viewRef = useRef({ tab: 'friends', chat: null as string | null, group: null as string | null, ls: null as string | null });
  const [theme, setTheme] = useState<Theme>(
    () => (localStorage.getItem('theme') as Theme | null) ?? 'system'
  );
  const { connection } = useConnection();
  // Mobile: the back button leaves the chats, groups and settings.
  useBackButton(!welcome && ['chat', 'group', 'localsend', 'settings'].includes(activeTab));
  const chatFriendIdRef = useRef<string | null>(null);
  // Delivery statuses that came before their message was shown, by message id.
  const earlyStatus = useRef(new Map<string, ChatMessage['status']>());
  // Only the latest answer of a burst of group reloads is applied.
  const groupsLoadId = useRef(0);
  // For the event handlers: our ID once known, and the friends shown.
  const myIdRef = useRef<string | null>(null);
  const friendsRef = useRef(friends);
  const overviewRef = useRef(overview);

  const chatFriend = friends.find(f => f.id === chatFriendId) ?? null;
  const offers = useOffers();
  // Several files from one sender are answered together, a folder's apart.
  const offerGroups = Object.values(offers.reduce<Record<string, Transfer[]>>((groups, t) => {
    (groups[t.folder ? `${t.friendId}/${t.folder.id}` : t.friendId] ??= []).push(t);
    return groups;
  }, {}));
  const friendBadge = Object.values(unread).reduce((n, u) => n + u.count, 0);
  // Muted groups count only their mentions.
  const groupBadge = overview.invites.length
    + overview.groups.reduce((n, g) => n + (g.muted ? g.unreadMentions : g.unread), 0);
  const friendName = (id: string) => friends.find(f => f.id === id)?.name;
  const senderName = (t: Transfer) => friendName(t.friendId) ?? t.peerName;
  const transferName = useCallback(
    (t: Transfer) => friends.find(f => f.id === t.friendId)?.name ?? t.peerName ?? tr('votre ami', 'your friend'),
    [friends],
  );
  // Paused by hand: with friends only (not LocalSend devices).
  const pausable = useCallback(
    (t: Transfer) => !t.peerName && !t.group && friends.some(f => f.id === t.friendId),
    [friends],
  );
  useEffect(() => { friendsRef.current = friends; }, [friends]);
  useEffect(() => { overviewRef.current = overview; }, [overview]);

  useEffect(() => {
    // Leaving the chat: its friend's next messages count as unread.
    if (activeTab !== 'chat') chatFriendIdRef.current = null;
    viewRef.current = {
      tab: activeTab,
      chat: activeTab === 'chat' ? chatFriendId : null,
      group: activeTab === 'group' ? groupId : null,
      ls: activeTab === 'localsend' ? lsChat : null,
    };
  }, [activeTab, chatFriendId, groupId, lsChat]);

  const withEarlyStatus = (m: ChatMessage): ChatMessage => {
    const status = earlyStatus.current.get(m.id);
    if (status === undefined) return m;
    earlyStatus.current.delete(m.id);
    return { ...m, status };
  };

  const showToast = useCallback((text: string) => {
    setToast(text);
    clearTimeout(toastTimer.current);
    toastTimer.current = setTimeout(() => setToast(''), 4000);
  }, []);

  // The open chat is seen: the friend gets "Vu" (if both show it).
  const markRead = () => {
    const friendId = chatFriendIdRef.current;
    if (friendId && document.hasFocus()) api.markChatRead(friendId);
  };

  const stopTyping = (friendId: string) => {
    clearTimeout(typingTimers.current.get(friendId));
    typingTimers.current.delete(friendId);
    setTyping(prev => {
      if (!prev[friendId]) return prev;
      const next = { ...prev };
      delete next[friendId];
      return next;
    });
  };

  // Our reactions, as the backend kept them.
  const setReactions = useCallback((id: string, reactions: Record<string, string>) => {
    setChatMessages(prev => prev.map(m => (m.id === id ? { ...m, reactions } : m)));
  }, []);

  const loadGroups = () => {
    const id = ++groupsLoadId.current;
    return api.getGroups().then(o => { if (id === groupsLoadId.current) setOverview(o); });
  };

  const loadFriends = () => { api.getFriends().then(setFriends); };
  const takeInvite = () => { api.takeInviteLink().then(link => { if (link) setInvite(link); }); };
  // More may come while the dialog is open (one file at a time from the Explorer).
  const takeShared = () => {
    api.takeShared().then(shared => {
      if (!shared) return;
      setSharing(prev => (prev
        ? { items: [...prev.items, ...shared.items.filter(i => !prev.items.some(p => p.path === i.path))], text: shared.text ?? prev.text }
        : shared));
    });
  };

  // A message came or went: its friend's last message, without asking the
  // whole list again (unless the friend or our ID is not known yet).
  const showLastMessage = (friendId: string, message: ChatMessage) => {
    const me = myIdRef.current;
    if (me === null || !friendsRef.current.some(f => f.id === friendId)) {
      loadFriends();
      return;
    }
    setFriends(prev => prev.map(f => (f.id === friendId ? withLastMessage(f, message, me) : f)));
  };

  useEffect(() => {
    const media = matchMedia('(prefers-color-scheme: dark)');
    const apply = () => {
      const dark = theme === 'dark' || (theme === 'system' && media.matches);
      document.documentElement.setAttribute('data-theme', dark ? 'dark' : '');
    };
    apply();
    localStorage.setItem('theme', theme);
    media.addEventListener('change', apply);
    return () => media.removeEventListener('change', apply);
  }, [theme]);

  const openChatWith = async (friendId: string) => {
    chatFriendIdRef.current = friendId;
    setChatFriendId(friendId);
    setChatMessages([]);
    setActiveTab('chat');
    setUnread(prev => {
      const next = { ...prev };
      delete next[friendId];
      return next;
    });
    earlyStatus.current.clear();
    const history = await api.getMessages(friendId);
    // Another chat may have been opened meanwhile.
    if (chatFriendIdRef.current !== friendId) return;
    // Keep what arrived while loading.
    setChatMessages(prev => {
      const known = new Set(history.map(m => m.id));
      return [...history, ...prev.filter(m => !known.has(m.id))].map(m => withEarlyStatus(m));
    });
    markRead();
  };

  const openGroup = (gid: string) => {
    chatFriendIdRef.current = null;
    setGroupId(gid);
    setActiveTab('group');
  };

  const openLocalSendChat = (fingerprint: string) => {
    chatFriendIdRef.current = null;
    setLsChat(fingerprint);
    setActiveTab('localsend');
  };

  const openFromOutside = (target: OpenChat | null) => {
    if (target?.kind === 'group') openGroup(target.id);
    else if (target?.kind === 'localsend') openLocalSendChat(target.id);
    else if (target) openChatWith(target.id);
  };

  const openSettings = (advanced: boolean) => {
    setSettingsAdvanced(advanced);
    setActiveTab('settings');
  };

  // Files sent, and where to follow them.
  const sentToast = (sent: number) => tr(
    `${plural(sent, 'fichier envoyé', 'fichiers envoyés')} : suivez l’envoi dans l’onglet Transferts.`,
    `${plural(sent, 'file sent', 'files sent')}: follow it in the Transfers tab.`,
  );

  // Files and folders dropped on a friend, a group or a LocalSend device.
  const dropPaths = async (target: DropTarget, paths: string[]) => {
    const inView = viewRef.current.tab !== 'friends' && viewRef.current.tab !== 'groups';
    if (target.kind === 'friend') {
      const { sent, skipped, errors } = await api.sendPaths(target.id, paths);
      if (errors.length > 0) showToast(folderErrors(errors));
      else if (skipped > 0) showToast(`${plural(skipped, tr('élément introuvable ignoré', 'missing item skipped'), tr('éléments introuvables ignorés', 'missing items skipped'))}.`);
      if (sent === 0) return;
      if (!inView) setActiveTab('transfers');
      // In the chat, only the images show.
      else if (errors.length === 0 && skipped === 0 && !paths.every(path => IMAGE_EXT.test(path))) {
        showToast(sentToast(sent));
      }
    } else if (target.kind === 'group') {
      const added = await api.addGroupPaths(target.id, paths).catch(() => 0);
      showToast(added > 0
        ? tr(
          `${plural(added, 'fichier ajouté', 'fichiers ajoutés')} au dépôt de « ${target.name || 'ce groupe'} ».`,
          `${plural(added, 'file added', 'files added')} to the shared files of “${target.name || 'this group'}”.`,
        )
        : tr('Aucun fichier ajouté au groupe.', 'No file added to the group.'));
    } else {
      try {
        const { sent, skipped } = await api.sendPathsToLocalSend(target.id, paths);
        if (skipped > 0) {
          showToast(sent > 0
            ? tr(
              `${plural(skipped, 'dossier ignoré', 'dossiers ignorés')} : un appareil LocalSend ne reçoit d’ici que des fichiers.`,
              `${plural(skipped, 'folder skipped', 'folders skipped')}: a LocalSend device only gets files from here.`,
            )
            : tr('Un appareil LocalSend ne reçoit d’ici que des fichiers, pas de dossiers.', 'A LocalSend device only gets files from here, not folders.'));
        }
        if (sent === 0) return;
        if (!inView) setActiveTab('transfers');
        else if (skipped === 0) showToast(sentToast(sent));
      } catch (e) {
        showToast(String(e));
      }
    }
  };

  // Files pasted in a chat: saved, then sent as if dropped there.
  const pasteFiles = async (target: DropTarget, files: File[]) => {
    if (files.some(f => f.size > MAX_PASTE)) {
      showToast(tr(
        `Fichier trop gros pour être collé : ${byPlatform('glissez-le dans la discussion', 'envoyez-le avec le trombone')}.`,
        `File too big to paste: ${byPlatform('drop it into the chat', 'send it with the paperclip')}.`,
      ));
      return;
    }
    try {
      const stamp = new Date().toLocaleString(locale).replace(/\//g, '-').replace(/:/g, '.');
      const paths = await Promise.all(files.map(f => {
        // A screenshot comes as "image.png": named after when it was pasted.
        const generic = !f.name || /^image\.\w+$/i.test(f.name);
        const ext = f.name.split('.').pop() || 'png';
        return api.savePasted(f, generic ? tr(`Image collée ${stamp}.${ext}`, `Pasted image ${stamp}.${ext}`) : f.name);
      }));
      await dropPaths(target, paths);
    } catch {
      showToast(tr('Impossible d’envoyer ce que vous avez collé.', 'What you pasted could not be sent.'));
    }
  };

  // A text shared from another app (Android): a message.
  const sendSharedText = async (target: DropTarget, text: string) => {
    try {
      if (target.kind === 'friend') {
        const res = await api.sendMessage(target.id, text);
        if ('error' in res) throw new Error(res.error);
        openChatWith(target.id);
      } else if (target.kind === 'group') {
        const res = await api.sendGroupMessage(target.id, text);
        if ('error' in res) throw new Error(res.error);
        openGroup(target.id);
      } else {
        await api.sendLocalSendText(target.id, text);
        openLocalSendChat(target.id);
      }
    } catch {
      showToast(tr('Le message n’a pas pu être envoyé.', 'The message could not be sent.'));
    }
  };

  // What was shared, sent where the dialog said, as if dropped there.
  const sendShared = async (target: DropTarget) => {
    const shared = sharing;
    setSharing(null);
    if (!shared) return;
    if (shared.items.length === 0) {
      if (shared.text !== undefined) await sendSharedText(target, shared.text);
      return;
    }
    const paths = await api.sharedPaths(shared.items.map(i => i.path), target.kind === 'group').catch(() => []);
    if (paths.length === 0) {
      showToast(tr('Impossible de lire ce que vous voulez envoyer.', 'What you want to send could not be read.'));
      return;
    }
    await dropPaths(target, paths);
  };

  useEffect(() => {
    // The backend writes its own texts in the window's language.
    api.setLanguage(lang);
    api.getMyId().then(id => {
      myIdRef.current = id;
      setMyId(id);
    });
    loadFriends();
    api.getConfig().then(c => {
      setConfig(c);
      setNickname(c.nickname);
      setWelcome(!c.welcomed);
      if (newsToShow(c.welcomed)) setNews(true);
    });
    loadGroups();
    api.getTransfers().then(list => transferStore.update(prev => {
      const known = new Set(prev.map(t => t.id));
      return [...prev, ...list.filter(t => !known.has(t.id))];
    }));

    // A folder of small files brings hundreds of updates a second: they are
    // applied together, newest first as one by one.
    const updates = new Map<string, Transfer>();
    let flushTimer: ReturnType<typeof setTimeout> | undefined;
    const flushUpdates = () => {
      flushTimer = undefined;
      const batch = [...updates.values()];
      updates.clear();
      transferStore.update(prev => {
        const index = new Map(prev.map((t, i) => [t.id, i]));
        const next = [...prev];
        const added: Transfer[] = [];
        for (const t of batch) {
          const i = index.get(t.id);
          if (i === undefined) added.unshift(t);
          else next[i] = t;
        }
        return [...added, ...next];
      });
    };

    const unsubscribers = [
      api.onTransferUpdate(data => {
        updates.set(data.id, data);
        flushTimer ??= setTimeout(flushUpdates, UPDATE_BATCH);
      }),
      api.onNewMessage(({ friendId, message }) => {
        // The list shows each chat's last message.
        showLastMessage(friendId, message);
        if (message.from === friendId) stopTyping(friendId);
        if (chatFriendIdRef.current === friendId) {
          setChatMessages(prev => (prev.some(m => m.id === message.id) ? prev : [...prev, message]));
          if (message.from === friendId) markRead();
          return;
        }
        // An image comes with its own offer banner.
        if (message.file) return;
        // No system notification while the window has the focus: shown here.
        setUnread(prev => ({ ...prev, [friendId]: { count: (prev[friendId]?.count ?? 0) + 1, last: message.text } }));
      }),
      api.onMessageStatus(({ friendId, id, status }) => {
        if (chatFriendIdRef.current !== friendId) return;
        setChatMessages(prev => {
          // A late "delivered" does not undo "read".
          if (prev.some(m => m.id === id)) return prev.map(m => (m.id === id && m.status !== 'read' ? { ...m, status } : m));
          // Our message is not shown yet (send_message has not returned): applied when it is.
          earlyStatus.current.set(id, status);
          return prev;
        });
      }),
      api.onMessagesRead(({ friendId, ids }) => {
        if (chatFriendIdRef.current !== friendId) return;
        setChatMessages(prev => {
          const shown = new Set(prev.map(m => m.id));
          ids.filter(id => !shown.has(id)).forEach(id => earlyStatus.current.set(id, 'read'));
          const read = new Set(ids);
          return prev.map(m => (read.has(m.id) ? { ...m, status: 'read' } : m));
        });
      }),
      api.onMessageReactions(({ friendId, id, reactions }) => {
        if (chatFriendIdRef.current === friendId) setReactions(id, reactions);
      }),
      api.onFriendTyping(({ friendId }) => {
        clearTimeout(typingTimers.current.get(friendId));
        typingTimers.current.set(friendId, setTimeout(() => stopTyping(friendId), TYPING_SHOWN));
        setTyping(prev => (prev[friendId] ? prev : { ...prev, [friendId]: true }));
      }),
      api.onDndChanged(({ dndUntil }) => setConfig(c => ({ ...c, dndUntil }))),
      // The payload is used only when the backend kept nothing for take_open_chat.
      api.onOpenChat(
        target => { api.takeOpenChat().then(pending => openFromOutside(pending ?? target)); },
        // Asked once listening, so that a request made while loading is not lost.
        () => { api.takeOpenChat().then(openFromOutside); },
      ),
      api.onGroupUpdate(loadGroups),
      api.onFriendsChanged(loadFriends),
      // Asked once listening too: a link that opened the app comes before.
      api.onInviteLink(takeInvite, takeInvite),
      api.onShared(takeShared, takeShared),
      api.onDragDrop(e => {
        if (e.type === 'leave') {
          setDragging(false);
          setDropTarget(null);
          return;
        }
        const { tab, chat, group, ls } = viewRef.current;
        // From another tab of the main view, the friends come to the files.
        if (['transfers', 'settings'].includes(tab)) setActiveTab('friends');
        let target: DropTarget | null;
        if (chat) target = { kind: 'friend', id: chat, name: friendsRef.current.find(f => f.id === chat)?.name ?? '' };
        else if (group) target = { kind: 'group', id: group, name: overviewRef.current.groups.find(g => g.id === group)?.name ?? '' };
        else if (ls) target = { kind: 'localsend', id: ls, name: '' };
        else {
          const el = document
            .elementFromPoint(e.x / devicePixelRatio, e.y / devicePixelRatio)
            ?.closest<HTMLElement>('[data-drop-kind]');
          target = el ? { kind: el.dataset.dropKind as DropTarget['kind'], id: el.dataset.dropId!, name: el.dataset.dropName ?? '' } : null;
        }
        if (e.type !== 'drop') {
          setDragging(true);
          // Same place: the same object, not a render per mouse move.
          setDropTarget(prev => (prev?.kind === target?.kind && prev?.id === target?.id ? prev : target));
          return;
        }
        setDragging(false);
        setDropTarget(null);
        if (!target) {
          showToast(tab === 'groups'
            ? tr('Déposez les fichiers sur un groupe pour les ajouter à son dépôt.', 'Drop the files on a group to add them to its shared files.')
            : tr('Déposez les fichiers sur un ami ou un appareil pour les lui envoyer.', 'Drop the files on a friend or a device to send them.'));
          return;
        }
        dropPaths(target, e.paths);
      }),
      api.onFriendStatus(({ friendId, online }) => {
        setFriends(prev => prev.map(f => (f.id === friendId ? { ...f, online, route: online ? f.route : undefined } : f)));
        loadGroups();
      }),
      api.onFriendRoute(({ friendId, route }) => {
        setFriends(prev => prev.map(f => (f.id === friendId ? { ...f, route: route ?? undefined } : f)));
      }),
    ];

    // Escape goes back, or hides the window from the main view. Dialogs and
    // menus close themselves first.
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== 'Escape' || document.querySelector('.modal-backdrop, .menu-popover, .reaction-picker')) return;
      const { tab } = viewRef.current;
      if (tab === 'chat' || tab === 'localsend' || tab === 'settings') setActiveTab('friends');
      else if (tab === 'group') {
        setActiveTab('groups');
        loadGroups();
      } else if (!isMobile) api.hideWindow();
    };
    window.addEventListener('keydown', onKey);
    // Back to the window: the open chat is seen.
    window.addEventListener('focus', markRead);
    const timers = typingTimers.current;

    return () => {
      unsubscribers.forEach(unsubscribe => unsubscribe());
      clearTimeout(flushTimer);
      window.removeEventListener('keydown', onKey);
      window.removeEventListener('focus', markRead);
      clearTimeout(toastTimer.current);
      timers.forEach(clearTimeout);
    };
    // Subscribed once: the handlers only use state setters and refs.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  useEffect(() => {
    if (!removing) return;
    const onKey = (e: KeyboardEvent) => e.key === 'Escape' && setRemoving(null);
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, [removing]);

  // Our message, sent from the chat.
  const onSent = (friendId: string, entry: ChatMessage) => {
    if (chatFriendIdRef.current === friendId) setChatMessages(prev => [...prev, withEarlyStatus(entry)]);
    showLastMessage(friendId, entry);
  };

  const removeFriend = async (id: string) => {
    setRemoving(null);
    setFriends(await api.removeFriend(id));
  };

  // From the friend list: the transfers show where it goes.
  const sendFile = async (friendId: string) => {
    const count = await api.selectAndSendFile(friendId);
    if (count > 0) setActiveTab('transfers');
  };

  const sendFolder = async (friendId: string) => {
    const res = await api.sendFolder(friendId);
    if ('error' in res) showToast(folderSendError(res.error));
    else if (res.sent > 0) setActiveTab('transfers');
  };

  const clearFinished = () => {
    api.clearFinished();
    transferStore.update(prev => prev.filter(t => !FINAL.has(t.status)));
  };

  // An offer's answer; the full card (in the sender's chat) or one line
  // elsewhere. A folder's files are answered together.
  const offerView = (group: Transfer[], compact: boolean) => {
    const [first] = group;
    const folder = first.folder;
    const respond = (accept: boolean) => {
      if (folder) api.respondToFolder(first.friendId, folder.id, accept);
      else group.forEach(t => api.respondToFileOffer(t.id, accept));
      // In a chat, the download goes on there (the strip shows it).
      if (accept && viewRef.current.tab !== 'chat') setActiveTab('transfers');
    };
    const many = folder !== undefined || group.length > 1;
    const risky = group.some(t => t.executable);
    const size = formatBytes(folder ? folder.size : group.reduce((n, t) => n + t.fileSize, 0));
    const who = senderName(first) ?? tr('Un ami', 'A friend');
    const what = folder
      ? tr(`le dossier « ${folder.name} »`, `the folder “${folder.name}”`)
      : group.length > 1 ? tr(`${group.length} fichiers`, `${group.length} files`) : tr(`« ${first.fileName} »`, `“${first.fileName}”`);
    const fromFriend = !first.peerName && friends.some(f => f.id === first.friendId);
    const acceptLabel = many ? tr('Tout accepter', 'Accept all') : tr('Accepter', 'Accept');
    const declineLabel = many ? tr('Tout refuser', 'Decline all') : tr('Refuser', 'Decline');
    if (compact) {
      return (
        <div key={first.id} className="offer-banner compact">
          <div className="file-icon"><Icon name={risky ? 'alert' : folder ? 'folder' : 'file'} size={16} /></div>
          <button
            className="item-body list-button offer-line"
            disabled={!fromFriend}
            title={fromFriend ? tr('Ouvrir la discussion', 'Open the chat') : undefined}
            onClick={() => openChatWith(first.friendId)}
          >
            <div className="offer-title ellipsis">{tr(`${who} vous envoie ${what}`, `${who} sends you ${what}`)}</div>
            <div className={`item-sub ellipsis ${risky ? 'danger' : ''}`}>
              {folder ? `${plural(folder.files, tr('fichier', 'file'), tr('fichiers', 'files'))} · ` : ''}{size}
              {first.peerName ? tr(' · LocalSend, non vérifié', ' · LocalSend, unverified') : ''}{risky ? tr(' · peut exécuter du code', ' · can run code') : ''}
            </div>
          </button>
          <button className="btn btn-small" onClick={() => respond(true)}>{folder ? acceptLabel : tr('Accepter', 'Accept')}</button>
          <button className="btn btn-secondary btn-small" onClick={() => respond(false)}>{folder ? declineLabel : tr('Refuser', 'Decline')}</button>
        </div>
      );
    }
    return (
      <div key={first.id} className="offer-banner">
        <div className="file-icon"><Icon name={folder ? 'folder' : 'file'} size={18} /></div>
        <div style={{ flex: 1, minWidth: 0 }}>
          <div className="offer-title">{folder
            ? tr('Dossier entrant', 'Incoming folder')
            : group.length > 1 ? tr(`${group.length} fichiers entrants`, `${group.length} incoming files`) : tr('Fichier entrant', 'Incoming file')}</div>
          <div className="offer-desc">
            {folder
              ? <><strong>{tr(`« ${folder.name} »`, `“${folder.name}”`)}</strong> ({plural(folder.files, tr('fichier', 'file'), tr('fichiers', 'files'))}, {size})</>
              : group.length > 1
                ? <>{group.slice(0, 3).map(t => tr(`« ${t.fileName} »`, `“${t.fileName}”`)).join(', ')}{group.length > 3 ? tr(` et ${group.length - 3} autres`, ` and ${group.length - 3} more`) : ''} ({size})</>
                : <><strong>{tr(`« ${first.fileName} »`, `“${first.fileName}”`)}</strong> ({size})</>}
          </div>
          {risky && (
            <div className="error-text" style={{ marginBottom: 8 }}>
              {tr(
                `Attention : ${folder ? 'ce dossier contient des fichiers qui peuvent' : group.length > 1 ? 'certains fichiers peuvent' : 'ce fichier peut'} exécuter du code${folder ? ', reçus aussi si vous acceptez' : ''}. Ne les ouvrez que si vous attendiez cet envoi.`,
                `Warning: ${folder ? 'this folder holds files that can' : group.length > 1 ? 'some files can' : 'this file can'} run code${folder ? ', received too if you accept' : ''}. Only open them if you were expecting them.`,
              )}
            </div>
          )}
          <div style={{ display: 'flex', gap: 8 }}>
            <button className="btn" style={{ flex: 1 }} onClick={() => respond(true)}>{acceptLabel}</button>
            <button className="btn btn-secondary" style={{ flex: 1 }} onClick={() => respond(false)}>{declineLabel}</button>
          </div>
        </div>
      </div>
    );
  };

  // Friend messages not read yet, shown in the views without the friend list.
  const messageBanners = Object.entries(unread).map(([id, u]) => (
    // openChatWith only runs on click, not during render.
    // eslint-disable-next-line react-hooks/refs
    <button key={`msg-${id}`} className="offer-banner compact list-button" onClick={() => openChatWith(id)}>
      <div className="file-icon"><Icon name="chat" size={16} /></div>
      <div className="item-body">
        <div className="offer-title ellipsis">
          {u.count > 1
            ? tr(`${u.count} nouveaux messages de ${friendName(id) ?? 'un ami'}`, `${u.count} new messages from ${friendName(id) ?? 'a friend'}`)
            : tr(`Nouveau message de ${friendName(id) ?? 'un ami'}`, `New message from ${friendName(id) ?? 'a friend'}`)}
        </div>
        <div className="item-sub ellipsis">{u.last}</div>
      </div>
    </button>
  ));
  const offerBanners = offerGroups.map(group => offerView(group, true));
  const notices = [...offerBanners, ...messageBanners];

  const transferStrip = <TransferStrip onOpen={() => setActiveTab('transfers')} />;

  const dropHint = () => {
    if (dropTarget?.kind === 'group') {
      return tr(`Déposer pour ajouter au dépôt de « ${dropTarget.name || 'ce groupe'} »`, `Drop to add to the shared files of “${dropTarget.name || 'this group'}”`);
    }
    if (dropTarget) return tr(`Déposer pour envoyer à ${dropTarget.name || 'cet appareil'}`, `Drop to send to ${dropTarget.name || 'this device'}`);
    return activeTab === 'groups'
      ? tr('Déposez les fichiers ou dossiers sur un groupe', 'Drop the files or folders on a group')
      : tr('Déposez les fichiers ou dossiers sur un ami ou un appareil', 'Drop the files or folders on a friend or a device');
  };
  // Over every view: they are fixed on the window.
  const overlays = (
    <>
      {dragging && <div className={`drop-hint ${dropTarget ? 'ready' : ''}`}>{dropHint()}</div>}
      {toast && !dragging && <div className="toast">{toast}</div>}
      {news && (
        <ChangelogDialog
          title={tr(`Nouveautés de la version ${__APP_VERSION__}`, `What’s new in version ${__APP_VERSION__}`)}
          releases={RELEASES.filter(r => r.version === __APP_VERSION__)}
          onClose={() => setNews(false)}
        />
      )}
      {removing && (
        <div className="modal-backdrop" onClick={() => setRemoving(null)}>
          <div className="modal" role="dialog" aria-modal="true" onClick={e => e.stopPropagation()}>
            <div className="modal-title">{tr(`Supprimer ${removing.name} ?`, `Remove ${removing.name}?`)}</div>
            <div className="modal-text">
              {tr(
                'Vous ne pourrez plus discuter ni échanger de fichiers avec cet ami. Pour le retrouver, il faudra vous inviter à nouveau.',
                'You will no longer chat or exchange files with this friend. To get them back, you will have to invite each other again.',
              )}
              <RemovedTransfers friendId={removing.id} />
            </div>
            <div style={{ display: 'flex', gap: 8 }}>
              <button className="btn btn-secondary" style={{ flex: 1 }} autoFocus onClick={() => setRemoving(null)}>{tr('Annuler', 'Cancel')}</button>
              <button className="btn btn-danger" style={{ flex: 1 }} onClick={() => removeFriend(removing.id)}>{tr('Supprimer', 'Remove')}</button>
            </div>
          </div>
        </div>
      )}
      {invite && (
        <InviteDialog
          invite={invite}
          onDone={added => {
            setInvite(null);
            if (added) {
              setFriends(added.friends);
              setActiveTab('friends');
              showToast(tr(`${added.name || 'Votre ami'} est maintenant dans vos amis.`, `${added.name || 'Your friend'} is now your friend.`));
            }
          }}
        />
      )}
      {sharing && (
        <ShareDialog shared={sharing} friends={friends} groups={overview.groups} onPick={sendShared} onClose={closeShare} />
      )}
    </>
  );
  const withOverlays = (view: ReactNode) => <>{view}{overlays}</>;

  /* ─── First launch ─── */
  if (welcome) {
    return (
      <Welcome
        nickname={nickname}
        onNickname={saved => { setNickname(saved); setConfig(c => ({ ...c, nickname: saved })); }}
        onDone={() => { setWelcome(false); setConfig(c => ({ ...c, welcomed: true })); }}
      />
    );
  }

  /* ─── Group View ─── */
  if (activeTab === 'group' && groupId) {
    return withOverlays(
      <GroupView
        key={groupId}
        gid={groupId}
        friends={friends}
        notice={notices}
        dropping={dragging}
        onPasteFiles={files => pasteFiles({ kind: 'group', id: groupId, name: overview.groups.find(g => g.id === groupId)?.name ?? '' }, files)}
        onChange={loadGroups}
        onBack={() => { setActiveTab('groups'); loadGroups(); }}
      />
    );
  }

  /* ─── LocalSend Chat View ─── */
  if (activeTab === 'localsend' && lsChat) {
    return withOverlays(
      <LocalSendChatView
        key={lsChat}
        fingerprint={lsChat}
        notice={notices}
        dropping={dragging}
        onBack={() => setActiveTab('friends')}
        onToast={showToast}
        onPasteFiles={files => pasteFiles({ kind: 'localsend', id: lsChat, name: '' }, files)}
      />
    );
  }

  // The open chat's friend is gone (removed): back to the friend list.
  const tab = activeTab === 'chat' && !chatFriend ? 'friends' : activeTab;

  /* ─── Chat View ─── */
  if (tab === 'chat' && chatFriend) {
    // This friend's offers are answered in the conversation; images in
    // their own bubble (not those of a folder).
    const own = offerGroups.filter(g => g[0].friendId === chatFriend.id);
    const otherNotices = [
      ...offerGroups.filter(g => g[0].friendId !== chatFriend.id).map(g => offerView(g, true)),
      ...messageBanners,
    ];
    const inlineOffers = own
      .map(g => (g[0].folder ? g : g.filter(t => !IMAGE_EXT.test(t.fileName))))
      .filter(g => g.length > 0);
    const friendId = chatFriend.id;
    return withOverlays(
      <FriendChatView
        key={friendId}
        friend={chatFriend}
        myId={myId}
        messages={chatMessages}
        typing={!!typing[friendId]}
        notices={otherNotices}
        offers={inlineOffers.map(g => offerView(g, false))}
        strip={transferStrip}
        dropping={dragging}
        onBack={() => setActiveTab('friends')}
        onSent={entry => onSent(friendId, entry)}
        onReactions={setReactions}
        onToast={showToast}
        onPasteFiles={files => pasteFiles({ kind: 'friend', id: friendId, name: chatFriend.name }, files)}
      />
    );
  }

  /* ─── Main View ─── */
  // Relays blocked, friends on the same network may still be online.
  const shown = connection === 'blocked' && friends.some(f => f.online) ? 'local' : connection;
  const status = {
    online: { label: tr('Connecté', 'Connected'), className: 'online', hint: '' },
    connecting: { label: tr('Connexion au réseau…', 'Connecting…'), className: '', hint: '' },
    local: {
      label: tr('Réseau local uniquement', 'Local network only'),
      className: 'warning',
      hint: tr(
        'Les relais ne répondent pas : seuls vos amis sur le même réseau sont joignables. Ouvrir les réglages réseau.',
        'The relays do not answer: only your friends on the same network are reachable. Open the network settings.',
      ),
    },
    blocked: {
      label: tr('Relais injoignables', 'Relays unreachable'),
      className: 'danger',
      hint: config.localNetwork === false
        ? tr(
          'Les relais ne répondent pas et le réseau local est désactivé : vos amis ne sont pas joignables. Ouvrir les réglages réseau.',
          'The relays do not answer and the local network is off: your friends are not reachable. Open the network settings.',
        )
        : tr(
          'Les relais ne répondent pas : vos amis ne sont joignables que sur le même réseau. Ouvrir les réglages réseau.',
          'The relays do not answer: your friends are only reachable on the same network. Open the network settings.',
        ),
    },
  }[shown];
  return withOverlays(
    <div className="app">
      {/* Header */}
      <div className="header">
        <div className="brand">
          <div className="brand-icon"><Icon name="sync" size={18} /></div>
          <div className="header-title">
            <div className="title">P2P Share</div>
            {connection === 'blocked'
              ? (
                <button className={`subtitle status-line link-title ${status.className}`} title={status.hint} onClick={() => openSettings(true)}>
                  <Icon name="alert" size={11} /><span className="ellipsis">{status.label}</span>
                </button>
              )
              : (
                <div className={`subtitle status-line ${status.className}`}>
                  <span className={`status-dot ${connection}`} /><span className="ellipsis">{status.label}</span>
                </div>
              )}
          </div>
        </div>
        <div className="header-actions">
          <DndButton until={config.dndUntil ?? 0} onChange={dndUntil => setConfig(c => ({ ...c, dndUntil }))} />
          <button className={`icon-btn ${tab === 'settings' ? 'active' : ''}`} title={tr('Paramètres', 'Settings')} onClick={() => openSettings(false)}><Icon name="settings" /></button>
          <WindowButtons />
        </div>
      </div>
      {tab !== 'transfers' && tab !== 'settings' && transferStrip}

      {/* Nav */}
      {tab !== 'settings' && (
        <div className="pivot">
          <button className={`pivot-btn ${tab === 'friends' ? 'active' : ''}`} onClick={() => setActiveTab('friends')}>
            {tr('Amis', 'Friends')}{friendBadge > 0 && <span className="badge">{friendBadge}</span>}
          </button>
          <button className={`pivot-btn ${tab === 'groups' ? 'active' : ''}`} onClick={() => setActiveTab('groups')}>
            {tr('Groupes', 'Groups')}{groupBadge > 0 && <span className="badge">{groupBadge}</span>}
          </button>
          <button className={`pivot-btn ${tab === 'transfers' ? 'active' : ''}`} onClick={() => setActiveTab('transfers')}>
            {tr('Transferts', 'Transfers')}<RunningBadge />
          </button>
        </div>
      )}

      <div className="content">
        {tab === 'friends' && <UpdateBanner />}
        {tab !== 'settings' && offerBanners}
        {tab !== 'friends' && tab !== 'settings' && messageBanners}

        {tab === 'friends' && (
          <FriendsTab
            friends={friends}
            unread={unread}
            dropTarget={dropTarget}
            localNetwork={config.localNetwork ?? true}
            onFriends={setFriends}
            onOpenInvite={setInvite}
            onOpenChat={openChatWith}
            onSendFile={sendFile}
            onSendFolder={sendFolder}
            onRemove={setRemoving}
            onLocalSendSent={() => setActiveTab('transfers')}
            onOpenLocalSendChat={openLocalSendChat}
            mutedFriends={config.notifications?.mutedFriends ?? []}
            mutedLocalsend={config.notifications?.mutedLocalsend ?? []}
            onMuted={notifications => setConfig(c => ({ ...c, notifications }))}
          />
        )}

        {tab === 'groups' && (
          <GroupsTab
            overview={overview}
            friends={friends}
            dropTarget={dropTarget?.kind === 'group' ? dropTarget.id : null}
            onOpen={openGroup}
          />
        )}

        {tab === 'transfers' && (
          <TransfersTab nameOf={transferName} pausable={pausable} onClear={clearFinished} />
        )}

        {tab === 'settings' && (
          <SettingsTab
            key={settingsAdvanced ? 'advanced' : 'simple'}
            config={config}
            setConfig={setConfig}
            myId={myId}
            onFriends={setFriends}
            theme={theme}
            setTheme={setTheme}
            advancedOpen={settingsAdvanced}
            onBack={() => setActiveTab('friends')}
          />
        )}
      </div>
    </div>
  );
}

export default App;
