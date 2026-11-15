import { useState } from 'react';
import { api } from './api';
import { BackupDialog } from './Backup';
import { tr } from './i18n';
import { Icon } from './icons';
import { InviteLinkMaker } from './Invite';
import { HideButton } from './PinButton';
import { byPlatform, isMobile } from './platform';

// First launch: the nickname, then how to invite friends.
export function Welcome({ nickname, onNickname, onDone }: {
  nickname: string;
  onNickname: (nickname: string) => void;
  onDone: () => void;
}) {
  const [step, setStep] = useState(0);
  const [name, setName] = useState(nickname);
  const [restoring, setRestoring] = useState(false);

  const saveName = async () => {
    if (!name.trim()) return;
    onNickname(await api.setNickname(name));
    setStep(1);
  };

  const finish = () => {
    api.finishWelcome();
    onDone();
  };

  return (
    <div className="app">
      <div className="header">
        <div className="brand">
          <div className="brand-icon"><Icon name="sync" size={18} /></div>
          <div className="header-title">
            <div className="title">P2P Share</div>
            <div className="subtitle">{tr('Bienvenue', 'Welcome')}</div>
          </div>
        </div>
        {!isMobile && <HideButton />}
      </div>

      <div className="content welcome">
        {step === 0 && (
          <>
            <div className="welcome-title">{tr('Bienvenue dans P2P Share', 'Welcome to P2P Share')}</div>
            <div className="welcome-text">
              {tr(
                `Discutez et envoyez des fichiers à vos amis, directement ${byPlatform('d’ordinateur à ordinateur', 'd’appareil à appareil')}, sans serveur qui les garde.`,
                `Chat and send files to your friends, straight from ${byPlatform('computer to computer', 'device to device')}, with no server keeping them.`,
              )}
            </div>
            <div className="card">
              <div className="card-label">{tr('Votre pseudo', 'Your nickname')}</div>
              <input
                className="input"
                autoFocus
                maxLength={40}
                placeholder={tr('Votre pseudo', 'Your nickname')}
                value={name}
                onChange={e => setName(e.target.value)}
                onKeyDown={e => e.key === 'Enter' && saveName()}
              />
              <div className="setting-sub" style={{ marginTop: 6 }}>{tr('Vos amis et les membres de vos groupes le verront.', 'Your friends and the members of your groups will see it.')}</div>
            </div>
            <button className="btn" style={{ width: '100%' }} disabled={!name.trim()} onClick={saveName}>{tr('Continuer', 'Continue')}</button>
            <button className="btn-link" style={{ alignSelf: 'center' }} onClick={() => setRestoring(true)}>{tr('Restaurer une sauvegarde', 'Restore a backup')}</button>
            {restoring && <BackupDialog mode="import" onClose={() => setRestoring(false)} />}
          </>
        )}

        {step === 1 && (
          <>
            <div className="welcome-title">{tr('Invitez vos amis', 'Invite your friends')}</div>
            <div className="welcome-text">
              {tr(
                'Créez un lien d’invitation et envoyez-le à un ami : il l’ouvre, et vous voilà amis. Vous en créerez d’autres dans l’onglet Amis.',
                'Create an invitation link and send it to a friend: they open it, and you are friends. You can create more in the Friends tab.',
              )}
            </div>
            <div className="card">
              <InviteLinkMaker />
            </div>
            <div className="welcome-text">
              {byPlatform(
                tr('Astuce : glissez un fichier sur un ami pour le lui envoyer.', 'Tip: drop a file on a friend to send it to them.'),
                tr('Astuce : touchez l’icône d’envoi à côté d’un ami pour lui envoyer un fichier.', 'Tip: tap the send icon next to a friend to send them a file.'),
              )}
            </div>
            <button className="btn" style={{ width: '100%' }} onClick={finish}>{tr('Commencer', 'Get started')}</button>
          </>
        )}

        <div className="welcome-dots">
          <span className={step === 0 ? 'active' : ''} />
          <span className={step === 1 ? 'active' : ''} />
        </div>
      </div>
    </div>
  );
}
