import { memo, useState } from 'react';
import { api } from './api';
import type { Transfer } from './api';
import { ACTIVE, FINAL, formatBytes, plural, RELAY_HINT, ROUTE_LABELS } from './format';
import { Icon } from './icons';
import { byPlatform } from './platform';
import { useTransfers } from './transferStore';


const formatSpeed = (bps: number) => (bps > 0 ? `${formatBytes(bps)}/s` : '');

const formatEta = (t: { fileSize: number; bytes: number; speed: number }) => {
  if (t.speed <= 0) return '';
  const s = Math.round((t.fileSize - t.bytes) / t.speed);
  if (s < 60) return `${s} s`;
  if (s < 3600) return `${Math.floor(s / 60)} min ${s % 60} s`;
  return `${Math.floor(s / 3600)} h ${Math.floor((s % 3600) / 60)} min`;
};

const color = (status: string) => {
  if (status === 'completed') return 'var(--success)';
  if (status === 'declined' || status === 'cancelled' || status === 'error') return 'var(--danger)';
  if (ACTIVE.has(status)) return 'var(--accent-fg)';
  return 'var(--text-secondary)';
};

// The status, and what happens next when it is waiting.
const describe = (t: Transfer, who: string): { label: string; hint?: string } => {
  switch (t.status) {
    case 'completed': return { label: t.dir === 'in' ? 'Reçu' : 'Envoyé' };
    case 'declined': return { label: 'Refusé', hint: t.dir === 'out' ? `${who} n’a pas accepté ce fichier.` : undefined };
    case 'cancelled': return { label: 'Annulé' };
    case 'error': return { label: 'Échec' };
    case 'pending': return { label: 'En file d’attente' };
    case 'connecting': return { label: 'En attente', hint: `L’envoi partira dès que ${who} sera en ligne.` };
    case 'waiting_consent': return { label: 'En attente', hint: `${who} doit accepter le fichier.` };
    case 'paused':
      if (t.pausedBy === 'me') return { label: 'En pause' };
      if (t.pausedBy === 'peer') return { label: 'Mis en pause par le contact' };
      return { label: 'En pause', hint: `Connexion perdue : reprise automatique dès que ${who} revient.` };
    case 'finishing': return { label: 'Finalisation…' };
    default: return { label: `${t.progress} %` };
  }
};

// Pause and resume by hand: accepted transfers with a friend.
const canPause = (t: Transfer) => ACTIVE.has(t.status) || (t.status === 'paused' && !t.pausedBy);
const canResume = (t: Transfer) => t.status === 'paused' && t.pausedBy === 'me';

const Progress = ({ value, status }: { value: number; status: string }) => (
  <div className="progress-track">
    <div
      className="progress-fill"
      style={{
        width: `${FINAL.has(status) && status !== 'completed' ? 100 : value}%`,
        background: status === 'completed' ? 'var(--success)' : FINAL.has(status) ? 'var(--danger)' : 'var(--accent)',
      }}
    />
  </div>
);

