import { useEffect, useRef, useSyncExternalStore } from 'react';
import type { MouseEvent, TouchEvent } from 'react';

// A phone or tablet (the Android app): full screen, touch, no tray window.
// Everything that differs from the desktop flyout depends on this.
export const isMobile = /Android|iPhone|iPad/i.test(navigator.userAgent);

// The same sentence for the desktop or the phone.
export const byPlatform = <T>(desktop: T, mobile: T): T => (isMobile ? mobile : desktop);

// Where received files go on Android (no folder to pick).
export const MOBILE_DOWNLOADS = 'Téléchargements/P2PShare';

if (isMobile) {
  const root = document.documentElement;
  root.classList.add('mobile');
  // The height left by the keyboard: the chat input stays above it.
  const viewport = window.visualViewport;
  const fit = () => {
    root.style.setProperty('--app-height', `${viewport ? viewport.height : window.innerHeight}px`);
    // The page itself never scrolls (the browser may push it up for the keyboard).
    if (window.scrollY !== 0) window.scrollTo(0, 0);
    // Typing: the last messages stay in view.
    if (document.activeElement?.classList.contains('chat-textarea')) {
      const list = document.querySelector('.chat-messages');
      if (list) list.scrollTop = list.scrollHeight;
    }
  };
  fit();
  viewport?.addEventListener('resize', fit);
  window.addEventListener('resize', fit);
}

/* ─── Folder sending (not available on every platform) ─── */

let foldersSupported = true;
const folderListeners = new Set<() => void>();

// The backend answered "unsupported" to a folder: its actions go away.
export const markFoldersUnsupported = () => {
  if (!foldersSupported) return;
  foldersSupported = false;
  folderListeners.forEach(l => l());
};

export const useFoldersSupported = () => useSyncExternalStore(
  listener => {
    folderListeners.add(listener);
    return () => { folderListeners.delete(listener); };
  },
  () => foldersSupported,
);

/* ─── Touch ─── */

const LONG_PRESS = 450;
// A finger moving more than this scrolls: no long press.
const PRESS_SLOP = 10;

// On mobile, a long press (or the context menu) calls onLong: it opens what
// the mouse shows on hover. Nothing on the desktop.
export function useLongPress(onLong: () => void) {
  const timer = useRef<ReturnType<typeof setTimeout>>(undefined);
  const fired = useRef(false);
  const start = useRef({ x: 0, y: 0 });
  useEffect(() => () => clearTimeout(timer.current), []);
  if (!isMobile) return undefined;
  return {
    onTouchStart: (e: TouchEvent) => {
      const t = e.touches[0];
      fired.current = false;
      start.current = { x: t.clientX, y: t.clientY };
      clearTimeout(timer.current);
      timer.current = setTimeout(() => {
        fired.current = true;
        navigator.vibrate?.(10);
        onLong();
      }, LONG_PRESS);
    },
    onTouchMove: (e: TouchEvent) => {
      const t = e.touches[0];
      if (Math.hypot(t.clientX - start.current.x, t.clientY - start.current.y) > PRESS_SLOP) clearTimeout(timer.current);
    },
    onTouchEnd: (e: TouchEvent) => {
      clearTimeout(timer.current);
      // The click that follows would close what just opened.
      if (fired.current) e.preventDefault();
    },
    onTouchCancel: () => clearTimeout(timer.current),
    onContextMenu: (e: MouseEvent) => {
      e.preventDefault();
      onLong();
    },
  };
}

// A menu placed under its button stays on the screen (mobile only).
export const fitOnScreen = (el: HTMLElement | null) => {
  if (!isMobile || !el) return;
  const margin = 8;
  const r = el.getBoundingClientRect();
  if (r.bottom > window.innerHeight - margin) el.style.top = `${Math.max(margin, window.innerHeight - r.height - margin)}px`;
  if (r.left < margin) {
    el.style.left = `${margin}px`;
    el.style.right = 'auto';
  }
};

/* ─── Android back button ─── */

// Sub-views get a history entry: the back button pops it, which goes back
// like Escape. Dialogs and menus close first.
const OVERLAYS = '.modal-backdrop, .menu-popover, .reaction-picker, .bubble-actions.open';

export function useBackButton(inSubView: boolean) {
  const subRef = useRef(inSubView);
  useEffect(() => {
    subRef.current = inSubView;
    if (!isMobile) return;
    const marked = (history.state as { sub?: boolean } | null)?.sub === true;
    if (inSubView && !marked) history.pushState({ sub: true }, '');
    // Left by its own back button: its entry goes too.
    else if (!inSubView && marked) history.back();
  }, [inSubView]);

  useEffect(() => {
    if (!isMobile) return;
    const escape = () => {
      const target = document.activeElement instanceof HTMLElement ? document.activeElement : document.body;
      target.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true, cancelable: true }));
    };
    let resync: ReturnType<typeof setTimeout> | undefined;
    const onPop = () => {
      if (document.querySelector(OVERLAYS)) {
        escape();
        // A dialog whose field did not have the focus: as a click outside it.
        document.querySelector<HTMLElement>('.modal-backdrop')?.click();
      } else if (subRef.current) escape();
      else return;
      // Still in a sub-view once it rendered (a menu closed, a reply
      // cancelled): the next back needs its entry again.
      clearTimeout(resync);
      resync = setTimeout(() => {
        if (subRef.current && (history.state as { sub?: boolean } | null)?.sub !== true) history.pushState({ sub: true }, '');
      }, 50);
    };
    window.addEventListener('popstate', onPop);
    return () => {
      clearTimeout(resync);
      window.removeEventListener('popstate', onPop);
    };
  }, []);
}
