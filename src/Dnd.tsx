import { useCallback, useState } from 'react';
import { api, DND_FOREVER } from './api';
import { formatTime } from './format';
import { tr } from './i18n';
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

const next8Label = () => (new Date().getHours() >= 8
  ? tr('Jusqu’à demain 8 h', 'Until 8 am tomorrow')
  : tr('Jusqu’à 8 h', 'Until 8 am'));

const dndLabel = (until: number) => {
  if (until >= DND_FOREVER) return tr('Ne pas déranger : activé', 'Do not disturb: on');
  const today = new Date().toDateString() === new Date(until).toDateString();
  return tr(
    `Ne pas déranger jusqu’à ${today ? '' : 'demain '}${formatTime(until)}`,
    `Do not disturb until ${today ? '' : 'tomorrow '}${formatTime(until)}`,
  );
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
    [tr('Pendant 1 heure', 'For 1 hour'), () => Date.now() + HOUR],
    [tr('Pendant 8 heures', 'For 8 hours'), () => Date.now() + 8 * HOUR],
    [next8Label(), next8],
    [tr('Jusqu’à désactivation', 'Until turned off'), () => DND_FOREVER],
  ];

  return (
    <>
      <button
        className={`icon-btn ${on ? 'active' : ''}`}
        title={on ? dndLabel(until) : tr('Ne pas déranger', 'Do not disturb')}
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
          <div className="menu-label">{on ? dndLabel(until) : tr('Ne pas déranger : aucune notification', 'Do not disturb: no notifications')}</div>
          {options.map(([label, value]) => (
            <button key={label} role="menuitem" onClick={() => set(value())}>{label}</button>
          ))}
          {on && <button role="menuitem" onClick={() => set(0)}><Icon name="close" size={14} />{tr('Désactiver', 'Turn off')}</button>}
        </div>
      )}
    </>
  );
}
