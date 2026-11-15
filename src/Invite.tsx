import { useState } from 'react';
import { api } from './api';
import type { Friend, InviteLink } from './api';
import { tr } from './i18n';
import { Icon } from './icons';

// Invitation links (see src-tauri/src/invite.rs): one friend sends a link,
// the other opens it; both are then friends.

const ACCEPT_ERRORS: Record<string, string> = tr({
  invalid_link: 'Ce lien d’invitation n’est pas valide.',
  self: 'C’est votre propre lien d’invitation.',
}, {
  invalid_link: 'This invitation link is not valid.',
  self: 'This is your own invitation link.',
});

// Makes a link and copies it.
export function InviteLinkMaker() {
  const [link, setLink] = useState('');
  const [copied, setCopied] = useState(false);
  const [error, setError] = useState('');

  // "Copié !" only once it is: the clipboard may be refused.
  const copy = (text: string) => {
    navigator.clipboard.writeText(text).then(
      () => {
        setCopied(true);
        setTimeout(() => setCopied(false), 2000);
      },
      () => setError(tr('Copie impossible : sélectionnez le lien pour le copier.', 'Copy failed: select the link to copy it.')),
    );
  };

  const create = async () => {
    setError('');
    try {
      const made = await api.createInvite();
      setLink(made);
      copy(made);
    } catch (e) {
      setError(String(e));
    }
  };

  return (
    <div className="stack">
      {link ? (
        <div className="id-box-inline">
          <div className="id-value" title={link}>{link}</div>
          <button className="btn btn-secondary btn-small" onClick={() => copy(link)}><Icon name="copy" size={14} />{copied ? tr('Copié !', 'Copied!') : tr('Copier', 'Copy')}</button>
        </div>
      ) : (
        <button className="btn" onClick={create}><Icon name="plus" size={14} />{tr('Créer un lien d’invitation', 'Create an invitation link')}</button>
      )}
      <div className="setting-sub">
        {link
          ? tr('Envoyez ce lien à votre ami (mail, message…) : valable 7 jours, pour une seule personne.', 'Send this link to your friend (email, message…): valid for 7 days, for one person.')
          : tr('Envoyez-le à votre ami par mail ou message : il l’ouvre, et vous voilà amis.', 'Send it to your friend by email or message: they open it, and you are friends.')}
      </div>
      {link && <button className="btn-link" style={{ alignSelf: 'flex-start' }} onClick={create}>{tr('Créer un autre lien', 'Create another link')}</button>}
      {error && <div className="error-text">{error}</div>}
    </div>
  );
}

// The card of the friends list: invite someone, or open a link received.
export function InviteFriend({ onOpenLink, onClose }: {
  onOpenLink: (invite: InviteLink) => void;
  onClose?: () => void;
}) {
  const [pasted, setPasted] = useState('');
  const [error, setError] = useState('');

  const open = async () => {
    if (!pasted.trim()) return;
    try {
      onOpenLink(await api.readInviteLink(pasted));
      setPasted('');
      setError('');
    } catch (e) {
      setError(String(e));
    }
  };

  return (
    <div className="card stack">
      <div className="section-header-row">
        <div className="card-label" style={{ marginBottom: 0 }}>{tr('Inviter un ami', 'Invite a friend')}</div>
        {onClose && <button className="icon-btn small" title={tr('Fermer', 'Close')} onClick={onClose}><Icon name="close" size={14} /></button>}
      </div>
      <InviteLinkMaker />
      <div className="card-label" style={{ margin: '4px 0 0' }}>{tr('Vous avez reçu un lien ?', 'Received a link?')}</div>
      <div className="setting-sub">{tr('S’il n’ouvre pas l’app, collez-le ici.', 'If it does not open the app, paste it here.')}</div>
      <input
        className="input"
        placeholder={tr('Lien d’invitation', 'Invitation link')}
        value={pasted}
        onChange={e => { setPasted(e.target.value); setError(''); }}
        onKeyDown={e => e.key === 'Enter' && open()}
      />
      {error && <div className="error-text">{error}</div>}
      <button className="btn btn-secondary" disabled={!pasted.trim()} onClick={open}>{tr('Ouvrir le lien', 'Open the link')}</button>
    </div>
  );
}