// Each row renders again only when its transfer changes. label: its path
// within its folder, shown under the folder.
const TransferRow = memo(function TransferRow({ t, who, pausable, label }: {
  t: Transfer;
  who: string;
  pausable: boolean;
  label?: string;
}) {
  const { label: state, hint } = describe(t, who);
  const name = label ?? t.fileName;
  // A click opens what we received, except files that run code.
  const openable = t.dir === 'in' && t.status === 'completed' && !t.executable;
  return (
    <div className="list-item transfer-item">
      <div className="file-icon"><Icon name="file" size={18} /></div>
      <div className="item-body">
        <div className="transfer-header">
          {openable
            ? <button className="item-title transfer-name link-title" title={`Ouvrir ${name}`} onClick={() => api.openTransfer(t.id)}>{name}</button>
            : <span className="item-title transfer-name" title={name}>{name}</span>}
          <span className="transfer-status" style={{ color: color(t.status) }}>{state}</span>
        </div>
        {!FINAL.has(t.status) && <Progress value={t.progress} status={t.status} />}
        <div className="transfer-meta">
          <span className="transfer-dir">
            {ACTIVE.has(t.status) || t.status === 'paused' ? `${formatBytes(t.bytes)} / ` : ''}{formatBytes(t.fileSize)}
          </span>
          {ACTIVE.has(t.status) && t.speed > 0 && (
            <span style={{ whiteSpace: 'nowrap' }}>
              {t.route && (
                <span title={t.route === 'relay' ? RELAY_HINT : undefined}>{ROUTE_LABELS[t.route]} · </span>
              )}
              {formatSpeed(t.speed)} · {formatEta(t)}
            </span>
          )}
        </div>
        {hint && !FINAL.has(t.status) && <div className="item-sub">{hint}</div>}
        {hint && FINAL.has(t.status) && <div className="error-text">{hint}</div>}
        {t.error && <div className="error-text" style={{ marginTop: 4 }}>{t.error}</div>}
        {t.status === 'completed' && t.dir === 'in' && t.executable && (
          <div className="item-sub">Ce fichier peut exécuter du code : ouvrez-le depuis son dossier si vous l’attendiez.</div>
        )}
        {(!FINAL.has(t.status) || (t.status === 'completed' && t.dir === 'in')) && (
          <div className="row-actions">
            {t.status === 'completed' && t.dir === 'in' && (
              <button className="btn btn-secondary btn-small" onClick={() => api.showInFolder(t.id)}><Icon name="folder" size={14} />Afficher dans le dossier</button>
            )}
            {pausable && canPause(t) && (
              <button className="btn btn-secondary btn-small" onClick={() => api.pauseTransfer(t.id)}><Icon name="pause" size={14} />Pause</button>
            )}
            {pausable && canResume(t) && (
              <button className="btn btn-secondary btn-small" onClick={() => api.resumeTransfer(t.id)}><Icon name="play" size={14} />Reprendre</button>
            )}
            {!FINAL.has(t.status) && (
              <button className="btn btn-secondary btn-small" onClick={() => api.cancelTransfer(t.id)}>Annuler</button>
            )}
          </div>
        )}
      </div>
    </div>
  );
});

const sameList = (a: Transfer[], b: Transfer[]) => a.length === b.length && a.every((t, i) => t === b[i]);

// Several files with the same person, one way: one line to unfold.
const TransferGroup = memo(function TransferGroup({ list, who, running, pausable }: {
  list: Transfer[];
  who: string;
  running: boolean;
  pausable: boolean;
}) {
  const [open, setOpen] = useState(false);
  const out = list[0].dir === 'out';
  const size = list.reduce((n, t) => n + t.fileSize, 0);
  const bytes = list.reduce((n, t) => n + (t.status === 'completed' ? t.fileSize : t.bytes), 0);
  const progress = size > 0 ? Math.floor((bytes * 100) / size) : 0;
  const done = list.filter(t => t.status === 'completed').length;
  const failed = list.filter(t => FINAL.has(t.status) && t.status !== 'completed').length;
  const status = running ? 'sending' : failed === list.length ? 'error' : 'completed';
  return (
    <div className="list-item transfer-item transfer-group">
      <button className="group-head list-button" onClick={() => setOpen(!open)} aria-expanded={open}>
        <div className="file-icon"><Icon name={out ? 'up' : 'down'} size={18} /></div>
        <div className="item-body">
          <div className="transfer-header">
            <span className="item-title">{list.length} fichiers {out ? `à ${who}` : `de ${who}`}</span>
            <Icon name={open ? 'chevronDown' : 'chevronRight'} size={14} />
          </div>
          {running && <Progress value={progress} status="sending" />}
          <div className="transfer-meta">
            <span>{formatBytes(size)}{running ? ` · ${progress} %` : ''}</span>
            <span style={{ color: color(status) }}>
              {running ? `${done}/${list.length} terminés` : failed > 0 ? `${done} ${out ? 'envoyés' : 'reçus'} · ${failed} en échec` : out ? 'Tous envoyés' : 'Tous reçus'}
            </span>
          </div>
        </div>
      </button>
      {running && (
        <div className="row-actions">
          <button className="btn btn-secondary btn-small" onClick={() => list.filter(t => !FINAL.has(t.status)).forEach(t => api.cancelTransfer(t.id))}>Tout annuler</button>
        </div>
      )}
      {open && <div className="list nested">{list.map(t => <TransferRow key={t.id} t={t} who={who} pausable={pausable} />)}</div>}
    </div>
  );
}, (a, b) => a.who === b.who && a.running === b.running && a.pausable === b.pausable && sameList(a.list, b.list));

