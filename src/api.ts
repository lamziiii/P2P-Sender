import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';

export type Friend = { id: string; name: string; online: boolean; legacy: boolean };

export type ChatMessage = { id: string; from: string; text: string; ts: number; status?: 'pending' | 'delivered' };

export type TransferStatus =
  | 'connecting' | 'waiting_consent' | 'pending' | 'sending' | 'receiving'
  | 'paused' | 'finishing' | 'completed' | 'declined' | 'cancelled' | 'error';

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
};

export type Config = { downloadPath: string; autoLaunch: boolean; nickname: string };

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
};

export type GroupInvite = { gid: string; name: string; from: string; fromName: string };

export type GroupsOverview = { groups: GroupSummary[]; invites: GroupInvite[] };

export type GroupMember = { id: string; name: string; online: boolean; friend: boolean; me: boolean };

export type GroupDetail = { id: string; name: string; members: GroupMember[]; syncing: boolean; removed: boolean };

export type GroupMessage = {
  id: string;
  author: string;
  authorName: string;
  text: string;
  ts: number;
  mine: boolean;
  receivedBy: number;
  total: number;
};

export type GroupFile = {
  id: string;
  name: string;
  size: number;
  author: string;
  authorName: string;
  ts: number;
  mine: boolean;
  local: boolean;
  transferId: string | null;
};

export type OpenChat = { kind: 'friend' | 'group'; id: string };

type Unsubscribe = () => void;

// Subscribe to a backend event; returns an unsubscribe function usable
// synchronously (e.g. in a React effect cleanup).
const on = <T>(event: string) => (callback: (data: T) => void): Unsubscribe => {
  let stop: Unsubscribe | null = null;
  let stopped = false;
  listen<T>(event, e => callback(e.payload)).then(unlisten => {
    if (stopped) unlisten();
    else stop = unlisten;
  });
  return () => {
    stopped = true;
    stop?.();
  };
};

export const api = {
  hideWindow: () => { invoke('hide_window'); },
  getMyId: () => invoke<string>('get_my_id'),
  getFriends: () => invoke<Friend[]>('get_friends'),
  addFriend: (id: string, name: string) =>
    invoke<{ friends: Friend[] } | { error: 'invalid_id' | 'self' | 'no_name' }>('add_friend', { id, name }),
  removeFriend: (id: string) => invoke<Friend[]>('remove_friend', { id }),
  getConfig: () => invoke<Config>('get_config'),
  setAutoLaunch: (enable: boolean) => invoke<boolean>('set_auto_launch', { enable }),
  selectDownloadDir: () => invoke<string | null>('select_download_dir'),

  // Files
  selectAndSendFile: (friendId: string) => invoke<number>('send_file', { friendId }),
  getTransfers: () => invoke<Transfer[]>('get_transfers'),
  clearFinished: () => { invoke('clear_finished'); },
  respondToFileOffer: (id: string, accept: boolean) => { invoke('respond_to_file_offer', { id, accept }); },
  cancelTransfer: (id: string) => { invoke('cancel_transfer', { id }); },
  showInFolder: (id: string) => { invoke('show_in_folder', { id }); },
  onTransferUpdate: on<Transfer>('transfer-update'),

  // Chat
  getMessages: (friendId: string) => invoke<ChatMessage[]>('get_messages', { friendId }),
  sendMessage: (friendId: string, text: string) =>
    invoke<ChatMessage | { error: string }>('send_message', { friendId, text }),
  onNewMessage: on<{ friendId: string; message: ChatMessage }>('new-message'),
  onMessageStatus: on<{ friendId: string; id: string; status: ChatMessage['status'] }>('message-status'),
  onOpenChat: on<OpenChat>('open-chat'),
  // Chat requested by a notification click before this window existed.
  takeOpenChat: () => invoke<OpenChat | null>('take_open_chat'),
  setNickname: (nickname: string) => invoke<string>('set_nickname', { nickname }),

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
  getGroupFiles: (gid: string) => invoke<GroupFile[]>('get_group_files', { gid }),
  addGroupFiles: (gid: string) => invoke<number>('add_group_files', { gid }),
  downloadGroupFile: (gid: string, fileId: string) =>
    invoke<{ id: string } | { error: string }>('download_group_file', { gid, fileId }),
  deleteGroupFile: (gid: string, fileId: string) => invoke<void>('delete_group_file', { gid, fileId }),
  showGroupFile: (gid: string, fileId: string) => invoke<void>('show_group_file', { gid, fileId }),
  onGroupUpdate: on<{ gid: string }>('group-update'),

  // UI / presence
  onSwitchTab: on<string>('switch-tab'),
  onFriendStatus: on<{ friendId: string; online: boolean }>('friend-status'),
};
