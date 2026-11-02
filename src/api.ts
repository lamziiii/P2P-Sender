import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { getCurrentWebview } from '@tauri-apps/api/webview';
import { markFoldersUnsupported } from './platform';

// lastText, lastTs, lastMine: the chat's last message, for the list.
export type Friend = {
  id: string;
  name: string;
  online: boolean;
  legacy: boolean;
  lastText?: string;
  lastTs: number;
  lastMine: boolean;
  // Their offers are accepted without asking, except programs.
  autoAccept: boolean;
  // While online, how the connection goes.
  route?: Route;
};

// An image sent or offered in the chat (the message id is the transfer's).
// path: ours, or theirs once received; state: how the transfer ended.
export type ChatFile = { name: string; size: number; path?: string; state?: TransferStatus };

export type ChatMessage = {
  id: string;
  from: string;
  text: string;
  ts: number;
  status?: 'pending' | 'delivered' | 'read';
  file?: ChatFile;
  // The message it answers: its text cut to 120 characters (an image: its name).
  reply?: { id: string; from: string; text: string };
  // One emoji per person, by their id.
  reactions?: Record<string, string>;
};

// The reactions offered (the backend accepts only these).
export const REACTIONS = ['👍', '❤️', '😂', '😮', '😢', '🙏'];

export type TransferStatus =
  | 'connecting' | 'waiting_consent' | 'pending' | 'sending' | 'receiving'
  | 'paused' | 'finishing' | 'completed' | 'declined' | 'cancelled' | 'error';

// A folder sent as one transfer per file; root: where it is saved here.
export type TransferFolder = { id: string; name: string; files: number; size: number; root?: string };

export type Route = 'lan' | 'direct' | 'relay';

// A newer version of the app; failed: its install failed already (the old
// version restarted), it only installs when asked.
export type UpdateInfo = { version: string; notes?: string; failed: boolean };

export type Transfer = {
  id: string;
  dir: 'in' | 'out';
  friendId: string;
  fileName: string;
  fileSize: number;
  bytes: number;
  progress: number;
  speed: number;
  status: TransferStatus;
  error?: string;
  savedPath?: string;
  // Sender outside the P2P network (LocalSend).
  peerName?: string;
  // Opening it may run code (program, script, shortcut…).
  executable: boolean;
  // Its bytes go over the local network, not the internet.
  lan: boolean;
  // While it runs, how its bytes go: local network, QUIC straight to the
  // peer, or through a relay.
  route?: Route;
  // A group's shared file: not paused by hand.
  group: boolean;
  // Paused by hand, here or by the other side.
  pausedBy?: 'me' | 'peer';
  // Incoming, accepted without asking: waits its turn (still pending).
  queued: boolean;
  // Its path within its folder, starting with the folder's name.
  relPath?: string;
  folder?: TransferFolder;
};

export type Config = {
  downloadPath: string;
  autoLaunch: boolean;
  // Android only: reachable in the background (a foreground service).
  backgroundService?: boolean;
  nickname: string;
  relayUrl: string;
  relayToken: string;
  // Desktop: newer versions install by themselves.
  autoUpdate?: boolean;
  // The first-launch welcome was shown.
  welcomed: boolean;
  // Do not disturb until then (unix ms); 0 off, from DND_FOREVER until turned off.
  dndUntil: number;
  // "Vu" and typing, both ways.
  readReceipts: boolean;
  // Bytes per second each way, 0 for none.
  speedLimit: number;
  notifications: NotifySettings;
};

// Which system notifications show; groups keep their own mute.
export type NotifySettings = {
  enabled: boolean;
  messages: boolean;
  files: boolean;
  groups: boolean;
  mutedFriends: string[];
  mutedLocalsend: string[];
};

export type RelayStatus = { url: string; custom: boolean; connected: boolean; error?: string };

export type LocalSendDevice = {
  fingerprint: string;
  alias: string;
  deviceModel?: string;
  deviceType?: 'mobile' | 'desktop' | 'web' | 'headless' | 'server';
  // HTTPS to the device holding this fingerprint; otherwise unencrypted.
  secure: boolean;
};

