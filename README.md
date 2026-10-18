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

The installers are not signed: Windows SmartScreen may ask you to confirm. Each release lists the SHA-256 of its files in `SHA256SUMS.txt`.

## Features

- **Private chat**: end-to-end encrypted messages with delivery receipts; messages written while a friend is offline are delivered when they reconnect.
- **File transfer**: files of any size, several at once, each one accepted by the receiver first, with automatic resume after a network drop.
- **Fast on the local network** (3.3): when a friend is on the same network, the bytes go over a direct connection instead of the internet path.
- **Groups**: group chats of up to 32 members and a shared file repository per group; members do not need to be friends with each other.
- **LocalSend**: send and receive files with the LocalSend app, and chat with LocalSend devices (3.3).
- **Friends only**: your ID is your public key (64 hex characters); only your friends and the members of your groups can connect, and every connection is checked against their key.
- **Self-hosted relay**: for networks that block iroh's public relays.
- **Tray app**: Windows 11 (Fluent) look, dark and light themes, clickable notifications, optional start with the computer (hidden in the tray).

## What's new in 3.3

### Faster transfers on the local network

When you send a file to a friend on the same network, its bytes now go over a direct TCP + TLS connection instead of QUIC through iroh. This is the same kind of connection LocalSend uses, and it is faster: 194 MB/s instead of 132 MB/s for 1 GB between two instances on one computer. It happens on its own, nothing to set up:

- Friends tell each other, over their authenticated iroh connection, the fingerprint of a certificate kept for this (`lan.json` in the app data folder), a port and their private IPv4 addresses.
- The sender connects to an address of the friend on one of its own networks. Each side checks the other's certificate against the fingerprint it was told, so nobody else on the network can read the file or pass for your friend.
- Offers, answers, cancelling and resuming still go through iroh. If the direct connection cannot be made or breaks, the transfer goes on through iroh from where it stopped.
- The transfer list shows "Réseau local" next to the speed when a file takes this path.
- Both friends need version 3.3. It works without turning on LocalSend compatibility.

### Chat with LocalSend devices

Each LocalSend device in the Friends tab now has a chat. Text sent from the LocalSend app on that device lands there, with a notification, and what you write there reaches the device as a LocalSend text message. Your messages show when the device got them, or why they could not be sent. Conversations are kept (`localsend-chats.json`), and devices you have chatted with stay listed while they are away. Messages from older LocalSend versions, which prove nothing about who sent them, are marked "non vérifié".

## LocalSend compatibility

Turn on Settings → Compatibilité LocalSend to exchange files and text with the LocalSend app on the same network:

- The computer shows up in LocalSend under your nickname. Each incoming file is offered for you to accept, like files from friends.
- The devices found appear in the Friends tab, to send them files or open their chat. When we send, the device's certificate must match the fingerprint it announced.
- It speaks LocalSend protocol v2.2 over HTTPS with a self-signed certificate (`localsend.json` in the app data folder), on port 53317 (TCP and UDP multicast), or another TCP port if that one is taken, for example by the LocalSend app itself.
- Allow the app through the firewall on private networks. Networks that isolate their clients (guest Wi-Fi, some campus networks) keep devices from seeing each other.

## Groups (since 3.1)

- Each group is a signed, append-only log: messages, members added or removed, renames, nicknames and shared files are entries signed by their author.
- Members exchange what they are missing whenever they connect, so a message reaches everyone, even members who were offline while its author has since left.
- Every member can invite their friends and remove members. Concurrent changes resolve the same way for everyone (the earliest wins).
- The file repository lists the files; each member downloads what they want, from any member who has the file, and the download is checked against its BLAKE3 hash. Shared files stay where they are on the sharer's disk.
- Set your nickname in the settings: it is how members who are not your friends see you.

## Self-hosted relay (since 3.2)

Some networks (eduroam behind a FortiGate, for example) block iroh's public relays and address directory, so friends cannot reach each other. The app can use your own relay instead: run [P2P-Sender-relay](https://github.com/lamziiii/P2P-Sender-relay) behind an HTTPS reverse proxy, then enter its address (and token) in Settings → Relais personnel. Friends who want to reach you from such a network use the same relay.

## Version 3

Version 3 replaced Electron/Node.js with Tauri/Rust and Hyperswarm with iroh:

- The installer is about 3 MB instead of 93 MB, and the installed app about 9 MB instead of 303 MB (system web view instead of a bundled Chromium).
- While hidden in the tray the web view is released: the app then uses about 9 MB of private memory instead of 142 MB.
- Files go over dedicated QUIC streams with large flow-control windows, read ahead from disk and written through an 8 MB buffer: about 1.8x the throughput of version 2 with less CPU (see `bench/RESULTS.md`).
- Your ID, friends, messages and settings from version 2 are imported on first launch, so your ID does not change.
- Version 3 cannot talk to version 2.x: your friends need to update too.

## Development

### Prerequisites

- [Node.js](https://nodejs.org/) (v20 or higher)
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

The app and its installer are copied to `release/`. Cargo's working files go to `%LOCALAPPDATA%\p2pshare-target` (or `$CARGO_TARGET_DIR`): several GB that do not belong in the project folder, and Windows refuses to run the build scripts Cargo compiles inside a OneDrive folder.

### Release

Push a version tag (`git tag v3.3.0 && git push origin v3.3.0`): GitHub Actions builds the installers for the three systems, publishes the release, then submits the new version to winget (see `.github/workflows/build.yml`). The winget step needs a `WINGET_TOKEN` secret: a classic GitHub token with the `public_repo` scope.

### Tests and benchmark

```bash
cargo test --lib --manifest-path src-tauri/Cargo.toml
cargo run --release --example groups_test --manifest-path src-tauri/Cargo.toml -- <workdir>
cargo run --example localsend_receiver --manifest-path src-tauri/Cargo.toml -- <workdir> [seconds]
cargo run --example localsend_send --manifest-path src-tauri/Cargo.toml -- <workdir> <device name> <file>... [--cancel-at <percent>]
cargo run --example localsend_send --manifest-path src-tauri/Cargo.toml -- <workdir> <device name> --text <message>
cargo build --release --example bench --manifest-path src-tauri/Cargo.toml
pwsh bench/run.ps1 -Sender v3 -Receiver v3 -File <some big file>
```

- `groups_test` runs three nodes (two of them not friends) through invitations, group chat, delivery to a member who was offline, shared-file downloads from another member, and removal.
- `localsend_receiver` runs a node with LocalSend on that accepts every file (except names containing `refuse`, declined, or `attente`, left unanswered; it cancels names containing `annule` midway), to send to from a LocalSend app or the LocalSend CLI. `localsend_send` sends files, or a text message, to a device by name and reports the outcome.
- `bench` sends a file between two headless nodes and reports the throughput; set `BENCH_NO_LAN=1` to compare without the local-network path. `bench/run.ps1` runs it and also reports peak memory and CPU time; with `-Sender v2` it uses the Electron version's core (`../P2P_electron`) for comparison.

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
