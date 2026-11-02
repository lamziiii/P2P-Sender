import { memo } from 'react';
import type { ReactNode } from 'react';
import { api } from './api';

const URL_RE = /\bhttps?:\/\/[^\s<>"]+/gi;
// Punctuation after a link usually ends the sentence, not the link.
const TRAILING = /[.,;:!?'"»\]}]+$/;

const escape = (s: string) => s.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');

// Where a link found in the text really ends: a closing parenthesis stays
// when the link opened one (Wikipedia-style addresses).
const trimLink = (url: string) => {
  let end = url.replace(TRAILING, '');
  while (end.endsWith(')') && (end.match(/\(/g)?.length ?? 0) < (end.match(/\)/g)?.length ?? 0)) {
    end = end.slice(0, -1).replace(TRAILING, '');
  }
  return end;
};

// Mentions ("@" + a member's handle, the longest first) and the searched
// text, marked in a piece of text without links.
const marked = (text: string, at: number, highlight?: string, mentions?: string[]): ReactNode[] => {
  const patterns = [
    ...[...(mentions ?? [])].sort((a, b) => b.length - a.length).map(h => escape(`@${h}`)),
    ...(highlight ? [escape(highlight)] : []),
  ];
  if (patterns.length === 0) return [text];
  const re = new RegExp(patterns.join('|'), 'gi');
  const mentionSet = new Set((mentions ?? []).map(h => `@${h}`.toLowerCase()));
  const parts: ReactNode[] = [];
  let last = 0;
  for (const match of text.matchAll(re)) {
    if (match[0] === '') continue;
    const start = match.index;
    if (start > last) parts.push(text.slice(last, start));
    parts.push(mentionSet.has(match[0].toLowerCase())
      ? <span key={at + start} className="mention">{match[0]}</span>
      : <mark key={at + start} className="search-mark">{match[0]}</mark>);
    last = start + match[0].length;
  }
  if (last < text.length) parts.push(text.slice(last));
  return parts;
};

// A message's text, with its web links clickable (opened in the browser,
// never in the app's window), and optionally the searched text and
// mentions marked.
export const RichText = memo(function RichText({ text, highlight, mentions }: {
  text: string;
  highlight?: string;
  // Handles of the group's members, "tous" and "all" included.
  mentions?: string[];
}) {
  const parts: ReactNode[] = [];
  let last = 0;
  for (const match of text.matchAll(URL_RE)) {
    const url = trimLink(match[0]);
    const start = match.index;
    if (start > last) parts.push(...marked(text.slice(last, start), last, highlight, mentions));
    parts.push(
      <span
        key={`url-${start}`}
        role="link"
        tabIndex={0}
        className="chat-link"
        title={url}
        onClick={() => api.openUrl(url)}
        onKeyDown={e => e.key === 'Enter' && api.openUrl(url)}
      >
        {url}
      </span>
    );
    last = start + url.length;
  }
  if (last < text.length) parts.push(...marked(text.slice(last), last, highlight, mentions));
  return <>{parts}</>;
});
