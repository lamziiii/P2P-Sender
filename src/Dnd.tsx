import { useCallback, useState } from 'react';
import { api, DND_FOREVER } from './api';
import { formatTime } from './format';
import { Icon } from './icons';
import { useDismiss } from './ui';

const HOUR = 3_600_000;

// The next 8 o'clock: tomorrow's, unless it is not yet 8 today.
const next8 = () => {
  const d = new Date();
  if (d.getHours() >= 8) d.setDate(d.getDate() + 1);
  d.setHours(8, 0, 0, 0);
  return d.getTime();
};

const next8Label = () => (new Date().getHours() >= 8 ? 'Jusqu’à demain 8 h' : 'Jusqu’à 8 h');

const dndLabel = (until: number) => {
  if (until >= DND_FOREVER) return 'Ne pas déranger : activé';
  const today = new Date().toDateString() === new Date(until).toDateString();
  return `Ne pas déranger jusqu’à ${today ? '' : 'demain '}${formatTime(until)}`;
};

// Do not disturb, from the header: a moon, lit while on, and its menu.
export function DndButton({ until, onChange }: { until: number; onChange: (until: number) => void }) {
  const [menu, setMenu] = useState<{ top: number; right: number } | null>(null);
  const close = useCallback(() => setMenu(null), []);
  useDismiss(menu !== null, close);
  // The backend turns it off when the time comes.
  const on = until > 0;

  const set = async (value: number) => {
    setMenu(null);
    onChange(await api.setDnd(value));
  };

  const options: [string, () => number][] = [
    ['Pendant 1 heure', () => Date.now() + HOUR],
    ['Pendant 8 heures', () => Date.now() + 8 * HOUR],
    [next8Label(), next8],
    ['Jusqu’à désactivation', () => DND_FOREVER],
  ];

  return (
    <>
      <button
        className={`icon-btn ${on ? 'active' : ''}`}
        title={on ? dndLabel(until) : 'Ne pas déranger'}
        onClick={e => {
          e.stopPropagation();
          const r = e.currentTarget.getBoundingClientRect();
          setMenu(menu ? null : { top: r.bottom + 4, right: window.innerWidth - r.right });
        }}
      >
        <Icon name="moon" />
      </button>
      {menu && (
        <div className="menu-popover" role="menu" style={{ top: menu.top, right: menu.right }}>
          <div className="menu-label">{on ? dndLabel(until) : 'Ne pas déranger : aucune notification'}</div>
          {options.map(([label, value]) => (
            <button key={label} role="menuitem" onClick={() => set(value())}>{label}</button>
          ))}
          {on && <button role="menuitem" onClick={() => set(0)}><Icon name="close" size={14} />Désactiver</button>}
        </div>
      )}
    </>
  );
}
