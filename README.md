# P2P Share - Peer-to-Peer File Transfer & Chat

P2P Share is a lightweight, privacy-focused app built with **Tauri**, **Rust** and **React**, for Windows, macOS, Linux and Android. Friends chat and send each other files directly, without any server storing them, over end-to-end encrypted [iroh](https://github.com/n0-computer/iroh) (QUIC) connections that cross NATs. On the same network, files take a faster direct path, and the app also talks to the [LocalSend](https://localsend.org) app on phones and other computers.

![P2P Share UI](icon.png)

## Install

Download the installer for your system from the [latest release](https://github.com/lamziiii/P2P-Sender/releases/latest):

- **Windows**: `P2PShare_*_x64-setup.exe`
- **macOS** (Apple Silicon and Intel): `P2PShare_*_universal.dmg`
- **Linux**: `P2PShare_*_amd64.AppImage`, or `P2PShare_*_amd64.deb` for Debian and Ubuntu (`sudo apt install ./P2PShare_*_amd64.deb`)
- **Android** (arm64): `P2PShare_*_android-arm64.apk`

Once installed, the app updates itself on every system. Each release lists the SHA-256 of its files in `SHA256SUMS.txt`.

**Windows**: the installers are not signed yet, so SmartScreen may ask you to confirm, and Smart App Control may block them.

**macOS**: the app is not notarized by Apple, so macOS refuses to open it the first time. Right-click the app in Applications → Open, or run `xattr -cr /Applications/P2PShare.app`. It lives in the menu bar (no Dock icon). On macOS 15 and later, allow it on the local network when asked: LocalSend and the fast local-network path need it.

**Linux**: the `.deb` installs the app in the applications menu, with its dependencies. The AppImage needs FUSE 2 (`sudo apt install libfuse2` on Ubuntu 22.04 and later), then `chmod +x P2PShare_*.AppImage`. GNOME shows tray icons only with the AppIndicator extension; without it, launch the app again to bring its window back. On Wayland the window opens where the compositor puts it.

**Android**: allow your browser or file manager to install apps when Android asks. Updates are then installed from the app, with your confirmation.

## Features

### Chat

- End-to-end encrypted messages with delivery and read receipts; messages written while a friend is offline are delivered when they reconnect.
- Reply to a message, react with an emoji, see when a friend is typing (can be turned off), search a conversation, clickable web links.
- Pictures (jpg, png, gif, webp, bmp) shown in the conversation, with a preview.
- Nicknames between friends, and a conversation list with the last message and unread messages.

### Files and folders

- Files of any size and whole folders, keeping their tree; several at once, each one accepted by the receiver first.
- Drag and drop on a friend, a group or a LocalSend device, or paste a screenshot or copied files in a chat.
- Pause and resume, speed limit, and automatic resume after a network drop or a restart of the app.
- Accept a friend's files automatically if you trust them; programs and scripts always ask.
- The transfer list shows whether a file goes over the local network, directly or through a relay.

### Send with P2P Share

Send a file or a folder without opening the app first: right-click it in the Windows File Explorer, "Open with" → P2P Share in the macOS Finder or a Linux file manager, or the Share button of any Android app. P2P Share opens and asks who to send it to: a friend, a group or a LocalSend device. Text or a link shared on Android is sent as a message. On Windows and Linux the entry can be removed in Settings → General.

### Groups

- Group chats of up to 32 members, whose members do not need to be friends with each other.
- A shared file repository per group, with folders that keep their tree.
- @mentions, pinned messages and files, silent groups.

### LocalSend

- Send and receive files with the LocalSend app on phones and computers of the same network.
- LocalSend devices are listed with your friends, each with its own chat.

### Fast on the local network

When a friend is on the same network, files go over a direct connection instead of the internet path, and friends find each other there even without the internet. The local network can be turned off in Settings → Network.

### Privacy

- Your ID is your public key (64 hex characters). Only your friends, the members of your groups and someone holding one of your open invitation links can connect, and every connection is checked against their key.
- Friends add each other with an invitation link sent by mail or message, or by ID.
- History and identity are encrypted on disk. Back up your identity to a password-protected file: without a backup, your ID cannot be recovered after reinstalling the system.
- Your own relay, for networks that block iroh's public relays (eduroam and some campus networks).

### The app

- In French or English: it follows the system language, or the one chosen in Settings → General.
- Tray app with a Windows 11 (Fluent) look, dark and light themes, optional start with the computer.
- Clickable notifications, set by kind, per chat and per group, and Do Not Disturb.
- Automatic updates, signed, that wait for running transfers to end.
- Light on resources: the app sleeps when idle, and uses about 9 MB of memory while hidden in the tray.

The list of changes for users, version by version, is in [CHANGELOG.md](CHANGELOG.md) (French) and [CHANGELOG.en.md](CHANGELOG.en.md) (English); the app shows it after each update and in Settings → About.

## How it works

### Local network

- Friends tell each other, over their authenticated iroh connection, the fingerprint of a certificate kept for this (`lan.json` in the app data folder), a port and their private IPv4 addresses.
- The sender connects to an address of the friend over TCP + TLS, the same kind of connection LocalSend uses, and faster than QUIC through iroh. Each side checks the other's certificate against the fingerprint it was told, so nobody else on the network can read the file or pass for your friend. When the sender cannot reach the receiver (its firewall), it asks the receiver to connect to it instead.
- Offers, answers, cancelling and resuming still go through iroh. If the direct connection cannot be made or breaks, the transfer goes on through iroh from where it stopped; a file that started over iroh moves to the local network once the friend is found there.
- The app announces itself through the LocalSend announcements and connects to the friends it finds there, even when the relays and the address directory are blocked.
- The Windows installer adds a firewall rule for the app on every network profile, on a first install. On Android, the Wi-Fi stays at full speed and the processor awake while files move.

### LocalSend compatibility

- The computer shows up in LocalSend under your nickname. Each incoming file is offered for you to accept, like files from friends.
- The devices found appear in the Friends tab, to send them files or open their chat. When we send, the device's certificate must match the fingerprint it announced.
- Text sent from the LocalSend app lands in the device's chat, and what you write there reaches the device as a LocalSend text message. Conversations are kept in `localsend-chats.json`. Messages from older LocalSend versions, which prove nothing about who sent them, are marked unverified.
- It speaks LocalSend protocol v2.2 over HTTPS with a self-signed certificate (`localsend.json` in the app data folder), on port 53317 (TCP and UDP multicast), or another TCP port if that one is taken, for example by the LocalSend app itself.
- Allow the app through the firewall, on public networks too (campus Wi-Fi is often one). Networks that isolate their clients (guest Wi-Fi, some campus networks) keep devices from seeing each other.

### Groups

- Each group is a signed, append-only log: messages, members added or removed, renames, nicknames and shared files are entries signed by their author.
- Members exchange what they are missing whenever they connect, so a message reaches everyone, even members who were offline while its author has since left.
- Every member can invite their friends and remove members. Concurrent changes resolve the same way for everyone (the earliest wins).
- The file repository lists the files; each member downloads what they want, from any member who has the file, and the download is checked against its BLAKE3 hash. Shared files stay where they are on the sharer's disk.
- Members who are not your friends see you under the nickname set in the settings.

### Transfers after a restart

Unfinished transfers with friends are kept in `transfers.json` (encrypted like the chats, `src-tauri/src/p2p/resume.rs`) and come back paused; the sender offers them again and the receiver answers with what its `.part` file holds. A source file changed meanwhile is not resumed. Shared files of groups and LocalSend transfers are not kept. Pasted screenshots and files are kept for 30 days in `pasted/` of the app data folder, for transfers that wait or resume.

### Self-hosted relay

Some networks (eduroam behind a FortiGate, for example) block iroh's public relays and address directory, so friends cannot reach each other. Run [P2P-Sender-relay](https://github.com/lamziiii/P2P-Sender-relay) behind an HTTPS reverse proxy, then enter its address (and token) in Settings → Network → Personal relay.

- It comes first: it is the home relay whenever it answers, so friends who use it too exchange through it.
- Friends elsewhere are still reached on their own relays, directly where the network allows and otherwise through your relay's tunnel (`src-tauri/src/p2p/tunnel.rs`: iroh goes through a local proxy that checks once per relay and network whether it answers directly). Friends are also found through your relay's directory, which passes records to iroh's.
- While it does not answer, the public relays stand in, and the app checks every minute whether it is back.

Friends who want to reach you from a blocking network enter the same relay; friends on the same local network find each other without any relay.

### Send with P2P Share

Every system starts the app with `p2pshare --send <paths>`, and the running instance gets them through `tauri-plugin-single-instance` (`src-tauri/src/share.rs`, `src/Share.tsx` for the dialog):

- **Windows**: the app writes the `Software\Classes\*\shell\P2PShare` and `Software\Classes\Directory\shell\P2PShare` keys of the current user at start; the uninstaller removes them.
- **Linux**: the app writes `~/.local/share/applications/p2pshare-send.desktop` (hidden from the menu, listed in "Open with").
- **macOS**: `CFBundleDocumentTypes` in `src-tauri/Info.plist`; the files arrive as an `Opened` event.
- **Android**: an intent filter for `SEND` and `SEND_MULTIPLE`; shared `content://` files are copied before sending.

## Development

### Prerequisites

- [Node.js](https://nodejs.org/) 20.19+ or 22.12+ (required by Vite 8)
- [Rust](https://rustup.rs/) (stable)
- Linux only: `libwebkit2gtk-4.1-dev libayatana-appindicator3-dev librsvg2-dev libxdo-dev libssl-dev`

### Run

```bash
git clone https://github.com/lamziiii/P2P-Sender.git
cd P2P-Sender
npm install
npm run dev
```

### Build

```bash
npm run dist
```

The app and its installer are copied to `release/`. Cargo's working files go to `$CARGO_TARGET_DIR` when it is set, otherwise to `%LOCALAPPDATA%\p2pshare-target` on Windows and `~/.cache/p2pshare-target` on macOS and Linux: several GB that do not belong in the project folder, and Windows refuses to run the build scripts Cargo compiles inside a OneDrive folder.

### Languages

The interface is written in French and English: `tr('…', '…')` from `src/i18n.ts` in the React UI, `lang::tr` and `trf!` from `src-tauri/src/lang.rs` for notifications, the tray menu and errors, and `Lang.tr` on the Android side (`Lang.kt`). The invitation page carries its English text in `data-en` attributes. Each version's notes go in both `CHANGELOG.md` and `CHANGELOG.en.md`.

### Release

Set the version with `npm run set-version <x.y.z>`: it writes it in package.json and its lock, `src-tauri/tauri.conf.json`, `src-tauri/Cargo.toml` and `Cargo.lock` (builds use `--locked`). A fourth number is optional (`npm run set-version 3.4.7.2`, from .1): Cargo, npm and Tauri take three numbers only, so the files hold `3.4.7+2` (semver build metadata, which Tauri also turns into the Windows version 3.4.7.2), while the tag, `CHANGELOG.md`, the app and the release files say 3.4.7.2, and the updater counts it as newer than 3.4.7.

Push a version tag (`git tag v3.4.7 && git push origin v3.4.7`, or `v3.4.7.2`): GitHub Actions checks that the tag matches that version, builds the installers for the three systems (a universal macOS app for Apple Silicon and Intel) and the APK, signs the update files, publishes the release with `latest.json` last (what installed apps read to update); see `.github/workflows/build.yml`.

- A stable release stops before publishing if the APK or the update signing key is missing, and before building if `CHANGELOG.md` has no `## x.y.z — <date>` section for it (until the release, the section of the next version says « à venir » instead of the date). That section becomes the release notes, the notes shown before installing an update, and what the app shows once updated.
- A prerelease tag (`v3.4.0-beta.1`) is published as a prerelease, not marked latest.

### Invitation links

A link is `https://lamziiii.github.io/P2P-Sender/ami/#i=<ID>&c=<code>&n=<nickname>`: a page of this repository (`docs/ami/index.html`, published by GitHub Pages: Settings → Pages → Deploy from a branch, `main`, folder `/docs`) that opens the app with `p2pshare://ami?…`, since mail and chat apps only make web links clickable. The part after `#` never reaches the server. The app registers the `p2pshare` scheme through `tauri-plugin-deep-link` (the installers, and the app itself at start on Windows and Linux for an AppImage or a moved app; Info.plist on macOS; an intent filter on Android).

The code, made for one link (`src-tauri/src/p2p/invites.rs`), is kept 7 days in `invites.json` (encrypted), with at most 20 links open at once (a new one drops the oldest); backups leave it out, so a restore never makes a used code valid again. Opening a link only shows the maker (nickname, start of the ID, a warning when a friend already has that nickname): a web page or another app can open one. Whoever accepts it adds its maker and sends the code in the hello of its connection (or first thing on a connection the maker opens, as a co-member of a group), and the maker, who lets in a peer that is not a friend only with a valid code, adds them in turn. Link format: `src-tauri/src/invite.rs`.

### Android

GitHub Actions builds an arm64 APK on every push, downloadable from the Actions tab as `Test-android`: it is signed with a temporary key, so each build must replace the previous one by uninstalling it first. To attach an APK to releases, create a release key once and keep it safe (without it, the app can never be updated), then add the secrets `ANDROID_KEYSTORE_BASE64` (the keystore in base64), `ANDROID_KEYSTORE_PASSWORD`, `ANDROID_KEY_ALIAS` and `ANDROID_KEY_PASSWORD`.

### Updates

The app looks for a newer release a minute after starting and then every 6 hours, and offers it over the friend list and in Settings → About (which also has a check button). It reads `latest.json` from the latest release.

- **Windows, macOS and Linux**: installing saves everything and restarts the app. With automatic installs on, it installs by itself once no transfer runs. Only files signed with the updater key are installed. An app installed from the `.deb` takes the `.deb` (`linux-x86_64-deb`), which asks for the administrator password.
- **Android**: `latest.json` gives the APK's address (`android.url`); the app downloads it and opens the system installer, which checks that the APK is signed with the same key as the installed app.

A stable release needs the secrets `TAURI_SIGNING_PRIVATE_KEY` (the private key from `npx tauri signer generate`) and `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`; its public key is in `src-tauri/tauri.conf.json`. The signatures go into `latest.json`. Keep the private key safe: without it, installed apps cannot be updated any more.

### Code signing

Windows release builds are signed through [SignPath](https://signpath.io) once the repository has its settings; without them, the installers are built unsigned. The workflow signs `p2pshare.exe` first, builds the installer around it (`tauri bundle`), then signs the installer. It needs:

- the secret `SIGNPATH_API_TOKEN` (a SignPath API token of a CI user allowed to submit signing requests);
- the repository variables `SIGNPATH_ORGANIZATION_ID`, `SIGNPATH_PROJECT_SLUG` and `SIGNPATH_SIGNING_POLICY_SLUG`;
- on SignPath, a trusted build system for GitHub.com linked to the project, and an artifact configuration that signs the `.exe` at the root of the artifact.

macOS release builds are signed with an Apple Developer ID and notarized once the repository has these secrets (an Apple Developer Program membership is needed); without them, the app keeps its ad hoc signature and macOS warns on first opening:

- `APPLE_CERTIFICATE` (the Developer ID Application certificate exported as `.p12`, in base64) and `APPLE_CERTIFICATE_PASSWORD`;
- `APPLE_SIGNING_IDENTITY` (its name, such as `Developer ID Application: Name (TEAMID)`);
- for notarization, `APPLE_ID`, `APPLE_PASSWORD` (an app-specific password) and `APPLE_TEAM_ID`.

Only version tags are signed, not every push or pull request.

### Tests and benchmark

```bash
cargo test --lib --manifest-path src-tauri/Cargo.toml
cargo run --release --example groups_test --manifest-path src-tauri/Cargo.toml -- <workdir>
cargo run --example localsend_receiver --manifest-path src-tauri/Cargo.toml -- <workdir> [seconds]
cargo run --example localsend_send --manifest-path src-tauri/Cargo.toml -- <workdir> <device name> <file>... [--cancel-at <percent>]
cargo run --example localsend_send --manifest-path src-tauri/Cargo.toml -- <workdir> <device name> --text <message>
cargo run --example nearby_test --manifest-path src-tauri/Cargo.toml -- <workdir> [sans-local]
cargo run --example invite_test --manifest-path src-tauri/Cargo.toml -- <workdir>
cargo run --example relay_test --manifest-path src-tauri/Cargo.toml -- <workdir> <path to P2P-Sender-relay's p2p-relay>
cargo run --example tunnel_test --manifest-path src-tauri/Cargo.toml -- <workdir> <path to P2P-Sender-relay's p2p-relay>
cargo run --release --example scenarios --manifest-path src-tauri/Cargo.toml -- <workdir> [features]
cargo build --release --example bench --manifest-path src-tauri/Cargo.toml
pwsh bench/run.ps1 -File <some big file>
```

- `groups_test` runs three nodes (two of them not friends) through invitations, group chat, delivery to a member who was offline, shared-file downloads from another member, and removal.
- `localsend_receiver` runs a node with LocalSend on that accepts every file (except names containing `refuse`, declined, or `attente`, left unanswered; it cancels names containing `annule` midway), to send to from a LocalSend app or the LocalSend CLI. `localsend_send` sends files, or a text message, to a device by name and reports the outcome.
- `tunnel_test` puts Alice on a simulated blocking network (`P2PSHARE_BLOCK_PUBLIC=1`: no direct access to iroh's relays and directory) with her relay, and Bob on public relays only: Alice finds Bob through her relay's directory, reaches his relay through the tunnel, and sends him a file.
- `relay_test` checks that your own relay comes first: with it off, a node falls back to a public relay; once it is started, the node comes back to it, and two friends on it send a file (with `P2PSHARE_RELAY_ONLY=1`, without direct paths, so through the relay).
- `nearby_test` runs two friends that find each other through the LocalSend announcements and send a file; with `P2PSHARE_NO_INTERNET=1` they have no relay and no address directory, as on a network that blocks them, and still connect over the local network.
- `scenarios` runs two nodes in one process through delivery to a friend who was offline and transfers cut by a disconnection; with `features`, a quick pass over replies, reactions, read receipts, typing, folders, pausing by hand and the speed limit.
- `invite_test` runs three nodes without internet: Alice makes a link, Bob accepts it and both become friends; Carol tries the same link, is turned away, and her app shows that the link no longer works.
- `bench` sends a file (or several, or a folder; `BENCH_COUNT` tells the receiver how many files to wait for) between two headless nodes and reports the throughput; set `BENCH_NO_LAN=1` to compare without the local-network path. `bench/run.ps1` runs it and also reports peak memory and CPU time.

## Project layout

- `src/`: React UI (`api.ts` wraps the Tauri commands and events, `i18n.ts` the two languages).
- `src/Groups.tsx`: groups UI; `src/LocalSendDevices.tsx`, `src/LocalSendChat.tsx`: LocalSend devices and their chat; `src/RelaySettings.tsx`: self-hosted relay setting; `src/Share.tsx`: the "Send with P2P Share" dialog; `src/Update.tsx`: updates.
- `src-tauri/src/p2p.rs`: P2P core (connections, chat, transfers).
- `src-tauri/src/p2p/lan.rs`: the local-network path for transfers; `src-tauri/src/p2p/resume.rs`: transfers kept across restarts.
- `src-tauri/src/group.rs`: group log (signed entries, replay, sync state); `src-tauri/src/p2p/groups.rs`: group networking (invitations, sync, shared files).
- `src-tauri/src/localsend.rs`: LocalSend compatibility (discovery, HTTPS server); `src-tauri/src/localsend/send.rs`: sending files and text; `src-tauri/src/p2p/external.rs` puts their files in the transfer list.
- `src-tauri/src/lib.rs`: window, tray, notifications, settings, LocalSend chats, commands.
- `src-tauri/src/share.rs`: "Send with P2P Share" on each system; `src-tauri/src/update.rs`: updates; `src-tauri/src/lang.rs`: French and English texts.
- `src-tauri/src/notify.rs`: clickable native notifications per platform.
- `src-tauri/gen/android`: the Android app (manifest, Kotlin plugin and service).
- `scripts/dist.mjs`: local build, copied to `release/`.

## Built With

- **Frontend**: [React 19](https://react.dev/), [Vite](https://vitejs.dev/)
- **App**: [Tauri 2](https://tauri.app/)
- **P2P networking**: [iroh](https://github.com/n0-computer/iroh); [rustls](https://github.com/rustls/rustls) for the local-network path and LocalSend
- **Style**: Vanilla CSS (Fluent / Windows 11 look)
- **Languages**: Rust, TypeScript, Kotlin, HTML, CSS

## License

This project is licensed under the MIT License - see the [LICENSE](LICENSE) file for details.

---

Built by [lamziiii](https://github.com/lamziiii)
