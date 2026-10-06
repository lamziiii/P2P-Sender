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

export type Config = { downloadPath: string; autoLaunch: boolean };

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
  onOpenChat: on<string>('open-chat'),
  // Chat requested by a notification click before this window existed.
  takeOpenChat: () => invoke<string | null>('take_open_chat'),

  // UI / presence
  onSwitchTab: on<string>('switch-tab'),
  onFriendStatus: on<{ friendId: string; online: boolean }>('friend-status'),
};
