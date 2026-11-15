import { useState } from 'react';
import { api } from './api';
import { plural } from './format';
import { tr } from './i18n';
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
      setError(tr('Les deux mots de passe ne sont pas identiques.', 'The two passwords do not match.'));
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
        <div className="modal-title" style={{ marginBottom: 0 }}>{exporting ? tr('Exporter une sauvegarde', 'Export a backup') : tr('Restaurer une sauvegarde', 'Restore a backup')}</div>
        <div className="setting-sub">
          {exporting
            ? tr(
              `Choisissez un mot de passe (${MIN_PASSWORD} caractères au moins) : il sera demandé pour la restaurer, et ne pourra pas être retrouvé.`,
              `Choose a password (at least ${MIN_PASSWORD} characters): it will be asked to restore it, and cannot be recovered.`,
            )
            : tr(
              'Le mot de passe de la sauvegarde, puis le fichier. Votre identité, vos amis et vos réglages actuels seront remplacés, puis l’application redémarrera.',
              'The backup’s password, then the file. Your current identity, friends and settings will be replaced, then the app will restart.',
            )}
        </div>
        <input
          className="input"
          type="password"
          autoFocus
          placeholder={tr('Mot de passe', 'Password')}
          value={password}
          disabled={busy}
          onChange={e => { setPassword(e.target.value); setError(''); }}
          onKeyDown={keys}
        />
        {exporting && (
          <input
            className="input"
            type="password"
            placeholder={tr('Confirmer le mot de passe', 'Confirm the password')}
            value={confirm}
            disabled={busy}
            onChange={e => { setConfirm(e.target.value); setError(''); }}
            onKeyDown={keys}
          />
        )}
        {exporting && password.length > 0 && password.length < MIN_PASSWORD && (
          <div className="setting-sub">
            {tr('Encore ', '')}{plural(MIN_PASSWORD - password.length, tr('caractère', 'more character'), tr('caractères', 'more characters'))}.
          </div>
        )}
        {error && <div className="error-text">{error}</div>}
        <div style={{ display: 'flex', gap: 8 }}>
          <button className="btn btn-secondary" style={{ flex: 1 }} disabled={busy} onClick={onClose}>{tr('Annuler', 'Cancel')}</button>
          <button className="btn" style={{ flex: 1 }} disabled={!ready || busy} onClick={submit}>
            {busy && <span className="spinner" />}
            {busy
              ? (exporting ? tr('Chiffrement…', 'Encrypting…') : tr('Restauration…', 'Restoring…'))
              : exporting ? tr('Choisir le fichier…', 'Choose the file…') : tr('Choisir la sauvegarde…', 'Choose the backup…')}
          </button>
        </div>
      </div>
    </div>
  );
}

// Settings: export or restore the whole app.
export function BackupRow() {
  const [includeChats, setIncludeChats] = useState(true);
  const [dialog, setDialog] = useState<'export' | 'import' | null>(null);
  const [saved, setSaved] = useState('');

  return (
    <div className="stack">
      <div className="setting-sub">
        {tr(
          `Votre identité, vos amis, vos groupes et vos réglages dans un fichier chiffré, pour les retrouver sur un autre ${byPlatform('ordinateur', 'appareil')}.`,
          `Your identity, friends, groups and settings in an encrypted file, to get them back on another ${byPlatform('computer', 'device')}.`,
        )}
      </div>
      <label className="check-line">
        <input type="checkbox" checked={includeChats} onChange={e => setIncludeChats(e.target.checked)} />
        {tr('Inclure les discussions', 'Include the chats')}
      </label>
      <div style={{ display: 'flex', gap: 8 }}>
        <button className="btn btn-secondary btn-small" onClick={() => { setSaved(''); setDialog('export'); }}>{tr('Exporter…', 'Export…')}</button>
        <button className="btn btn-secondary btn-small" onClick={() => setDialog('import')}>{tr('Restaurer…', 'Restore…')}</button>
      </div>
      {saved && <div className="setting-sub saved-mark" title={saved}>{tr('Sauvegarde enregistrée ✓', 'Backup saved ✓')}</div>}
      <div className="setting-sub warning-text">
        {tr(
          `N’utilisez jamais une même sauvegarde sur deux ${byPlatform('ordinateurs', 'appareils')} à la fois : ils auraient le même ID et la même identité LocalSend.`,
          `Never use the same backup on two ${byPlatform('computers', 'devices')} at once: they would have the same ID and the same LocalSend identity.`,
        )}
      </div>
      {dialog && (
        <BackupDialog mode={dialog} includeChats={includeChats} onClose={() => setDialog(null)} onExported={setSaved} />
      )}
    </div>
  );
}
