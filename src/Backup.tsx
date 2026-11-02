import { useState } from 'react';
import { api } from './api';
import { byPlatform } from './platform';

const MIN_PASSWORD = 8;

// Asks the backup's password (twice to export), then the file.
export function BackupDialog({ mode, includeChats = false, onClose, onExported }: {
  mode: 'export' | 'import';
  includeChats?: boolean;
  onClose: () => void;
  onExported?: (path: string) => void;
}) {
  const [password, setPassword] = useState('');
  const [confirm, setConfirm] = useState('');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState('');
  const exporting = mode === 'export';
  const ready = exporting ? password.length >= MIN_PASSWORD && confirm.length > 0 : password.length > 0;

  const submit = async () => {
    if (!ready || busy) return;
    if (exporting && password !== confirm) {
      setError('Les deux mots de passe ne sont pas identiques.');
      return;
    }
    setError('');
    setBusy(true);
    try {
      if (exporting) {
        const path = await api.exportBackup(password, includeChats);
        if (path) {
          onExported?.(path);
          onClose();
        }
      } else if (await api.importBackup(password)) {
        // The app restarts on the backup.
        return;
      }
      setBusy(false);
    } catch (e) {
      setError(String(e));
      setBusy(false);
    }
  };

  const keys = (e: React.KeyboardEvent) => {
    if (e.key === 'Enter') submit();
    if (e.key === 'Escape' && !busy) onClose();
  };

  return (
    <div className="modal-backdrop" onClick={() => !busy && onClose()}>
      <div className="modal stack" role="dialog" aria-modal="true" onClick={e => e.stopPropagation()}>
        <div className="modal-title" style={{ marginBottom: 0 }}>{exporting ? 'Exporter une sauvegarde' : 'Restaurer une sauvegarde'}</div>
        <div className="setting-sub">
          {exporting
            ? `Choisissez un mot de passe (${MIN_PASSWORD} caractères au moins) : il sera demandé pour la restaurer, et ne pourra pas être retrouvé.`
            : 'Le mot de passe de la sauvegarde, puis le fichier. Votre identité, vos amis et vos réglages actuels seront remplacés, puis l’application redémarrera.'}
        </div>
        <input
          className="input"
          type="password"
          autoFocus
          placeholder="Mot de passe"
          value={password}
          disabled={busy}
          onChange={e => { setPassword(e.target.value); setError(''); }}
          onKeyDown={keys}
        />
        {exporting && (
          <input
            className="input"
            type="password"
            placeholder="Confirmer le mot de passe"
            value={confirm}
            disabled={busy}
            onChange={e => { setConfirm(e.target.value); setError(''); }}
            onKeyDown={keys}
          />
        )}
        {exporting && password.length > 0 && password.length < MIN_PASSWORD && (
          <div className="setting-sub">Encore {MIN_PASSWORD - password.length} caractère{MIN_PASSWORD - password.length > 1 ? 's' : ''}.</div>
        )}
        {error && <div className="error-text">{error}</div>}
        <div style={{ display: 'flex', gap: 8 }}>
          <button className="btn btn-secondary" style={{ flex: 1 }} disabled={busy} onClick={onClose}>Annuler</button>
          <button className="btn" style={{ flex: 1 }} disabled={!ready || busy} onClick={submit}>
            {busy && <span className="spinner" />}
            {busy ? (exporting ? 'Chiffrement…' : 'Restauration…') : exporting ? 'Choisir le fichier…' : 'Choisir la sauvegarde…'}
          </button>
        </div>
      </div>
    </div>
  );
}

// Settings: export or restore the whole app.
export function BackupCard() {
  const [includeChats, setIncludeChats] = useState(true);
  const [dialog, setDialog] = useState<'export' | 'import' | null>(null);
  const [saved, setSaved] = useState('');

  return (
    <div className="card stack">
      <div className="card-label" style={{ marginBottom: 0 }}>Sauvegarde</div>
      <div className="setting-sub">
        Votre identité, vos amis, vos groupes et vos réglages dans un fichier chiffré, pour les retrouver sur un autre {byPlatform('ordinateur', 'appareil')}.
      </div>
      <label className="check-line">
        <input type="checkbox" checked={includeChats} onChange={e => setIncludeChats(e.target.checked)} />
        Inclure les discussions
      </label>
      <div style={{ display: 'flex', gap: 8 }}>
        <button className="btn btn-secondary btn-small" onClick={() => { setSaved(''); setDialog('export'); }}>Exporter…</button>
        <button className="btn btn-secondary btn-small" onClick={() => setDialog('import')}>Restaurer…</button>
      </div>
      {saved && <div className="setting-sub saved-mark" title={saved}>Sauvegarde enregistrée ✓</div>}
      <div className="setting-sub warning-text">
        N’utilisez jamais une même sauvegarde sur deux {byPlatform('ordinateurs', 'appareils')} à la fois : ils auraient le même ID et la même identité LocalSend.
      </div>
      {dialog && (
        <BackupDialog mode={dialog} includeChats={includeChats} onClose={() => setDialog(null)} onExported={setSaved} />
      )}
    </div>
  );
}
