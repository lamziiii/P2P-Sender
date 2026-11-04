# P2P Share - Peer-to-Peer File Transfer & Chat

P2P Share is a lightweight, privacy-focused desktop app built with **Tauri**, **Rust** and **React**. Friends chat and send each other files directly, without any server storing them, over end-to-end encrypted [iroh](https://github.com/n0-computer/iroh) (QUIC) connections that cross NATs. On the same network, files take a faster direct path, and the app also talks to the [LocalSend](https://localsend.org) app on phones and other computers.

![P2P Share UI](icon.png)

## Install

**Windows** (winget):

```powershell
winget install lamzii.P2PShare
```

Or download the installer for your system from the [latest release](https://github.com/lamziiii/P2P-Sender/releases/latest):

- **Windows**: `P2PShare_*_x64-setup.exe`
- **macOS** (Apple Silicon): `P2PShare_*_aarch64.dmg`
- **Linux**: `P2PShare_*_amd64.AppImage`

The installers are not signed yet: Windows SmartScreen may ask you to confirm, and Smart App Control may block them.

**macOS**: the app is not notarized by Apple, so macOS refuses to open it the first time. Right-click the app in Applications → Open, or run `xattr -cr /Applications/P2PShare.app`. It lives in the menu bar (no Dock icon). On macOS 15 and later, allow it on the local network when asked: LocalSend and the fast local-network path need it.

**Linux**: the AppImage needs FUSE 2 (`sudo apt install libfuse2` on Ubuntu 22.04 and later), then `chmod +x P2PShare_*.AppImage`. GNOME shows tray icons only with the AppIndicator extension; without it, launch the app again to bring its window back. On Wayland the window opens where the compositor puts it. Each release lists the SHA-256 of its files in `SHA256SUMS.txt`.

## Features

- **Private chat**: end-to-end encrypted messages with delivery receipts; messages written while a friend is offline are delivered when they reconnect.
- **File transfer**: files of any size, several at once, each one accepted by the receiver first, with automatic resume after a network drop.
- **Fast on the local network** (3.3): when a friend is on the same network, the bytes go over a direct connection instead of the internet path.
- **Groups**: group chats of up to 32 members and a shared file repository per group; members do not need to be friends with each other.
- **LocalSend**: send and receive files with the LocalSend app, and chat with LocalSend devices (3.3).
- **Friends only**: your ID is your public key (64 hex characters); only your friends, the members of your groups and someone bringing the code of one of your open invitation links can connect, and every connection is checked against their key. Friends add each other with an invitation link (3.4.6), or by ID.
- **Self-hosted relay**: for networks that block iroh's public relays.
- **Tray app**: Windows 11 (Fluent) look, dark and light themes, clickable notifications, optional start with the computer (hidden in the tray).

The full list of changes, for users and in French, is in [CHANGELOG.md](CHANGELOG.md); the app shows it after each update and in Settings → À propos.

## What's new in 3.4.7 (in progress)

- **Local network off switch**: Settings → Réseau → Réseau local turns off the app's own use of the local network: LocalSend (announcements, server, nearby devices), finding friends there without the internet, and the fast path between friends. Friends on the same network are then reached through iroh only. The setting is read at start, so changing it restarts the app (`set_local_network`, `localNetwork` in `config.json`).
- **Backup moves to Settings → Avancé**, with the P2P ID.
- **Versions with a fourth number** (3.4.7.2) can be released: see "Release" below.

## What's new in 3.4.6

- **Invitation links** instead of exchanging IDs: Friends tab → + → create a link, send it by mail or message. Clicking it opens P2P Share, which asks whether to add whoever sent it; both become friends, nobody types an ID. A link works for one person, for 7 days; pasted in the Friends tab when clicking does not open the app. Your ID moves to Settings → Avancé, with adding by ID, for friends on 3.4.5 or older.
- **macOS**: the window opens on the current Space, over a full-screen app too, instead of switching to the desktop and needing a second click.
- **No friends twice**: a friend on the same network is no longer listed among the nearby LocalSend devices as well (their app must be 3.4.5 or later, which announces its P2P ID).
- **Automatic updates** wait for a transfer started during the download instead of cutting it with the restart.
- **More file types count as programs** (Python scripts, Windows consoles, macOS scripts and workflows…): never accepted without asking, even from a trusted friend.
- An Android phone shows as a phone, not a Linux computer, on other LocalSend devices.

## What's new in 3.4.5

- **Automatic updates** on Windows, macOS and Linux: the app finds new versions by itself, offers them over the friend list and in Settings → À propos (with a check button), and can install them on its own once no transfer runs. Only files signed with the project's key are installed. Android links to the releases page instead.
- **Friends on the same network without the internet**: the app announces itself on the local network (through the LocalSend announcements, now always on) and connects to friends it finds there directly, even when the relays and the address directory are blocked.
- **Your own relay comes first**: friends who use it too exchange with you through it, faster than through the shared public relays; friends on public relays are still reached on theirs, and the public relays stand in while yours does not answer.
- **Your own relay as a way to the public ones**: on a network that blocks iroh's relays and directory (eduroam…), the app reaches them through your relay's tunnel and finds friends through its directory, which passes records to iroh's; friends need nothing. Each relay is tried directly first. Needs the latest P2P-Sender-relay.
- **See how you are connected**: a chat shows whether the friend is on the local network, reached directly or through a relay; the header says when only the local network works.
- **Clearer settings**: grouped by section (profile, general, chats and notifications, backup, network, about).

Friends on 3.4 or older still talk to you as before; finding each other without the internet needs 3.4.5 on both sides. Install 3.4.5 by hand once: from then on, updates come by themselves.

## What's new in 3.4

- **Chat**: reply to a message, react with an emoji, see when a message was read and when a friend is typing (can be turned off), search a conversation, clickable web links, and pictures (jpg, png, gif, webp, bmp) shown in the conversation with a preview.
- **Folders and transfers**: send whole folders, keeping their tree; drag and drop on a friend, a group or a LocalSend device; pause and resume; speed limit; accept a friend's files automatically (programs still ask). Folders of small files go about 20 times faster, and the list shows whether a transfer is on the local network, direct or through a relay.
- **Groups**: @mentions, pinned messages and files, silent groups, and shared folders that keep their tree when downloaded.
- **Notifications**: settings by kind, per chat and per group, and Do Not Disturb.
- **Your data**: history and identity are encrypted on disk (the key is tied to your session on Windows and kept in the Keychain on macOS; on Linux it stays in a private file). Backup and restore your identity with a password-protected file: without a backup, your ID cannot be recovered after reinstalling the system.
- **Easier to use**: welcome on first launch, nicknames between friends, a conversation list with the last message, OneDrive colors, and the window goes away when you click elsewhere (pin it to drag files from the file explorer).
- **Lighter**: the app sleeps when idle, retries offline friends less and less often, and groups use about a third of the memory.
- Many fixes: safer saves, offers and cancels that always get through, more robust LocalSend, local-network and group sync.

Friends on 3.3 can still talk to you: they receive pictures as plain files and folders without their tree, and do not see reactions, read marks or pins.

## What's new in 3.3

### Faster transfers on the local network

When you send a file to a friend on the same network, its bytes now go over a direct TCP + TLS connection instead of QUIC through iroh. This is the same kind of connection LocalSend uses, and it is faster: 194 MB/s instead of 132 MB/s for 1 GB between two instances on one computer. It happens on its own, nothing to set up (turning the local network off in the settings stops it):

- Friends tell each other, over their authenticated iroh connection, the fingerprint of a certificate kept for this (`lan.json` in the app data folder), a port and their private IPv4 addresses.
- The sender connects to an address of the friend on one of its own networks. Each side checks the other's certificate against the fingerprint it was told, so nobody else on the network can read the file or pass for your friend.
- Offers, answers, cancelling and resuming still go through iroh. If the direct connection cannot be made or breaks, the transfer goes on through iroh from where it stopped.
- The transfer list shows "Réseau local" next to the speed when a file takes this path.
- Both friends need version 3.3.

### Chat with LocalSend devices

Each LocalSend device in the Friends tab now has a chat. Text sent from the LocalSend app on that device lands there, with a notification, and what you write there reaches the device as a LocalSend text message. Your messages show when the device got them, or why they could not be sent. Conversations are kept (`localsend-chats.json`), and devices you have chatted with stay listed while they are away. Messages from older LocalSend versions, which prove nothing about who sent them, are marked "non vérifié".

## LocalSend compatibility

LocalSend compatibility is on unless the local network is turned off in the settings (Réseau → Réseau local): the app exchanges files and text with the LocalSend app on the same network.

- The computer shows up in LocalSend under your nickname. Each incoming file is offered for you to accept, like files from friends.
- The devices found appear in the Friends tab, to send them files or open their chat. When we send, the device's certificate must match the fingerprint it announced.
- It speaks LocalSend protocol v2.2 over HTTPS with a self-signed certificate (`localsend.json` in the app data folder), on port 53317 (TCP and UDP multicast), or another TCP port if that one is taken, for example by the LocalSend app itself.
- Allow the app through the firewall, on public networks too (campus Wi-Fi is often one). Networks that isolate their clients (guest Wi-Fi, some campus networks) keep devices from seeing each other.

## Groups (since 3.1)

- Each group is a signed, append-only log: messages, members added or removed, renames, nicknames and shared files are entries signed by their author.
- Members exchange what they are missing whenever they connect, so a message reaches everyone, even members who were offline while its author has since left.
- Every member can invite their friends and remove members. Concurrent changes resolve the same way for everyone (the earliest wins).
- The file repository lists the files; each member downloads what they want, from any member who has the file, and the download is checked against its BLAKE3 hash. Shared files stay where they are on the sharer's disk.
- Set your nickname in the settings: it is how members who are not your friends see you.

## Self-hosted relay (since 3.2)

Some networks (eduroam behind a FortiGate, for example) block iroh's public relays and address directory, so friends cannot reach each other. The app can use your own relay as well: run [P2P-Sender-relay](https://github.com/lamziiii/P2P-Sender-relay) behind an HTTPS reverse proxy, then enter its address (and token) in Settings → Réseau → Relais personnel. Since 3.4.5 it comes first: it is the home relay whenever it answers, so friends who use it too exchange through it; friends elsewhere are still reached on their own relays, directly where the network allows and otherwise through your relay's tunnel (`src-tauri/src/p2p/tunnel.rs`: iroh goes through a local proxy that checks once per relay and network whether it answers directly); while it does not answer, the public relays stand in, and the app checks every minute whether it is back. Friends who want to reach you from a blocking network enter the same relay; friends on the same local network find each other without any relay.

## Version 3

Version 3 replaced Electron/Node.js with Tauri/Rust and Hyperswarm with iroh:

- The installer is about 3 MB instead of 93 MB, and the installed app about 9 MB instead of 303 MB (system web view instead of a bundled Chromium).
- While hidden in the tray the web view is released: the app then uses about 9 MB of private memory instead of 142 MB.
- Files go over dedicated QUIC streams with large flow-control windows, read ahead from disk and written through an 8 MB buffer: about 1.8x the throughput of version 2 with less CPU (see `bench/RESULTS.md`).
- Your ID, friends, messages and settings from version 2 are imported on first launch, so your ID does not change.
- Version 3 cannot talk to version 2.x: your friends need to update too.

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

### Release

Set the version with `npm run set-version <x.y.z>`: it writes it in package.json and its lock, `src-tauri/tauri.conf.json`, `src-tauri/Cargo.toml` and `Cargo.lock` (builds use `--locked`). A fourth number is optional (`npm run set-version 3.4.7.2`, from .1): Cargo, npm and Tauri take three numbers only, so the files hold `3.4.7+2` (semver build metadata, which Tauri also turns into the Windows version 3.4.7.2), while the tag, `CHANGELOG.md`, the app, the release files and winget say 3.4.7.2, and the updater counts it as newer than 3.4.7. Push a version tag (`git tag v3.4.7 && git push origin v3.4.7`, or `v3.4.7.2`): GitHub Actions checks that the tag matches that version, builds the installers for the three systems and the APK, signs the update files, publishes the release with `latest.json` last (what installed apps read to update), then submits the new version to winget (see `.github/workflows/build.yml`). A stable release stops before publishing if the APK or the update signing key is missing, and before building if `CHANGELOG.md` has no `## x.y.z — <date>` section for it (until the release, the section of the next version says « à venir » instead of the date): that section becomes the release notes, the notes shown before installing an update, and what the app shows once updated. A prerelease tag (`v3.4.0-beta.1`) is published as a prerelease, not marked latest, and not sent to winget. The winget step needs a `WINGET_TOKEN` secret: a classic GitHub token with the `public_repo` scope.

### Invitation links

A link is `https://lamziiii.github.io/P2P-Sender/ami/#i=<ID>&c=<code>&n=<nickname>`: a page of this repository (`docs/ami/index.html`, published by GitHub Pages: Settings → Pages → Deploy from a branch, `main`, folder `/docs`) that opens the app with `p2pshare://ami?…`, since mail and chat apps only make web links clickable. The part after `#` never reaches the server. The app registers the `p2pshare` scheme through `tauri-plugin-deep-link` (the installers, and the app itself at start on Windows and Linux for an AppImage or a moved app; Info.plist on macOS; an intent filter on Android). The code, made for one link (`src-tauri/src/p2p/invites.rs`), is kept 7 days in `invites.json` (encrypted), with at most 20 links open at once (a new one drops the oldest); whoever accepts the link adds its maker and sends the code in the hello of its connection (or first thing on a connection the maker opens, as a co-member of a group), and the maker, who lets in a peer that is not a friend only with a valid code, adds them in turn. Link format: `src-tauri/src/invite.rs`.

### Android

GitHub Actions builds an arm64 APK on every push, downloadable from the Actions tab as `Test-android`: it is signed with a temporary key, so each build must replace the previous one by uninstalling it first. To attach an APK to releases, create a release key once and keep it safe (without it, the app can never be updated), then add the secrets `ANDROID_KEYSTORE_BASE64` (the keystore in base64), `ANDROID_KEYSTORE_PASSWORD`, `ANDROID_KEY_ALIAS` and `ANDROID_KEY_PASSWORD`.

### Updates

On Windows, macOS and Linux (AppImage) the app looks for a newer release a minute after starting and then every 6 hours, and offers it over the friend list and in Settings (which also has a check button); installing saves everything and restarts the app. With "Installer les mises à jour automatiquement" on, it installs by itself once no transfer runs. It reads `latest.json` from the latest release, and only installs files signed with the updater key. Android has no automatic updates: Settings links to the releases page. A stable release needs the secrets `TAURI_SIGNING_PRIVATE_KEY` (the private key from `npx tauri signer generate`) and `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`; its public key is in `src-tauri/tauri.conf.json`. Keep the private key safe: without it, installed apps cannot be updated any more.

### Code signing

Windows release builds are signed through [SignPath](https://signpath.io) once the repository has its settings; without them, the installers are built unsigned as before. The workflow signs `p2pshare.exe` first, builds the installer around it (`tauri bundle`), then signs the installer. It needs:

- the secret `SIGNPATH_API_TOKEN` (a SignPath API token of a CI user allowed to submit signing requests);
- the repository variables `SIGNPATH_ORGANIZATION_ID`, `SIGNPATH_PROJECT_SLUG` and `SIGNPATH_SIGNING_POLICY_SLUG`;
- on SignPath, a trusted build system for GitHub.com linked to the project, and an artifact configuration that signs the `.exe` at the root of the artifact.

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
cargo build --release --example bench --manifest-path src-tauri/Cargo.toml
pwsh bench/run.ps1 -File <some big file>
```

- `groups_test` runs three nodes (two of them not friends) through invitations, group chat, delivery to a member who was offline, shared-file downloads from another member, and removal.
- `localsend_receiver` runs a node with LocalSend on that accepts every file (except names containing `refuse`, declined, or `attente`, left unanswered; it cancels names containing `annule` midway), to send to from a LocalSend app or the LocalSend CLI. `localsend_send` sends files, or a text message, to a device by name and reports the outcome.
- `tunnel_test` puts Alice on a simulated blocking network (`P2PSHARE_BLOCK_PUBLIC=1`: no direct access to iroh's relays and directory) with her relay, and Bob on public relays only: Alice finds Bob through her relay's directory, reaches his relay through the tunnel, and sends him a file.
- `relay_test` checks that your own relay comes first: with it off, a node falls back to a public relay; once it is started, the node comes back to it, and two friends on it send a file (with `P2PSHARE_RELAY_ONLY=1`, without direct paths, so through the relay).
- `nearby_test` runs two friends that find each other through the LocalSend announcements and send a file; with `P2PSHARE_NO_INTERNET=1` they have no relay and no address directory, as on a network that blocks them, and still connect over the local network.
- `invite_test` runs three nodes without internet: Alice makes a link, Bob accepts it and both become friends; Carol tries the same link, is turned away, and her app shows that the link no longer works.
- `bench` sends a file (or several, or a folder; `BENCH_COUNT` tells the receiver how many files to wait for) between two headless nodes and reports the throughput; set `BENCH_NO_LAN=1` to compare without the local-network path. `bench/run.ps1` runs it and also reports peak memory and CPU time.

## Project layout

- `src/`: React UI (`api.ts` wraps the Tauri commands and events).
- `src/Groups.tsx`: groups UI; `src/LocalSendDevices.tsx`, `src/LocalSendChat.tsx`: LocalSend devices and their chat; `src/RelaySettings.tsx`: self-hosted relay setting.
- `src-tauri/src/p2p.rs`: P2P core (connections, chat, transfers).
- `src-tauri/src/p2p/lan.rs`: the local-network path for transfers.
- `src-tauri/src/group.rs`: group log (signed entries, replay, sync state); `src-tauri/src/p2p/groups.rs`: group networking (invitations, sync, shared files).
- `src-tauri/src/localsend.rs`: LocalSend compatibility (discovery, HTTPS server); `src-tauri/src/localsend/send.rs`: sending files and text; `src-tauri/src/p2p/external.rs` puts their files in the transfer list.
- `src-tauri/src/lib.rs`: window, tray, notifications, settings, LocalSend chats, commands.
- `src-tauri/src/notify.rs`: clickable native notifications per platform.
- `scripts/dist.mjs`: local build, copied to `release/`.

## Built With

- **Frontend**: [React 19](https://react.dev/), [Vite](https://vitejs.dev/)
- **Desktop**: [Tauri 2](https://tauri.app/)
- **P2P networking**: [iroh](https://github.com/n0-computer/iroh); [rustls](https://github.com/rustls/rustls) for the local-network path and LocalSend
- **Style**: Vanilla CSS (Fluent / Windows 11 look)
- **Languages**: Rust, TypeScript, HTML, CSS

## License

This project is licensed under the MIT License - see the [LICENSE](LICENSE) file for details.

---

Built by [lamziiii](https://github.com/lamziiii)
