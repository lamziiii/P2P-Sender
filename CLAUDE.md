# P2P Share

App de chat et d'envoi de fichiers en pair-à-pair : Tauri 2 + Rust (iroh/QUIC) pour le cœur, React 19 + Vite pour l'interface.

## Règle n°1 : chaque modification doit marcher sur toutes les plateformes

L'app est distribuée sur **Windows, macOS (Apple Silicon), Linux (AppImage) et Android (arm64)**. Toute modification, même petite, doit être vérifiée pour chacune d'elles avant d'être considérée comme terminée :

- **Une fonction ajoutée existe partout**, ou elle est explicitement absente là où elle ne peut pas exister : un message clair à l'utilisateur, l'option cachée dans l'interface. Jamais de fonction qui casse en silence sur une plateforme.
- **Code Rust propre à un système** : derrière `#[cfg(...)]` (`windows`, `target_os = "macos"`, `target_os = "linux"`, `target_os = "android"`, `desktop`, `mobile`). Vérifier que chaque branche existe et compile, y compris le cas « autres systèmes ».
- **Dépendances** : une nouvelle dépendance doit compiler pour Android et pour les trois systèmes de bureau. Sinon, la limiter avec `[target.'cfg(...)'.dependencies]` dans `src-tauri/Cargo.toml`.
- **Interface** : ce qui diffère sur téléphone passe par `isMobile` / `byPlatform` (`src/platform.ts`) et la classe CSS `.mobile`. L'apparence et le comportement sur PC ne doivent pas changer à cause d'une adaptation mobile, et inversement.
- **Côté Android natif** (`src-tauri/gen/android`) : penser au manifeste (permissions, services), au code Kotlin et aux règles de stockage (fichiers en `content://`, `Download/P2PShare`).
- **Textes** : ils doivent avoir du sens partout. Par exemple « ordinateur » contre « appareil », « cliquez » contre « touchez », « barre des tâches » qui n'existe pas sur téléphone.

### Vérifications à faire

- **Windows** : `cargo check --all-targets` et `cargo test --lib` (voir « Compiler » plus bas), puis `npm run build:web` et `npm run lint`.
- **Android** : compiler le Rust pour `aarch64-linux-android` et produire un APK (procédure plus bas).
- **macOS et Linux** : impossible à compiler sur ce PC. Relire soigneusement chaque `cfg` concerné ; GitHub Actions (`.github/workflows/build.yml`) compile les trois systèmes de bureau à chaque push, et c'est là que la compilation est vraiment confirmée.
- **Dans le compte rendu à l'utilisateur** : dire clairement ce qui a été vérifié sur quelle plateforme, et ce qui ne l'a pas été.

## Compatibilité entre versions

Des amis peuvent garder une ancienne version (3.3, 3.4…). Les messages entre pairs et le journal signé des groupes n'évoluent que par **champs optionnels et nouveaux types de messages**, que les anciennes versions ignorent. Vérifier comment l'ancien code lit une donnée nouvelle avant de l'ajouter.

## Compiler

