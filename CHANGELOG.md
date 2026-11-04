# Nouveautés de P2P Share

## 3.4.8 — à venir

- **Les appareils LocalSend se présentent comme vos amis** : un clic ouvre la discussion, avec le dernier message, l'heure et les messages non lus. Le bouton d'envoi de fichiers reste sur la ligne, et le menu « … » permet de couper les notifications et de supprimer la discussion. Elle n'est effacée que chez vous : les messages restent dans l'app LocalSend de l'autre appareil. Dans la discussion, le trombone envoie un fichier.
- **Envoyer à un ami** : le bouton d'envoi, sur sa ligne, propose un fichier ou un dossier, comme le trombone de la discussion.
- Corrections : une fenêtre épinglée reste au premier plan quand vous cliquez ailleurs, au lieu de passer derrière les autres fenêtres.

## 3.4.7 — 4 novembre 2026

- **Réseau local désactivable** : dans Paramètres → Réseau, un bouton coupe l'usage du réseau local par l'app. Vous n'y êtes plus visible, LocalSend s'arrête, et vos amis sur le même réseau ne sont plus joints que par Internet, sans la connexion plus rapide. L'app redémarre pour appliquer le changement.
- **La sauvegarde passe dans Paramètres → Avancé**, avec votre identifiant P2P.

## 3.4.6 — 3 novembre 2026

- **Invitez vos amis avec un lien** : dans l'onglet Amis (bouton +), créez un lien d'invitation et envoyez-le par mail ou message. Votre ami clique dessus, P2P Share s'ouvre et lui demande s'il veut vous ajouter : vous voilà amis tous les deux, sans copier d'identifiant. Un lien sert à une seule personne et reste valable 7 jours. S'il n'ouvre pas l'app, il se colle dans l'onglet Amis.
- **Votre identifiant P2P passe dans Paramètres → Avancé**, avec l'ajout par identifiant, pour vos amis restés en 3.4.5 ou avant.
- **Mises à jour automatiques** : une mise à jour attend la fin d'un transfert commencé pendant son téléchargement, au lieu de le couper en redémarrant.
- **Vos amis n'apparaissent plus en double** : un ami sur le même réseau n'est plus listé aussi parmi les appareils LocalSend à proximité (il lui faut la 3.4.5 ou plus).
- **Plus de types de programmes reconnus** (scripts Python, consoles Windows, scripts et processus macOS…) : ils ne sont jamais acceptés sans vous demander, même d'un ami de confiance.
- Corrections : sur Mac, la fenêtre s'ouvre là où vous êtes, même par-dessus une app en plein écran, au lieu de vous ramener sur le bureau et de demander un second clic ; un téléphone Android apparaît comme un téléphone, et non comme un ordinateur Linux, sur les autres appareils LocalSend.

Les liens d'invitation demandent la 3.4.6 des deux côtés ; avec un ami resté en 3.4.5 ou avant, échangez vos identifiants (Paramètres → Avancé).

## 3.4.5 — 3 novembre 2026

- **Mises à jour automatiques** (Windows, macOS, Linux) : l'app trouve toute seule les nouvelles versions et vous les propose au-dessus de la liste d'amis et dans Paramètres → À propos, avec un bouton pour vérifier. Vous pouvez aussi les laisser s'installer seules, une fois les transferts terminés. Seules les versions signées par le projet sont installées. Sur Android, un bouton ouvre la page des versions.
- **Vos amis sur le même réseau, même sans Internet** : l'app se signale sur le réseau local et se connecte directement aux amis qu'elle y trouve, même quand les relais sont bloqués.
- **Votre relais en priorité** : avec un relais personnel, vos amis qui l'ont aussi échangent avec vous par lui, plus vite que par les relais publics. Vos amis qui ne l'ont pas restent joignables par les relais publics, qui prennent aussi le relais si le vôtre ne répond pas.
- **Votre relais ouvre la porte des relais publics** : sur un réseau qui les bloque (eduroam…), l'app les joint à travers votre relais, et vous joignez vos amis qui n'ont rien configuré. Elle essaie toujours d'abord en direct. Il faut la nouvelle version de votre relais (P2P-Sender-relay).
- **Voir comment vous êtes connecté** : une discussion indique si votre ami est sur le réseau local, joint en direct ou par un relais. Quand les relais ne répondent pas, l'en-tête l'indique, et précise si le réseau local fonctionne encore.
- **Paramètres plus clairs**, rangés par section : profil, général, discussions et notifications, sauvegarde, réseau, à propos.
- **LocalSend toujours activé** : il sert aussi à trouver vos amis sur le réseau local.
- **Nouveautés** : cette liste s'affiche après chaque mise à jour et reste dans Paramètres → À propos.
- Corrections : l'état de la connexion ne déborde plus de l'en-tête, et les longs messages d'erreur passent à la ligne.

