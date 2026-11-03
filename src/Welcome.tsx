import { useState } from 'react';
import { api } from './api';
import { BackupDialog } from './Backup';
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
            <div className="subtitle">Bienvenue</div>
          </div>
        </div>
        {!isMobile && <HideButton />}
      </div>

      <div className="content welcome">
        {step === 0 && (
          <>
            <div className="welcome-title">Bienvenue dans P2P Share</div>
            <div className="welcome-text">
              Discutez et envoyez des fichiers à vos amis, directement {byPlatform('d’ordinateur à ordinateur', 'd’appareil à appareil')}, sans serveur qui les garde.
            </div>
            <div className="card">
              <div className="card-label">Votre pseudo</div>
              <input
                className="input"
                autoFocus
                maxLength={40}
                placeholder="Votre pseudo"
                value={name}
                onChange={e => setName(e.target.value)}
                onKeyDown={e => e.key === 'Enter' && saveName()}
              />
              <div className="setting-sub" style={{ marginTop: 6 }}>Vos amis et les membres de vos groupes le verront.</div>
            </div>
            <button className="btn" style={{ width: '100%' }} disabled={!name.trim()} onClick={saveName}>Continuer</button>
            <button className="btn-link" style={{ alignSelf: 'center' }} onClick={() => setRestoring(true)}>Restaurer une sauvegarde</button>
            {restoring && <BackupDialog mode="import" onClose={() => setRestoring(false)} />}
          </>
        )}

        {step === 1 && (
          <>
            <div className="welcome-title">Invitez vos amis</div>
            <div className="welcome-text">
              Créez un lien d’invitation et envoyez-le à un ami : il l’ouvre, accepte, et vous voilà amis. Vous en créerez d’autres dans l’onglet Amis.
            </div>
            <div className="card">
              <InviteLinkMaker />
            </div>
            <div className="welcome-text">
              {byPlatform(
                'Astuce : glissez un fichier sur un ami pour le lui envoyer.',
                'Astuce : touchez l’icône d’envoi à côté d’un ami pour lui envoyer un fichier.',
              )}
            </div>
            <button className="btn" style={{ width: '100%' }} onClick={finish}>Commencer</button>
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
