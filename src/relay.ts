import { useEffect, useState } from 'react';
import { api } from './api';
import type { RelayStatus } from './api';

// How long the relay may take to answer at start before the network counts as blocked.
const CONNECT_GRACE = 15_000;

export type Connection = 'connecting' | 'online' | 'blocked';

// The relay status: asked once, then followed through its event.
export function useRelayStatus() {
  const [status, setStatus] = useState<RelayStatus | null>(null);
  useEffect(() => {
    // Once an event came, an older answer must not replace it.
    let fromEvent = false;
    const load = () => api.getRelayStatus().then(s => { if (!fromEvent) setStatus(s); }, () => {});
    load();
    // Asked again once listening: a change in between is not lost.
    return api.onRelayStatus(s => {
      fromEvent = true;
      setStatus(s);
    }, load);
  }, []);
  return status;
}

// Whether we reach the relays, which friends elsewhere than on our local
// network are reached through. With every relay blocked, iroh may never pick
// one: no status at all past the grace counts as blocked too.
export function useConnection(): { status: RelayStatus | null; connection: Connection } {
  const status = useRelayStatus();
  const [late, setLate] = useState(false);
  useEffect(() => {
    const grace = setTimeout(() => setLate(true), CONNECT_GRACE);
    return () => clearTimeout(grace);
  }, []);
  let connection: Connection = 'connecting';
  if (status?.connected) connection = 'online';
  else if (late || status?.error) connection = 'blocked';
  return { status, connection };
}
