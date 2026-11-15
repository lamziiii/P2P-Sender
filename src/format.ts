import type { Route } from './api';
import { locale, tr } from './i18n';

// Transfer statuses: moving bytes, and over.
export const ACTIVE = new Set(['sending', 'receiving']);
export const FINAL = new Set(['completed', 'declined', 'cancelled', 'error']);

// How a running transfer's bytes go, shown next to its speed.
export const ROUTE_LABELS: Record<Route, string> = tr(
  { lan: 'Réseau local', direct: 'Direct', relay: 'Via relais' },
  { lan: 'Local network', direct: 'Direct', relay: 'Via relay' },
);
export const RELAY_HINT = tr(
  'Par un serveur relais, plus lent qu’une connexion directe. Un relais personnel (Paramètres → Réseau) peut aider.',
  'Through a relay server, slower than a direct connection. A personal relay (Settings → Network) may help.',
);

const UNITS = tr(['o', 'Ko', 'Mo', 'Go'], ['B', 'KB', 'MB', 'GB']);

export const formatBytes = (n: number) => {
  if (n >= 1024 ** 3) return `${(n / 1024 ** 3).toFixed(2)} ${UNITS[3]}`;
  if (n >= 1024 ** 2) return `${(n / 1024 ** 2).toFixed(1)} ${UNITS[2]}`;
  if (n >= 1024) return `${(n / 1024).toFixed(0)} ${UNITS[1]}`;
  return `${n} ${UNITS[0]}`;
};

export const formatTime = (ts: number) =>
  new Date(ts).toLocaleTimeString(locale, { hour: '2-digit', minute: '2-digit' });

export const formatDate = (ts: number) => {
  const d = new Date(ts);
  const today = new Date();
  if (d.toDateString() === today.toDateString()) return formatTime(ts);
  return d.toLocaleDateString(locale, { day: 'numeric', month: 'short' });
};

// Day shown above the messages of that day in a chat.
export const formatDay = (ts: number) => {
  const d = new Date(ts);
  const today = new Date();
  const yesterday = new Date();
  yesterday.setDate(today.getDate() - 1);
  if (d.toDateString() === today.toDateString()) return tr('Aujourd’hui', 'Today');
  if (d.toDateString() === yesterday.toDateString()) return tr('Hier', 'Yesterday');
  const days = (today.getTime() - d.getTime()) / 86_400_000;
  if (days < 6) return d.toLocaleDateString(locale, { weekday: 'long' }).replace(/^./, c => c.toUpperCase());
  return d.toLocaleDateString(locale, {
    weekday: 'long',
    day: 'numeric',
    month: 'long',
    year: d.getFullYear() === today.getFullYear() ? undefined : 'numeric',
  }).replace(/^./, c => c.toUpperCase());
};

// A fixed color per person or group, to tell them apart at a glance
// (Fluent's persona colors, readable with white text).
const AVATAR_COLORS = ['#0078d4', '#8764b8', '#038387', '#ca5010', '#498205', '#c239b3', '#4f6bed', '#986f0b', '#d13438', '#00838f'];

export const avatarColor = (id: string) => {
  let h = 0;
  for (const c of id) h = (h * 31 + c.charCodeAt(0)) >>> 0;
  return AVATAR_COLORS[h % AVATAR_COLORS.length];
};

export const initial = (name: string) => name.trim()[0]?.toUpperCase() ?? '?';

// French counts 0 and 1 as one, English only 1.
export const plural = (n: number, one: string, many: string) => `${n} ${tr(n > 1, n !== 1) ? many : one}`;

// Why a folder was not sent.
const FOLDER_ERRORS: Record<string, string> = tr({
  empty: 'le dossier est vide',
  too_many: 'plus de 10 000 fichiers',
  too_deep: 'plus de 32 niveaux de sous-dossiers',
  not_found: 'dossier introuvable',
}, {
  empty: 'the folder is empty',
  too_many: 'more than 10,000 files',
  too_deep: 'more than 32 levels of subfolders',
  not_found: 'folder not found',
});

// What failed sending folders, in one sentence.
export const folderErrors = (errors: string[]) =>
  `${tr('Dossier non envoyé : ', 'Folder not sent: ')}${[...new Set(errors)].map(e => FOLDER_ERRORS[e] ?? e).join(', ')}.`;

// Why the folder picked was not sent (the platform may not send folders).
export const folderSendError = (error: string) =>
  (error === 'unsupported' ? tr('Envoi de dossiers non disponible sur Android.', 'Sending folders is not available on Android.') : folderErrors([error]));