// A message of a LocalSend device's chat. unverified: theirs, from an older
// LocalSend that proves nothing about who sent it.
export type LocalSendMessage = {
  id: string;
  mine: boolean;
  text: string;
  ts: number;
  status?: 'pending' | 'delivered' | 'failed';
  error?: string;
  unverified: boolean;
};

export type LocalSendChat = { fingerprint: string; alias: string; unread: number; lastText: string; lastTs: number };

export type LocalSendStatus =
  | { running: true; status: { port: number; alias: string; fingerprint: string; error?: string } }
  | { running: false; error?: string };

export type GroupSummary = {
  id: string;
  name: string;
  members: number;
  online: number;
  unread: number;
  lastMessage: string | null;
  lastTs: number;
  syncing: boolean;
  removed: boolean;
  muted: boolean;
  // Unread messages that mention us (or everyone).
  unreadMentions: number;
};

export type GroupInvite = { gid: string; name: string; from: string; fromName: string };

export type GroupsOverview = { groups: GroupSummary[]; invites: GroupInvite[] };

// removable: we created the group or added them. handle: what follows "@"
// to mention them (it may hold spaces).
export type GroupMember = {
  id: string;
  name: string;
  online: boolean;
  friend: boolean;
  me: boolean;
  removable: boolean;
  handle: string;
};

// A pinned message or file; ts: when it was sent or shared.
export type GroupPin = {
  target: string;
  kind: 'message' | 'file';
  text: string;
  authorName: string;
  ts: number;
  pinnedBy: string;
  pinnedByName: string;
  pinnedAt: number;
};

// pins: the most recently pinned first.
export type GroupDetail = {
  id: string;
  name: string;
  members: GroupMember[];
  syncing: boolean;
  removed: boolean;
  muted: boolean;
  pins: GroupPin[];
};

export type GroupMessage = {
  id: string;
  author: string;
  authorName: string;
  text: string;
  ts: number;
  mine: boolean;
  receivedBy: number;
  total: number;
  // It mentions us or everyone (never our own).
  mentionsMe: boolean;
  pinned: boolean;
};

export type GroupFile = {
  id: string;
  name: string;
  // Folder within what was shared ("Photos/2024").
  dir: string | null;
  size: number;
  author: string;
  authorName: string;
  ts: number;
  mine: boolean;
  local: boolean;
  transferId: string | null;
  pinned: boolean;
};

export type OpenChat = { kind: 'friend' | 'group' | 'localsend'; id: string };

// Why a folder was not sent.
export type FolderError = 'empty' | 'too_many' | 'too_deep' | 'not_found';

// Do not disturb until turned off (the backend keeps anything above as such).
export const DND_FOREVER = Number.MAX_SAFE_INTEGER;

type Unsubscribe = () => void;

// Subscribe to a backend event; returns an unsubscribe function usable
// synchronously (e.g. in a React effect cleanup). onReady runs once the
// listener is registered: events emitted before it are not received.
const on = <T>(event: string) => (callback: (data: T) => void, onReady?: () => void): Unsubscribe => {
  let stop: Unsubscribe | null = null;
  let stopped = false;
  listen<T>(event, e => callback(e.payload))
    .then(unlisten => {
      if (stopped) unlisten();
      else {
        stop = unlisten;
        onReady?.();
      }
    })
    .catch(e => console.error(`listen ${event}:`, e));
  return () => {
    stopped = true;
    stop?.();
  };
};