- **Dossier de compilation** : toujours `CARGO_TARGET_DIR=C:/Users/Mahe/AppData/Local/p2pshare-target`. Windows refuse d'exécuter des programmes compilés dans OneDrive.
- **Smart App Control** bloque parfois des scripts de build fraîchement compilés (`os error 4551`). Supprimer le dossier bloqué sous `p2pshare-target/<profil>/build/` (ou `<cible>/<profil>/build/`), puis relancer.
- **Build PC** : `npm run dist` (voir `scripts/dist.mjs`) produit l'exe portable et l'installeur dans `release/`.
- **Dossiers Android hors OneDrive** : `src-tauri/gen/android/app/build` et `app/src/main/jniLibs` sont des liens vers `%LOCALAPPDATA%\p2pshare-android\` (`app-build`, `jniLibs`). Ne pas supprimer ce dossier ; s'il manque, recréer `app-build` et `jniLibs` dedans.
- **Build Android** : `ANDROID_HOME` et `NDK_HOME` doivent être définis. Sans le mode développeur de Windows, `npx tauri android build` échoue après la partie Rust. Dans ce cas :
  1. Lancer `npx tauri android build --apk --target aarch64` (ajouter `--debug` pour une version de test) : le Rust se compile.
  2. Copier `libp2pshare_lib.so` depuis `p2pshare-target/aarch64-linux-android/<profil>/` vers `src-tauri/gen/android/app/src/main/jniLibs/arm64-v8a/`.
  3. Dans `src-tauri/gen/android`, lancer `gradlew.bat assembleUniversalRelease` (ou `assembleUniversalDebug`) avec `-x rustBuildArm64Release` (ou `-x rustBuildArm64Debug`).

## Release

- **Changelog obligatoire à chaque mise à jour** : chaque version a sa section dans `CHANGELOG.md`, `## x.y.z — <date en français>`, puis une ligne `- ` par changement (le mot-clé en `**gras**`). Elle est écrite pour les utilisateurs : en français, avec « vous », ce qui change pour eux et pas comment c'est codé ; les corrections regroupées sur une ligne ; une note à la fin si les amis doivent aussi mettre à jour. Dès qu'une modification change quelque chose pour les utilisateurs, l'ajouter dans la section de la prochaine version (la créer au besoin, avec « à venir » à la place de la date) sans attendre la release ; mettre la vraie date au moment de la release, le workflow refuse une section sans date. Cette section s'affiche dans l'app après la mise à jour et dans Paramètres → À propos (`src/releaseNotes.ts`), sur la page de la release GitHub et dans les notes de la mise à jour : une release stable sans elle est refusée par le workflow.
- **Version** : `npm run set-version x.y.z` (ou `x.y.z.n`, un quatrième chiffre facultatif à partir de 1) l'écrit dans `package.json` et son lock, `src-tauri/tauri.conf.json`, `src-tauri/Cargo.toml` et `Cargo.lock` (le build utilise `--locked`). Cargo, npm et Tauri ne prennent que trois chiffres : le quatrième y est écrit en métadonnée semver (`3.4.7+2`), mais le tag (`v3.4.7.2`), `CHANGELOG.md` (`## 3.4.7.2 — …`), l'app, les fichiers de la release et winget disent `3.4.7.2`. La mise à jour automatique le compare (`update::is_newer`), et la signature porte `3.4.7+2`, comme `latest.json`. Ne pas utiliser `npm version`, qui retire le quatrième chiffre.
- **Secrets GitHub obligatoires** pour une release stable : `ANDROID_KEYSTORE_*` (APK) et `TAURI_SIGNING_PRIVATE_KEY` / `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` (mises à jour). Sans eux, la publication s'arrête.
- **Clés** : dans `key/` (ignoré par git), ne jamais les déplacer ni les modifier. Vérifier que `key/` reste ignoré avant chaque commit. Perdre la clé de mise à jour bloque les mises à jour automatiques pour toujours.
- **Mises à jour** : les apps lisent `latest.json` de la dernière release (voir `src-tauri/src/update.rs`) ; Android n'en a pas.
- **Tester sans Internet** : `P2PSHARE_NO_INTERNET=1` coupe relais et annuaire ; `cargo run --example nearby_test -- <dossier>` vérifie que deux amis se trouvent par le réseau local, `cargo run --example invite_test -- <dossier>` que les liens d'invitation marchent (et qu'un lien déjà utilisé est refusé).
- **Liens d'invitation** : la page `docs/ami/index.html` (GitHub Pages) ouvre l'app par `p2pshare://`. Le manifeste Android porte le bloc du plugin deep-link (`DEEP LINK PLUGIN`), que le build réécrit : ne pas le modifier à la main. Les liens ne marchent qu'avec une app installée (pas en `tauri dev`).
- **Tester le relais personnel** (avec le `p2p-relay` de P2P-Sender-relay compilé) : `relay_test` (relais en priorité, secours public, retour) et `tunnel_test` (réseau qui bloque iroh simulé par `P2PSHARE_BLOCK_PUBLIC=1`, accès aux relais publics par le tunnel). `P2PSHARE_RELAY_ONLY=1` coupe les chemins directs pour que tout passe par les relais.
- **Relais (projet P2P-Sender-relay)** : une modification de l'app qui dépend du relais (tunnel, annuaire) demande aussi de mettre à jour ce projet, son README et son image Docker.

## Conventions

- **Attribution** : ne jamais mentionner Claude comme auteur ou co-auteur, nulle part : commits, PR, fichiers, notes de version.
- **Commentaires du code** : en anglais, courts.
- **Interface** : textes en français, avec « vous ».
- **Style** : suivre celui du code existant (densité de commentaires, noms, structure).