// A folder's files: one row for the folder, its files unfolded.
const FolderGroup = memo(function FolderGroup({ list, who, running, pausable }: {
  list: Transfer[];
  who: string;
  running: boolean;
  pausable: boolean;
}) {
  const [open, setOpen] = useState(false);
  const [first] = list;
  const folder = first.folder!;
  const out = first.dir === 'out';
  // The folder's size as the sender counted it, unless fewer files are listed.
  const listed = list.reduce((n, t) => n + t.fileSize, 0);
  const size = list.length >= folder.files ? folder.size : listed;
  const bytes = list.reduce((n, t) => n + (t.status === 'completed' ? t.fileSize : t.bytes), 0);
  const progress = size > 0 ? Math.min(100, Math.floor((bytes * 100) / size)) : 0;
  const done = list.filter(t => t.status === 'completed').length;
  const failed = list.filter(t => FINAL.has(t.status) && t.status !== 'completed').length;
  const status = running ? 'sending' : failed === list.length ? 'error' : 'completed';
  const pausedByPeer = running && list.some(t => t.pausedBy === 'peer');
  const pauses = pausable && running ? list.filter(canPause) : [];
  const resumes = pausable && running ? list.filter(canResume) : [];
  const shown = !out && list.find(t => t.status === 'completed');
  // Its files' paths without the folder's name.
  const inner = (t: Transfer) => t.relPath?.split('/').slice(1).join('/') || t.fileName;
  return (
    <div className="list-item transfer-item transfer-group">
      <button className="group-head list-button" onClick={() => setOpen(!open)} aria-expanded={open}>
        <div className="file-icon"><Icon name="folder" size={18} /></div>
        <div className="item-body">
          <div className="transfer-header">
            <span className="item-title" title={folder.name}>{folder.name}</span>
            <Icon name={open ? 'chevronDown' : 'chevronRight'} size={14} />
          </div>
          {running && <Progress value={progress} status="sending" />}
          <div className="transfer-meta">
            <span className="ellipsis">{plural(folder.files, 'fichier', 'fichiers')} · {formatBytes(size)} · {out ? `à ${who}` : `de ${who}`}</span>
            <span style={{ color: color(status), whiteSpace: 'nowrap' }}>
              {pausedByPeer ? 'Mis en pause par le contact'
                : running ? `${done}/${list.length} · ${progress} %`
                  : failed > 0 ? `${done} ${out ? 'envoyés' : 'reçus'} · ${failed} en échec` : out ? 'Envoyé' : 'Reçu'}
            </span>
          </div>
        </div>
      </button>
      {(running || shown) && (
        <div className="row-actions">
          {shown && (
            <button className="btn btn-secondary btn-small" onClick={() => api.showInFolder(shown.id)}><Icon name="folder" size={14} />Afficher le dossier</button>
          )}
          {resumes.length > 0 && (
            <button className="btn btn-secondary btn-small" onClick={() => resumes.forEach(t => api.resumeTransfer(t.id))}><Icon name="play" size={14} />Reprendre</button>
          )}
          {resumes.length === 0 && pauses.length > 0 && (
            <button className="btn btn-secondary btn-small" onClick={() => pauses.forEach(t => api.pauseTransfer(t.id))}><Icon name="pause" size={14} />Pause</button>
          )}
          {running && (
            <button className="btn btn-secondary btn-small" onClick={() => list.filter(t => !FINAL.has(t.status)).forEach(t => api.cancelTransfer(t.id))}>Tout annuler</button>
          )}
        </div>
      )}
      {open && (
        <div className="list nested">
          {list.map(t => <TransferRow key={t.id} t={t} who={who} pausable={pausable} label={inner(t)} />)}
        </div>
      )}
    </div>
  );
}, (a, b) => a.who === b.who && a.running === b.running && a.pausable === b.pausable && sameList(a.list, b.list));

