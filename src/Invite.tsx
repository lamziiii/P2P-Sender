import { useEffect, useRef, useState } from 'react';
import { api } from './api';
import type { Friend, InviteLink } from './api';
import { Icon } from './icons';

// Invitation links (see src-tauri/src/invite.rs): one friend sends a link,
// the other opens it; both are then friends.

const ACCEPT_ERRORS: Record<string, string> = {
  invalid_link: 'Ce lien d’invitation n’est pas valide.',
  self: 'C’est votre propre lien d’invitation.',
};

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
      () => setError('Copie impossible : sélectionnez le lien pour le copier.'),
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
          <button className="btn btn-secondary btn-small" onClick={() => copy(link)}><Icon name="copy" size={14} />{copied ? 'Copié !' : 'Copier'}</button>
        </div>
      ) : (
        <button className="btn" onClick={create}><Icon name="plus" size={14} />Créer un lien d’invitation</button>
      )}
      <div className="setting-sub">
        {link
          ? 'Envoyez ce lien à votre ami (mail, message…) : valable 7 jours, pour une seule personne.'
          : 'Envoyez-le à votre ami par mail ou message : il l’ouvre, et vous voilà amis.'}
      </div>
      {link && <button className="btn-link" style={{ alignSelf: 'flex-start' }} onClick={create}>Créer un autre lien</button>}
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
        <div className="card-label" style={{ marginBottom: 0 }}>Inviter un ami</div>
        {onClose && <button className="icon-btn small" title="Fermer" onClick={onClose}><Icon name="close" size={14} /></button>}
      </div>
      <InviteLinkMaker />
      <div className="card-label" style={{ margin: '4px 0 0' }}>Vous avez reçu un lien ?</div>
      <div className="setting-sub">S’il n’ouvre pas l’app, collez-le ici.</div>
      <input
        className="input"
        placeholder="Lien d’invitation"
        value={pasted}
        onChange={e => { setPasted(e.target.value); setError(''); }}
        onKeyDown={e => e.key === 'Enter' && open()}
      />
      {error && <div className="error-text">{error}</div>}
      <button className="btn btn-secondary" disabled={!pasted.trim()} onClick={open}>Ouvrir le lien</button>
    </div>
  );
}

// A link opened (or pasted) is accepted at once: opening it is the yes.
// Shown only while adding, when it fails, or when there is nothing to add.
export function InviteDialog({ invite, onDone }: {
  invite: InviteLink;
  // With the friends once added.
  onDone: (added?: { friends: Friend[]; id: string; name: string }) => void;
}) {
  const [error, setError] = useState('');
  const who = invite.name || 'cette personne';
  const adding = !invite.own && !invite.known;
  // Once: the effect runs twice in development.
  const started = useRef(false);

  useEffect(() => {
    if (!adding || started.current) return;
    started.current = true;
    api.acceptInvite(invite.link).then(
      res => {
        if ('error' in res) setError(ACCEPT_ERRORS[res.error] ?? 'Impossible d’ajouter cet ami.');
        else onDone({ ...res, name: invite.name });
      },
      () => setError('Impossible d’ajouter cet ami.'),
    );
  }, [adding, invite.link, invite.name, onDone]);

  let title;
  let text;
  if (invite.own) {
    title = 'Votre propre lien';
    text = 'C’est un lien que vous avez créé : envoyez-le à un ami pour qu’il vous ajoute.';
  } else if (invite.known) {
    title = `${who} est déjà dans vos amis`;
    text = 'Vous êtes déjà connectés : rien à faire.';
  } else if (error) {
    title = `${who} n’a pas pu être ajouté`;
    text = 'Demandez-lui un nouveau lien d’invitation.';
  } else {
    title = `Ajout de ${who}…`;
    text = 'Vous avez ouvert son lien d’invitation.';
  }

  return (
    <div className="modal-backdrop" onClick={() => onDone()}>
      <div className="modal stack" role="dialog" aria-modal="true" onClick={e => e.stopPropagation()}>
        <div className="modal-title">{title}</div>
        <div className="modal-text">{text}</div>
        {error && <div className="error-text">{error}</div>}
        {(!adding || error) && (
          <button className="btn" autoFocus onClick={() => onDone()}>OK</button>
        )}
      </div>
    </div>
  );
}
