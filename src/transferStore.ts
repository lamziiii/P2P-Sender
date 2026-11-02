import { useSyncExternalStore } from 'react';
import type { Route, Transfer } from './api';
import { ACTIVE, FINAL } from './format';

// The transfer list, outside React state: progress updates come often, and
// only what shows transfers renders again.
let list: Transfer[] = [];
const listeners = new Set<() => void>();

export const transferStore = {
  get: () => list,
  update: (change: (prev: Transfer[]) => Transfer[]) => {
    const next = change(list);
    if (next === list) return;
    list = next;
    listeners.forEach(listener => listener());
  },
  subscribe: (listener: () => void) => {
    listeners.add(listener);
    return () => { listeners.delete(listener); };
  },
};

// A value made from the list, kept (same reference) while `equal` finds it unchanged.
const derived = <T>(select: (list: Transfer[]) => T, equal: (a: T, b: T) => boolean) => {
  let from: Transfer[] | null = null;
  let value: T;
  return () => {
    if (from !== list) {
      const next = select(list);
      if (from === null || !equal(value, next)) value = next;
      from = list;
    }
    return value;
  };
};

const sameItems = <T>(a: T[], b: T[]) => a.length === b.length && a.every((x, i) => x === b[i]);

// Offers waiting for an answer (not those accepted, waiting their turn):
// they do not change with others' progress.
const offers = derived(l => l.filter(t => t.dir === 'in' && t.status === 'pending' && !t.queued), sameItems);

// `route`: how the running ones go, when all the same way.
type Running = { count: number; active: boolean; progress: number; route?: Route };
const running = derived<Running>(l => {
  const list = l.filter(t => (t.status !== 'pending' || t.queued) && !FINAL.has(t.status));
  const size = list.reduce((n, t) => n + t.fileSize, 0);
  const bytes = list.reduce((n, t) => n + t.bytes, 0);
  const routes = new Set(list.filter(t => ACTIVE.has(t.status)).map(t => t.route));
  return {
    count: list.length,
    active: list.some(t => ACTIVE.has(t.status)),
    progress: size > 0 ? Math.floor((bytes * 100) / size) : 0,
    route: routes.size === 1 ? [...routes][0] : undefined,
  };
}, (a, b) => a.count === b.count && a.active === b.active && a.progress === b.progress && a.route === b.route);

export const useTransfers = () => useSyncExternalStore(transferStore.subscribe, transferStore.get);
export const useOffers = () => useSyncExternalStore(transferStore.subscribe, offers);
export const useRunning = () => useSyncExternalStore(transferStore.subscribe, running);
export const useTransfer = (id: string) =>
  useSyncExternalStore(transferStore.subscribe, () => list.find(t => t.id === id));
