import { useEffect, useState } from 'react';
import { api } from './api';
import type { Config, UpdateInfo, UpdateProgress } from './api';
import { ChangelogDialog } from './Changelog';
import { formatBytes } from './format';
import { Icon } from './icons';
import { isMobile } from './platform';
import { parseLines, RELEASES } from './releaseNotes';

const RELEASES_PAGE = 'https://github.com/lamziiii/P2P-Sender/releases/latest';

// The newer version found (desktop): asked once, then followed through its
// event. Installing shows the download, then the app restarts on it.
function useUpdate() {
  const [update, setUpdate] = useState<UpdateInfo | null>(null);
  const [progress, setProgress] = useState<UpdateProgress | null>(null);
  const [error, setError] = useState('');
  useEffect(() => {
    if (isMobile) return;
    let fromEvent = false;
    const load = () => api.getUpdate().then(u => { if (!fromEvent) setUpdate(u); }, () => {});
    load();
    const stops = [
      api.onUpdateAvailable(u => {
        fromEvent = true;
        setUpdate(u);
      }, load),
      api.onUpdateProgress(setProgress),
      api.onUpdateError(message => {
        setError(message);
        setProgress(null);
      }),
    ];
    return () => stops.forEach(stop => stop());
  }, []);

  const install = async () => {
    setError('');
    setProgress({ done: 0, total: null });
    try {
      await api.installUpdate();
    } catch (e) {
      setError(String(e));
      setProgress(null);
    }
  };
  return { update, setUpdate, progress, error, install };
}

function progressText(p: UpdateProgress) {
  if (p.waiting) return 'Prête : installation à la fin des transferts.';
  if (!p.total) return p.done ? `Téléchargement… ${formatBytes(p.done)}` : 'Téléchargement…';
  if (p.done >= p.total) return 'Installation…';
  return `Téléchargement… ${Math.round((p.done / p.total) * 100)} %`;
}

// Over the main view while a newer version waits.
export function UpdateBanner() {
  const { update, progress, error, install } = useUpdate();
  if (!update) return null;
  return (
    <div className="offer-banner compact">
      <div className="file-icon"><Icon name="down" size={16} /></div>
      <div className="item-body">
        <div className="offer-title ellipsis">Version {update.version} disponible</div>
        <div className="item-sub ellipsis" style={error ? { color: 'var(--danger)' } : undefined} title={error || undefined}>
          {error || (progress ? progressText(progress) : update.failed ? 'L’installation a échoué : réessayez.' : 'L’app redémarre une fois installée.')}
        </div>
      </div>
      <button className="btn btn-small" disabled={!!progress} onClick={install}>Mettre à jour</button>
    </div>
  );
}

// The version and its updates, rows of the settings.
export function UpdateRows({ config, setConfig }: {
  config: Partial<Config>;
  setConfig: (update: (c: Partial<Config>) => Partial<Config>) => void;
}) {
  const { update, setUpdate, progress, error, install } = useUpdate();
  const [checking, setChecking] = useState(false);
  const [checked, setChecked] = useState('');
  const [news, setNews] = useState(false);
  const [toggleError, setToggleError] = useState('');
  // A version to install comes first, with the notes it was published with.
  const releases = update?.notes
    ? [{ version: `${update.version} (à installer)`, date: '', lines: parseLines(update.notes) }, ...RELEASES]
    : RELEASES;

  const check = async () => {
    setChecking(true);
    setChecked('');
    try {
      const found = await api.checkUpdate();
      setUpdate(found);
      if (!found) setChecked('Vous avez la dernière version.');
    } catch (e) {
      setChecked(String(e));
    }
    setChecking(false);
  };

  const sub = isMobile
    ? 'Sur Android, téléchargez chaque nouvelle version (APK) depuis la page des versions.'
    : error
      || (progress && progressText(progress))
      || (update && (update.failed
        ? `L’installation de la version ${update.version} a échoué. Réessayez, ou installez-la depuis la page des versions.`
        : `La version ${update.version} est disponible.`))
      || checked
      || 'Les nouvelles versions sont cherchées automatiquement.';
  let action;
  if (isMobile) action = <button className="btn btn-secondary btn-small" onClick={() => api.openUrl(RELEASES_PAGE)}>Voir</button>;
  else if (update) action = <button className="btn btn-small" disabled={!!progress} onClick={install}>Mettre à jour</button>;
  else action = <button className="btn btn-secondary btn-small" disabled={checking} onClick={check}>{checking ? 'Recherche…' : 'Vérifier'}</button>;

  return (
    <>
      <div className="setting-row">
        <div>
          <div className="setting-label">P2P Share {__APP_VERSION__}</div>
          <div className="setting-sub" style={error ? { color: 'var(--danger)' } : undefined}>{sub}</div>
        </div>
        {action}
      </div>
      <div className="setting-row">
        <div>
          <div className="setting-label">Nouveautés</div>
          <div className="setting-sub">
            {update?.notes ? `Ce qu’apporte la version ${update.version}, et les précédentes` : 'Ce qui a changé à chaque version'}
          </div>
        </div>
        <button className="btn btn-secondary btn-small" onClick={() => setNews(true)}>Voir</button>
      </div>
      {news && <ChangelogDialog title="Nouveautés" releases={releases} onClose={() => setNews(false)} />}
      {!isMobile && (
        <div className="setting-row">
          <div>
            <div className="setting-label">Installer les mises à jour automatiquement</div>
            <div className="setting-sub" style={toggleError ? { color: 'var(--danger)' } : undefined}>
              {toggleError || 'Sans rien demander, une fois les transferts terminés. L’app redémarre d’elle-même.'}
            </div>
          </div>
          <label className="toggle">
            <input
              type="checkbox"
              checked={config.autoUpdate ?? false}
              onChange={async e => {
                setToggleError('');
                try {
                  const autoUpdate = await api.setAutoUpdate(e.target.checked);
                  setConfig(c => ({ ...c, autoUpdate }));
                } catch (err) {
                  setToggleError(String(err));
                }
              }}
            />
            <span className="toggle-track" />
          </label>
        </div>
      )}
    </>
  );
}
