import { useEffect, useState } from 'react';
import { api } from './api';
import type { RelayStatus } from './api';

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