// A link opened (or pasted) asks first: a web page or another app can open
// one, so opening it is not the yes.
export function InviteDialog({ invite, onDone }: {
  invite: InviteLink;
  // With the friends once added.
  onDone: (added?: { friends: Friend[]; id: string; name: string }) => void;
}) {
  const [error, setError] = useState('');
  const [busy, setBusy] = useState(false);
  const who = invite.name || tr('cette personne', 'this person');
  const adding = !invite.own && !invite.known;

  const accept = () => {
    setBusy(true);
    api.acceptInvite(invite.link).then(
      res => {
        setBusy(false);
        if ('error' in res) setError(ACCEPT_ERRORS[res.error] ?? tr('Impossible d’ajouter cet ami.', 'Could not add this friend.'));
        else onDone({ ...res, name: invite.name });
      },
      () => {
        setBusy(false);
        setError(tr('Impossible d’ajouter cet ami.', 'Could not add this friend.'));
      },
    );
  };

  let title;
  let text;
  if (invite.own) {
    title = tr('Votre propre lien', 'Your own link');
    text = tr('C’est un lien que vous avez créé : envoyez-le à un ami pour qu’il vous ajoute.', 'This is a link you created: send it to a friend so they add you.');
  } else if (invite.known) {
    title = tr(`${who} est déjà dans vos amis`, `${who} is already your friend`);
    text = tr('Vous êtes déjà connectés : rien à faire.', 'You are already connected: nothing to do.');
  } else if (error) {
    title = tr(`${who} n’a pas pu être ajouté`, `${who} could not be added`);
    text = tr('Demandez-lui un nouveau lien d’invitation.', 'Ask them for a new invitation link.');
  } else {
    title = tr(`Ajouter ${who} à vos amis ?`, `Add ${who} as a friend?`);
    text = tr(
      'Un lien d’invitation a été ouvert. Ajoutez cette personne seulement si vous attendiez son lien : elle pourra vous écrire et vous proposer des fichiers.',
      'An invitation link was opened. Only add this person if you were expecting their link: they will be able to write to you and offer you files.',
    );
  }

  return (
    <div className="modal-backdrop" onClick={() => !busy && onDone()}>
      <div className="modal stack" role="dialog" aria-modal="true" onClick={e => e.stopPropagation()}>
        <div className="modal-title">{title}</div>
        <div className="modal-text">{text}</div>
        {adding && !error && (
          <div className="setting-sub" title={invite.id}>
            {tr('Identifiant', 'ID')} : <code>{invite.id.slice(0, 12)}…</code>
          </div>
        )}
        {adding && !error && invite.lookalike && (
          <div className="error-text">
            {tr(
              `Un de vos amis s’appelle déjà « ${invite.name} ». Ce lien vient d’un autre appareil : ce n’est peut-être pas lui.`,
              `One of your friends is already called “${invite.name}”. This link comes from another device: it may not be them.`,
            )}
          </div>
        )}
        {error && <div className="error-text">{error}</div>}
        {adding && !error ? (
          <div style={{ display: 'flex', gap: 8 }}>
            <button className="btn btn-secondary" style={{ flex: 1 }} autoFocus disabled={busy} onClick={() => onDone()}>{tr('Annuler', 'Cancel')}</button>
            <button className="btn" style={{ flex: 1 }} disabled={busy} onClick={accept}>{busy ? tr('Ajout…', 'Adding…') : tr('Ajouter', 'Add')}</button>
          </div>
        ) : (
          <button className="btn" autoFocus onClick={() => onDone()}>OK</button>
        )}
      </div>
    </div>
  );
}
