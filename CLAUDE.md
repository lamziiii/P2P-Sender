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
- **Build Android** : `ANDROID_HOME` et `NDK_HOME` doivent être définis. Sans le mode développeur de Windows, `npx tauri android build` échoue après la partie Rust. Dans ce cas :
  1. Lancer `npx tauri android build --apk --target aarch64` (ajouter `--debug` pour une version de test) : le Rust se compile.
  2. Copier `libp2pshare_lib.so` depuis `p2pshare-target/aarch64-linux-android/<profil>/` vers `src-tauri/gen/android/app/src/main/jniLibs/arm64-v8a/`.
  3. Dans `src-tauri/gen/android`, lancer `gradlew.bat assembleUniversalRelease` (ou `assembleUniversalDebug`) avec `-x rustBuildArm64Release` (ou `-x rustBuildArm64Debug`).

## Conventions

- **Attribution** : ne jamais mentionner Claude comme auteur ou co-auteur, nulle part : commits, PR, fichiers, notes de version.
- **Commentaires du code** : en anglais, courts.
- **Interface** : textes en français, avec « vous ».
- **Style** : suivre celui du code existant (densité de commentaires, noms, structure).
