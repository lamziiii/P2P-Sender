import { useLayoutEffect, useRef } from 'react';
import type { KeyboardEvent, ReactNode, RefObject } from 'react';
import { Icon } from './icons';

// The input grows up to this many lines, then scrolls.
const MAX_LINES = 5;

// Shown on our message until the other side has it.
export const PendingMark = () => (
  <span className="pending-mark" title="En attente de livraison"><Icon name="clock" size={11} /></span>
);

// Message input: Enter sends, Shift+Enter starts a new line; it grows with
// its text.
export function ChatInput({ value, onChange, onSend, placeholder, disabled, maxLength, error, before, popup, onKey, inputRef, onPasteFiles }: {
  value: string;
  onChange: (value: string) => void;
  onSend: () => void;
  placeholder: string;
  disabled?: boolean;
  maxLength: number;
  error?: string;
  // Buttons before the field (attach a file…).
  before?: ReactNode;
  // Shown above the field (mention suggestions…).
  popup?: ReactNode;
  // Sees the keys first; true when it handled the key.
  onKey?: (e: KeyboardEvent<HTMLTextAreaElement>) => boolean;
  inputRef?: RefObject<HTMLTextAreaElement | null>;
  // Files pasted (a screenshot, files copied in the explorer): sent.
  onPasteFiles?: (files: File[]) => void;
}) {
  const own = useRef<HTMLTextAreaElement>(null);
  const ref = inputRef ?? own;

  useLayoutEffect(() => {
    const el = ref.current;
    if (!el) return;
    const css = getComputedStyle(el);
    const line = parseFloat(css.lineHeight) || 19;
    const padding = parseFloat(css.paddingTop) + parseFloat(css.paddingBottom);
    const border = parseFloat(css.borderTopWidth) + parseFloat(css.borderBottomWidth);
    el.style.height = 'auto';
    // border-box: the height counts the border, scrollHeight does not.
    el.style.height = `${Math.min(el.scrollHeight, line * MAX_LINES + padding) + border}px`;
  }, [value, ref]);

  return (
    <div className="chat-input-row">
      {error && <div className="chat-error">{error}</div>}
      {popup}
      {before}
      <textarea
        ref={ref}
        className="input chat-textarea"
        rows={1}
        placeholder={placeholder}
        value={value}
        maxLength={maxLength}
        disabled={disabled}
        onChange={e => onChange(e.target.value)}
        onPaste={e => {
          const files = Array.from(e.clipboardData.files);
          if (!onPasteFiles || files.length === 0) return;
          e.preventDefault();
          onPasteFiles(files);
        }}
        onKeyDown={e => {
          if (onKey?.(e)) return;
          if (e.key === 'Enter' && !e.shiftKey && !e.nativeEvent.isComposing) {
            e.preventDefault();
            onSend();
          }
        }}
      />
      <button className="btn btn-icon" title="Envoyer" disabled={disabled} onClick={onSend}><Icon name="send" /></button>
    </div>
  );
}

// The search over a chat's loaded messages: the field, which match is shown
// out of how many, and previous / next.
export function ChatSearch({ query, onQuery, index, count, onStep, onClose }: {
  query: string;
  onQuery: (query: string) => void;
  index: number;
  count: number;
  // -1 older, 1 newer.
  onStep: (step: number) => void;
  onClose: () => void;
}) {
  return (
    <div className="chat-search">
      <div className="search-box">
        <Icon name="search" size={14} />
        <input
          className="input"
          autoFocus
          placeholder="Rechercher dans la discussion"
          value={query}
          onChange={e => onQuery(e.target.value)}
          onKeyDown={e => {
            if (e.key === 'Escape') {
              e.stopPropagation();
              onClose();
            } else if (e.key === 'Enter') {
              e.preventDefault();
              onStep(e.shiftKey ? 1 : -1);
            }
          }}
        />
      </div>
      <span className="search-count">{query.trim() ? (count > 0 ? `${index + 1}/${count}` : '0') : ''}</span>
      <button className="icon-btn small" title="Précédent (plus ancien)" disabled={count === 0} onClick={() => onStep(-1)}><Icon name="chevronUp" size={14} /></button>
      <button className="icon-btn small" title="Suivant (plus récent)" disabled={count === 0} onClick={() => onStep(1)}><Icon name="chevronDown" size={14} /></button>
      <button className="icon-btn small" title="Fermer la recherche" onClick={onClose}><Icon name="close" size={14} /></button>
    </div>
  );
}

// A message's actions, beside its bubble on hover (or while open: a picker
// is shown).
export function BubbleActions({ open, children }: { open?: boolean; children: ReactNode }) {
  return <div className={`bubble-actions ${open ? 'open' : ''}`}>{children}</div>;
}