export const api = {
  hideWindow: () => { invoke('hide_window'); },
  // The window stays open when it loses the focus (it goes away otherwise).
  setWindowPinned: (pinned: boolean) => invoke<boolean>('set_window_pinned', { pinned }),
  getWindowPinned: () => invoke<boolean>('get_window_pinned'),
  getMyId: () => invoke<string>('get_my_id'),
  getFriends: () => invoke<Friend[]>('get_friends'),
  addFriend: (id: string, name: string) =>
    invoke<{ friends: Friend[] } | { error: 'invalid_id' | 'self' }>('add_friend', { id, name }),
  removeFriend: (id: string) => invoke<Friend[]>('remove_friend', { id }),
  // An empty name shows the friend's own nickname.
  renameFriend: (id: string, name: string) => invoke<Friend[]>('rename_friend', { id, name }),
  setFriendAutoAccept: (id: string, enable: boolean) => invoke<Friend[]>('set_friend_auto_accept', { id, enable }),
  getConfig: () => invoke<Config>('get_config'),
  setAutoLaunch: (enable: boolean) => invoke<boolean>('set_auto_launch', { enable }),
  // Android: a service keeps the app reachable once closed; the value kept.
  setBackgroundService: (enable: boolean) => invoke<boolean>('set_background_service', { enable }),
  selectDownloadDir: () => invoke<string | null>('select_download_dir'),
  finishWelcome: () => { invoke('finish_welcome'); },
  // A friend's nickname came: their name may have changed.
  onFriendsChanged: on<null>('friends-changed'),

  // Files
  selectAndSendFile: (friendId: string) => invoke<number>('send_file', { friendId }),
  // Picks a folder, then sends it whole. "unsupported": not on this platform
  // (Android); the folder actions then go away.
  sendFolder: (friendId: string) => invoke<{ sent: number } | { error: FolderError | 'unsupported' }>('send_folder', { friendId })
    .catch((e): { error: 'unsupported' } => {
      if (String(e).includes('unsupported')) return { error: 'unsupported' };
      throw e;
    })
    .then(res => {
      if ('error' in res && res.error === 'unsupported') markFoldersUnsupported();
      return res;
    }),
  // Files and folders dropped on a friend; errors: why folders were skipped.
  sendPaths: (friendId: string, paths: string[]) =>
    invoke<{ sent: number; skipped: number; errors: FolderError[] }>('send_paths', { friendId, paths }),
  getTransfers: () => invoke<Transfer[]>('get_transfers'),
  clearFinished: () => { invoke('clear_finished'); },
  respondToFileOffer: (id: string, accept: boolean) => { invoke('respond_to_file_offer', { id, accept }); },
  // All the offers of a folder; resolves to how many.
  respondToFolder: (friendId: string, folderId: string, accept: boolean) =>
    invoke<number>('respond_to_folder', { friendId, folderId, accept }),
  cancelTransfer: (id: string) => { invoke('cancel_transfer', { id }); },
  pauseTransfer: (id: string) => invoke<boolean>('pause_transfer', { id }),
  resumeTransfer: (id: string) => invoke<boolean>('resume_transfer', { id }),
  // Bytes per second each way; null for none.
  setSpeedLimit: (limit: number | null) => invoke<number | null>('set_speed_limit', { limit }),
  showInFolder: (id: string) => { invoke('show_in_folder', { id }); },
  // Opens a received file with its app (never one that runs code).
  openTransfer: (id: string) => { invoke('open_transfer', { id }); },
  onTransferUpdate: on<Transfer>('transfer-update'),

  // Chat
  getMessages: (friendId: string) => invoke<ChatMessage[]>('get_messages', { friendId }),
  sendMessage: (friendId: string, text: string, replyTo?: string) =>
    invoke<ChatMessage | { error: string }>('send_message', { friendId, text, replyTo }),
  // The same emoji again, or null, takes ours back.
  reactMessage: (friendId: string, id: string, emoji: string | null) =>
    invoke<{ reactions: Record<string, string> } | { error: string }>('react_message', { friendId, id, emoji }),
  // The chat is on screen, the window focused.
  markChatRead: (friendId: string) => { invoke('mark_chat_read', { friendId }); },
  // We are typing to them (at most every 3 s).
  chatTyping: (friendId: string) => { invoke('chat_typing', { friendId }); },
  setReadReceipts: (enable: boolean) => invoke<boolean>('set_read_receipts', { enable }),
  setNotifications: (enabled: boolean, messages: boolean, files: boolean, groups: boolean) =>
    invoke<NotifySettings>('set_notifications', { enabled, messages, files, groups }),
  // One friend's or LocalSend device's chat without notifications.
  setChatMuted: (kind: 'friend' | 'localsend', id: string, muted: boolean) =>
    invoke<NotifySettings>('set_chat_muted', { kind, id, muted }),
  onNewMessage: on<{ friendId: string; message: ChatMessage }>('new-message'),
  onMessageStatus: on<{ friendId: string; id: string; status: ChatMessage['status'] }>('message-status'),
  // Our messages the friend has seen.
  onMessagesRead: on<{ friendId: string; ids: string[] }>('messages-read'),
  onMessageReactions: on<{ friendId: string; id: string; reactions: Record<string, string> }>('message-reactions'),
  onFriendTyping: on<{ friendId: string }>('friend-typing'),
  onOpenChat: on<OpenChat>('open-chat'),
  // A chat image's preview as a data URL, null if it cannot be read.
  chatImage: (friendId: string, id: string) => invoke<string | null>('chat_image', { friendId, id }),
  openChatFile: (friendId: string, id: string) => { invoke('open_chat_file', { friendId, id }); },
  // Web links only, in the system's browser.
  openUrl: (url: string) => { invoke('open_url', { url }); },
  // Chat requested by a notification click before this window could listen.
  takeOpenChat: () => invoke<OpenChat | null>('take_open_chat'),
  setNickname: (nickname: string) => invoke<string>('set_nickname', { nickname }),
  getRelayStatus: () => invoke<RelayStatus | null>('get_relay_status'),
  // The relay status changed (same payload as getRelayStatus).
  onRelayStatus: on<RelayStatus | null>('relay-status'),
  // Updates (desktop; on Android nothing is found, see Update.tsx).
  getUpdate: () => invoke<UpdateInfo | null>('get_update'),
  // Errors are French messages.
  checkUpdate: () => invoke<UpdateInfo | null>('check_update'),
  // Saves everything, installs and restarts; rejects with a French message.
  installUpdate: () => invoke<void>('install_update'),
  // Install newer versions by themselves, once no transfer runs.
  setAutoUpdate: (enable: boolean) => invoke<boolean>('set_auto_update', { enable }),
  onUpdateAvailable: on<UpdateInfo>('update-available'),
  onUpdateProgress: on<{ done: number; total: number | null }>('update-progress'),
  // A download failed (French message), also when it started by itself.
  onUpdateError: on<string>('update-error'),
  // Restarts the app when the setting changes.
  setRelay: (url: string, token: string) => invoke<void>('set_relay', { url, token }),
  getLocalSendStatus: () => invoke<LocalSendStatus>('get_localsend_status'),
  getLocalSendChats: () => invoke<LocalSendChat[]>('get_localsend_chats'),
  getLocalSendChat: (fingerprint: string) =>
    invoke<{ alias: string; messages: LocalSendMessage[] }>('get_localsend_chat', { fingerprint }),
  markLocalSendRead: (fingerprint: string) => { invoke('mark_localsend_read', { fingerprint }); },
  // Resolves to the message, shown as pending until the device has it.
  sendLocalSendText: (fingerprint: string, text: string) =>
    invoke<LocalSendMessage>('send_localsend_text', { fingerprint, text }),
  // A message arrived, or one of ours changed status.
  onLocalSendChat: on<{ fingerprint: string; alias: string; message: LocalSendMessage }>('localsend-chat'),
  getLocalSendDevices: () => invoke<LocalSendDevice[]>('get_localsend_devices'),
  // Looks again; the list changes through onLocalSendDevices.
  scanLocalSend: () => { invoke('scan_localsend'); },
  onLocalSendDevices: on<null>('localsend-devices'),
  // Picks the files, then sends them; resolves to how many.
  sendToLocalSend: (fingerprint: string) => invoke<number>('send_localsend_files', { fingerprint }),
  // Files dropped on a device; folders are skipped.
  sendPathsToLocalSend: (fingerprint: string, paths: string[]) =>
    invoke<{ sent: number; skipped: number }>('send_localsend_paths', { fingerprint, paths }),

  // Groups
  getGroups: () => invoke<GroupsOverview>('get_groups'),
  createGroup: (name: string, members: string[]) =>
    invoke<{ id: string } | { error: string }>('create_group', { name, members }),
  respondToGroupInvite: (gid: string, accept: boolean) => invoke<void>('respond_to_group_invite', { gid, accept }),
  getGroup: (gid: string) => invoke<GroupDetail | null>('get_group', { gid }),
  getGroupMessages: (gid: string) => invoke<GroupMessage[]>('get_group_messages', { gid }),
  sendGroupMessage: (gid: string, text: string) =>
    invoke<{ ok: true } | { error: string }>('send_group_message', { gid, text }),
  addGroupMembers: (gid: string, members: string[]) =>
    invoke<{ ok: true } | { error: string }>('add_group_members', { gid, members }),
  removeGroupMember: (gid: string, member: string) => invoke<void>('remove_group_member', { gid, member }),
  leaveGroup: (gid: string) => invoke<void>('leave_group', { gid }),
  forgetGroup: (gid: string) => invoke<void>('forget_group', { gid }),
  renameGroup: (gid: string, name: string) => invoke<void>('rename_group', { gid, name }),
  markGroupRead: (gid: string) => invoke<void>('mark_group_read', { gid }),
  setGroupMuted: (gid: string, muted: boolean) => invoke<void>('set_group_muted', { gid, muted }),
  getGroupFiles: (gid: string) => invoke<GroupFile[]>('get_group_files', { gid }),
  addGroupFiles: (gid: string) => invoke<number>('add_group_files', { gid }),
  // Files and folders dropped on a group: resolves to how many files were added.
  addGroupPaths: (gid: string, paths: string[]) => invoke<number>('add_group_paths', { gid, paths }),
  downloadGroupFile: (gid: string, fileId: string) =>
    invoke<{ id: string } | { error: string }>('download_group_file', { gid, fileId }),
  deleteGroupFile: (gid: string, fileId: string) => invoke<void>('delete_group_file', { gid, fileId }),
  showGroupFile: (gid: string, fileId: string) => invoke<void>('show_group_file', { gid, fileId }),
  // target: a message's or a file's id.
  pinGroupItem: (gid: string, target: string) =>
    invoke<{ ok: true } | { error: string }>('pin_group_item', { gid, target }),
  unpinGroupItem: (gid: string, target: string) =>
    invoke<{ ok: true } | { error: string }>('unpin_group_item', { gid, target }),
  onGroupUpdate: on<{ gid: string }>('group-update'),

  // Backup: asks where to save it, resolves to the path (null if cancelled).
  // Errors are French messages.
  exportBackup: (password: string, includeChats: boolean) =>
    invoke<string | null>('export_backup', { password, includeChats }),
  // False if cancelled; otherwise the app restarts on it.
  importBackup: (password: string) => invoke<boolean>('import_backup', { password }),

  // Do not disturb until then (unix ms, 0 off, DND_FOREVER): the value kept.
  setDnd: (until: number) => invoke<number>('set_dnd', { until }),
  onDndChanged: on<{ dndUntil: number }>('dnd-changed'),

  // UI / presence
  onFriendStatus: on<{ friendId: string; online: boolean }>('friend-status'),
  onFriendRoute: on<{ friendId: string; route: Route | null }>('friend-route'),

  // Files dragged over the window, from the file explorer. Positions are in
  // physical pixels from the window's top left.
  onDragDrop: (callback: (e: DragDrop) => void): Unsubscribe => {
    let stop: Unsubscribe | null = null;
    let stopped = false;
    getCurrentWebview()
      .onDragDropEvent(e => {
        const p = e.payload;
        if (p.type === 'leave') callback({ type: 'leave' });
        else callback({ type: p.type, x: p.position.x, y: p.position.y, paths: 'paths' in p ? p.paths : [] });
      })
      .then(unlisten => {
        if (stopped) unlisten();
        else stop = unlisten;
      })
      .catch(e => console.error('drag-drop:', e));
    return () => {
      stopped = true;
      stop?.();
    };
  },
};

// Where dropped files would go.
export type DropTarget = { kind: 'friend' | 'group' | 'localsend'; id: string; name: string };

export type DragDrop =
  | { type: 'enter' | 'over' | 'drop'; x: number; y: number; paths: string[] }
  | { type: 'leave' };
