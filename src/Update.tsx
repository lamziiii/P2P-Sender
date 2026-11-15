import { useEffect, useState } from 'react';
import { api } from './api';
import type { Config, UpdateInfo, UpdateProgress } from './api';
import { ChangelogDialog } from './Changelog';
import { formatBytes } from './format';
import { isEnglish, tr } from './i18n';
import { Icon } from './icons';
import { isMobile } from './platform';
import { parseLines, RELEASES } from './releaseNotes';

// The newer version found: asked once, then followed through its event.
// Installing shows the download, then the app restarts on it (on Android,
// the system's installer opens).
function useUpdate() {
  const [update, setUpdate] = useState<UpdateInfo | null>(null);
  const [progress, setProgress] = useState<UpdateProgress | null>(null);
  const [error, setError] = useState('');
  useEffect(() => {
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
  if (p.waiting) return tr('Prête : installation à la fin des transferts.', 'Ready: it installs once the transfers end.');
  const downloading = tr('Téléchargement…', 'Downloading…');
  if (!p.total) return p.done ? `${downloading} ${formatBytes(p.done)}` : downloading;
  if (p.done >= p.total) return tr('Installation…', 'Installing…');
  return `${downloading} ${Math.round((p.done / p.total) * 100)} %`;
}

// What happens once installed.
const afterInstall = () => byInstall(
  tr('L’app redémarre une fois installée.', 'The app restarts once it is installed.'),
  tr('Android vous demande de confirmer l’installation.', 'Android asks you to confirm the install.'),
);
const byInstall = (desktop: string, android: string) => (isMobile ? android : desktop);

// Over the main view while a newer version waits.
export function UpdateBanner() {
  const { update, progress, error, install } = useUpdate();
  if (!update) return null;
  return (
    <div className="offer-banner compact">
      <div className="file-icon"><Icon name="down" size={16} /></div>
      <div className="item-body">
        <div className="offer-title ellipsis">{tr(`Version ${update.version} disponible`, `Version ${update.version} available`)}</div>
        <div className="item-sub ellipsis" style={error ? { color: 'var(--danger)' } : undefined} title={error || undefined}>
          {error || (progress ? progressText(progress) : update.failed ? tr('L’installation a échoué : réessayez.', 'The install failed: try again.') : afterInstall())}
        </div>
      </div>
      <button className="btn btn-small" disabled={!!progress} onClick={install}>{tr('Mettre à jour', 'Update')}</button>
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
  // Installed from the .deb: installing asks for the password.
  const [asksPassword, setAsksPassword] = useState(false);
  useEffect(() => {
    if (!isMobile) api.updateAsksPassword().then(setAsksPassword, () => {});
  }, []);
  // A version to install comes first, with the notes it was published with
  // (written in French: not shown in English, whose notes come with the app).
  const releases = update?.notes && !isEnglish
    ? [{ version: `${update.version} (à installer)`, date: '', lines: parseLines(update.notes) }, ...RELEASES]
    : RELEASES;

  const check = async () => {
    setChecking(true);
    setChecked('');
    try {
      const found = await api.checkUpdate();
      setUpdate(found);
      if (!found) setChecked(tr('Vous avez la dernière version.', 'You have the latest version.'));
    } catch (e) {
      setChecked(String(e));
    }
    setChecking(false);
  };

  const sub = error
    || (progress && progressText(progress))
    || (update && (update.failed
      ? tr(
        `L’installation de la version ${update.version} a échoué. Réessayez, ou installez-la depuis la page des versions.`,
        `Installing version ${update.version} failed. Try again, or install it from the releases page.`,
      )
      : tr(`La version ${update.version} est disponible.`, `Version ${update.version} is available.`)))
    || checked
    || tr('Les nouvelles versions sont cherchées automatiquement.', 'New versions are looked for automatically.');
  const action = update
    ? <button className="btn btn-small" disabled={!!progress} onClick={install}>{tr('Mettre à jour', 'Update')}</button>
    : <button className="btn btn-secondary btn-small" disabled={checking} onClick={check}>{checking ? tr('Recherche…', 'Checking…') : tr('Vérifier', 'Check')}</button>;

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
          <div className="setting-label">{tr('Nouveautés', 'What’s new')}</div>
          <div className="setting-sub">
            {update?.notes && !isEnglish
              ? `Ce qu’apporte la version ${update.version}, et les précédentes`
              : tr('Ce qui a changé à chaque version', 'What changed in each version')}
          </div>
        </div>
        <button className="btn btn-secondary btn-small" onClick={() => setNews(true)}>{tr('Voir', 'View')}</button>
      </div>
      {news && <ChangelogDialog title={tr('Nouveautés', 'What’s new')} releases={releases} onClose={() => setNews(false)} />}
      {!isMobile && (
        <div className="setting-row">
          <div>
            <div className="setting-label">{tr('Installer les mises à jour automatiquement', 'Install updates automatically')}</div>
            <div className="setting-sub" style={toggleError ? { color: 'var(--danger)' } : undefined}>
              {toggleError || (asksPassword
                ? tr(
                  'Une fois les transferts terminés : votre mot de passe vous est demandé pour installer le paquet .deb, puis l’app redémarre d’elle-même.',
                  'Once the transfers end: your password is asked to install the .deb package, then the app restarts by itself.',
                )
                : tr(
                  'Sans rien demander, une fois les transferts terminés. L’app redémarre d’elle-même.',
                  'Without asking, once the transfers end. The app restarts by itself.',
                ))}
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