Vos amis en 3.4 ou avant échangent toujours avec vous ; pour se trouver sans Internet, il faut la 3.4.5 des deux côtés. Installez cette version une fois à la main : les suivantes arriveront toutes seules.

## 3.4.0 — 2 novembre 2026

- **Android** : première version pour téléphone (arm64), qui peut rester joignable en arrière-plan.
- **Discussions** : répondre à un message, réagir avec un emoji, voir quand un message a été lu et quand un ami écrit (désactivable), rechercher dans une discussion, liens cliquables, images affichées avec un aperçu.
- **Dossiers et transferts** : envoyer un dossier entier en gardant ses sous-dossiers, glisser-déposer sur un ami, un groupe ou un appareil LocalSend, mettre en pause et reprendre, limiter la vitesse, accepter automatiquement les fichiers d'un ami (les programmes demandent toujours). Les dossiers de petits fichiers passent environ 20 fois plus vite.
- **Groupes** : mentions @, messages et fichiers épinglés, groupes silencieux, dossiers partagés.
- **Notifications** : réglages par type, par discussion et par groupe, et mode Ne pas déranger.
- **Vos données** : historique et identité chiffrés sur le disque, sauvegarde et restauration de votre identité dans un fichier protégé par un mot de passe.
- **Plus simple** : accueil au premier lancement, pseudos entre amis, dernier message dans la liste, couleurs de OneDrive, fenêtre qui se ferme quand vous cliquez ailleurs (épinglez-la pour y glisser des fichiers).
- **Plus léger** : l'app se met en veille quand elle ne fait rien, et les groupes utilisent environ trois fois moins de mémoire.
- Nombreuses corrections : enregistrements plus sûrs, envois et annulations qui arrivent toujours, LocalSend et groupes plus fiables.

## 3.3.0 — 18 octobre 2026

- **Compatibilité LocalSend** : échangez des fichiers avec l'app LocalSend (téléphones, autres ordinateurs) sur le même réseau. Chaque fichier reçu vous est proposé avant d'être enregistré.
- **Discussion avec les appareils LocalSend** : envoyez et recevez des messages texte, gardés d'une fois sur l'autre.
- **Transferts plus rapides sur le réseau local** : entre amis sur le même réseau, les fichiers passent par une connexion directe et chiffrée, environ 1,5 fois plus vite.

## 3.2.0 — 8 octobre 2026

- **Relais personnel** : pour les réseaux qui bloquent les relais publics (eduroam…), indiquez l'adresse de votre propre relais dans les paramètres.

## 3.1.0 — 7 octobre 2026

- **Groupes** : discussion de groupe, invitations par les membres, et dépôt de fichiers partagés que chacun télécharge quand il veut. Les messages arrivent même aux membres qui étaient hors ligne.

## 3.0.0 — 7 octobre 2026

- **Nouvelle version, réécrite de zéro** : un installeur d'environ 3 Mo au lieu de 93 Mo, beaucoup moins de mémoire utilisée, et des transferts environ 1,8 fois plus rapides.
- Votre ID, vos amis, vos messages et vos réglages de la version 2 sont repris au premier lancement.
- La version 3 n'échange pas avec les versions 2.x : vos amis doivent aussi passer à la version 3.
