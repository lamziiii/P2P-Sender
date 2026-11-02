import type { ReactNode } from 'react';
import { formatDay } from './format';

// Messages of one sender this close together form a run: the time shows
// once, under the last of them.
const RUN_GAP = 5 * 60_000;

// Lays out a chat: a divider above each day, and runs of messages from the
// same sender drawn close together.
export function chatRows<T>(items: T[], { ts, sender, key, render }: {
  ts: (item: T) => number;
  sender: (item: T) => string;
  key: (item: T) => string;
  // cont: follows the previous one in a run; last: ends its run.
  render: (item: T, run: { cont: boolean; last: boolean }) => ReactNode;
}): ReactNode[] {
  const rows: ReactNode[] = [];
  const sameRun = (a: T | undefined, b: T | undefined) =>
    !!a && !!b
    && sender(a) === sender(b)
    && ts(b) - ts(a) < RUN_GAP
    && new Date(ts(a)).toDateString() === new Date(ts(b)).toDateString();
  items.forEach((item, i) => {
    const prev = items[i - 1];
    const day = new Date(ts(item)).toDateString();
    if (!prev || new Date(ts(prev)).toDateString() !== day) {
      rows.push(<div key={`day-${key(item)}`} className="day-divider"><span>{formatDay(ts(item))}</span></div>);
    }
    rows.push(render(item, { cont: sameRun(prev, item), last: !sameRun(item, items[i + 1]) }));
  });
  return rows;
}