const folderKey = (t: Transfer) => `${t.dir}:${t.friendId}:${t.folder!.id}`;

// Folders apart, then the other files by person and direction, in the order
// they first appear.
const arrange = (list: Transfer[]) => {
  const groups = new Map<string, Transfer[]>();
  for (const t of list) {
    const key = t.folder ? `folder:${folderKey(t)}` : `${t.dir}:${t.friendId}:${t.peerName ?? ''}`;
    groups.set(key, [...(groups.get(key) ?? []), t]);
  }
  return [...groups].map(([key, group]) => ({ folder: key.startsWith('folder:'), list: group }));
};

export function TransfersTab({ nameOf, pausable, onClear }: {
  nameOf: (t: Transfer) => string;
  // Paused by hand: transfers with friends only.
  pausable: (t: Transfer) => boolean;
  onClear: () => void;
}) {
  // Without the offers waiting for an answer.
  const transfers = useTransfers().filter(t => t.status !== 'pending' || t.queued);
  // A folder stays whole in the running ones until all its files are over.
  const runningFolders = new Set(transfers.filter(t => t.folder && !FINAL.has(t.status)).map(folderKey));
  const isRunning = (t: Transfer) => !FINAL.has(t.status) || (!!t.folder && runningFolders.has(folderKey(t)));
  const running = transfers.filter(isRunning);
  const finished = transfers.filter(t => !isRunning(t));

  const section = (list: Transfer[], areRunning: boolean) => (
    <div className="list">
      {arrange(list).map(({ folder, list: group }) => {
        const first = group[0];
        const who = nameOf(first);
        const canPause = pausable(first);
        if (folder) return <FolderGroup key={first.id} list={group} who={who} running={areRunning} pausable={canPause} />;
        return group.length === 1
          ? <TransferRow key={first.id} t={first} who={who} pausable={canPause} />
          : <TransferGroup key={first.id} list={group} who={who} running={areRunning} pausable={canPause} />;
      })}
    </div>
  );

  if (transfers.length === 0) {
    return (
      <div className="empty-state">
        <Icon name="transfers" size={32} />
        {byPlatform(
          'Aucun transfert. Glissez un fichier ou un dossier sur un ami pour le lui envoyer.',
          'Aucun transfert. Touchez l’icône d’envoi à côté d’un ami pour lui envoyer un fichier.',
        )}
      </div>
    );
  }
  return (
    <>
      {running.length > 0 && (
        <>
          <div className="section-header">En cours ({running.length})</div>
          {section(running, true)}
        </>
      )}
      {finished.length > 0 && (
        <>
          <div className="section-header section-header-row" style={{ marginTop: running.length > 0 ? 6 : 0 }}>
            <span>Terminés ({finished.length})</span>
            <button className="btn-link" onClick={onClear}>Effacer</button>
          </div>
          {section(finished, false)}
          <div className="setting-sub" style={{ textAlign: 'center' }}>Les transferts terminés disparaissent d’eux-mêmes au bout d’un jour.</div>
        </>
      )}
    </>
  );
}
