import { useEffect } from 'react';

// A menu or picker closes on a click elsewhere or Escape. close must keep
// its identity; the click that opens it must not reach the window.
export function useDismiss(active: boolean, close: () => void) {
  useEffect(() => {
    if (!active) return;
    const onKey = (e: KeyboardEvent) => e.key === 'Escape' && close();
    window.addEventListener('click', close);
    window.addEventListener('keydown', onKey);
    return () => {
      window.removeEventListener('click', close);
      window.removeEventListener('keydown', onKey);
    };
  }, [active, close]);
}

// Brings a loaded message into view (element id "msg-<id>"), briefly
// marked; false if it is not loaded.
export const jumpTo = (id: string) => {
  const el = document.getElementById(`msg-${id}`);
  if (!el) return false;
  el.scrollIntoView({ block: 'center', behavior: 'smooth' });
  el.classList.remove('flash');
  // A layout in between restarts the animation.
  el.getBoundingClientRect();
  el.classList.add('flash');
  return true;
};
