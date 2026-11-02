import type { Route } from './api';

// Transfer statuses: moving bytes, and over.
export const ACTIVE = new Set(['sending', 'receiving']);
export const FINAL = new Set(['completed', 'declined', 'cancelled', 'error']);

// How a running transfer's bytes go, shown next to its speed.
export const ROUTE_LABELS: Record<Route, string> = { lan: 'Réseau local', direct: 'Direct', relay: 'Via relais' };
export const RELAY_HINT =
  'Par un serveur relais, plus lent qu’une connexion directe. Un relais auto-hébergé (Paramètres → Avancé) peut aider.';

export const formatBytes = (n: number) => {
  if (n >= 1024 ** 3) return `${(n / 1024 ** 3).toFixed(2)} Go`;
  if (n >= 1024 ** 2) return `${(n / 1024 ** 2).toFixed(1)} Mo`;
  if (n >= 1024) return `${(n / 1024).toFixed(0)} Ko`;
  return `${n} o`;
};

export const formatTime = (ts: number) =>
  new Date(ts).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' });

export const formatDate = (ts: number) => {
  const d = new Date(ts);
  const today = new Date();
  if (d.toDateString() === today.toDateString()) return formatTime(ts);
  return d.toLocaleDateString([], { day: 'numeric', month: 'short' });
};

// Day shown above the messages of that day in a chat.
export const formatDay = (ts: number) => {
  const d = new Date(ts);
  const today = new Date();
  const yesterday = new Date();
  yesterday.setDate(today.getDate() - 1);
  if (d.toDateString() === today.toDateString()) return 'Aujourd’hui';
  if (d.toDateString() === yesterday.toDateString()) return 'Hier';
  const days = (today.getTime() - d.getTime()) / 86_400_000;
  if (days < 6) return d.toLocaleDateString([], { weekday: 'long' }).replace(/^./, c => c.toUpperCase());
  return d.toLocaleDateString([], {
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

export const plural = (n: number, one: string, many: string) => `${n} ${n > 1 ? many : one}`;

// Why a folder was not sent.
export const FOLDER_ERRORS: Record<string, string> = {
  empty: 'le dossier est vide',
  too_many: 'plus de 10 000 fichiers',
  too_deep: 'plus de 32 niveaux de sous-dossiers',
  not_found: 'dossier introuvable',
};

// What failed sending folders, in one sentence.
export const folderErrors = (errors: string[]) =>
  `Dossier non envoyé : ${[...new Set(errors)].map(e => FOLDER_ERRORS[e] ?? e).join(', ')}.`;

// Why the folder picked was not sent (the platform may not send folders).
export const folderSendError = (error: string) =>
  (error === 'unsupported' ? 'Envoi de dossiers non disponible sur Android.' : folderErrors([error]));
