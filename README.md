# P2P Share - Cross-platform Peer-to-Peer File Transfer & Chat

P2P Share is a lightweight, privacy-focused desktop application built with **Tauri**, **Rust** and **React**. It enables direct peer-to-peer file sharing and chat without intermediate servers, using **iroh** (QUIC) for peer discovery, NAT hole-punching and end-to-end encrypted connections.

![P2P Share UI](icon.png)

## Features

- **Private P2P Chat**: End-to-end encrypted messaging with delivery receipts; messages sent while a friend is offline are delivered when they reconnect.
- **Direct File Transfer**: Send files of any size (several at once) over the friend's existing connection, with automatic resume after a network drop.
- **Friend System**: Your ID is your public key (64 hex characters); only friends can connect to you, and every connection is authenticated against that key.
- **Groups**: Group chats with up to 32 members, and a shared file repository per group. Members do not need to be friends with each other.
- **Real-time Monitoring**: Track transfer progress, speed (MB/s), and connection status.
- **Theming**: Windows 11 (Fluent) style interface with black dark mode and light mode.
- **Automatic Startup**: Option to launch automatically (hidden in the tray) with the computer.
- **Privacy First**: No central server stores your messages or files.

## Version 3

Version 3 replaces Electron/Node.js with Tauri/Rust and Hyperswarm with iroh:

- The installer is 2.9 MB instead of 93 MB and the installed app 7.7 MB instead of 303 MB (system web view instead of a bundled Chromium).
- While hidden in the tray the web view is released: the app then uses about 9 MB of private memory instead of 142 MB.
- Files go over dedicated QUIC streams with large flow-control windows, read ahead from disk and written through an 8 MB buffer: about 1.8x the throughput of version 2 with less CPU (see `bench/RESULTS.md`).
- Your ID, friends, messages and settings from version 2 are imported on first launch, so your ID does not change.
- Version 3 cannot talk to version 2.x: your friends need to update too.

## Groups (version 3.1)

- Each group is a signed, append-only log: messages, members added or removed, renames, nicknames and shared files are entries signed by their author.
- Members exchange what they are missing whenever they connect, so a message reaches everyone, even members who were offline while its author has since left.
- Every member can invite their friends and remove members. Concurrent changes resolve the same way for everyone (the earliest wins).
- The file repository lists the files; each member downloads what they want, from any member who has the file, and the download is checked against its BLAKE3 hash. Shared files stay where they are on the sharer's disk.
- Set your nickname in the settings: it is how members who are not your friends see you.
- Groups need version 3.1 for every member; friends on 3.0 keep private chat and transfers.

## Getting Started

### Prerequisites

- [Node.js](https://nodejs.org/) (v20 or higher)
- [Rust](https://rustup.rs/) (stable)
- Linux only: `libwebkit2gtk-4.1-dev libayatana-appindicator3-dev librsvg2-dev libxdo-dev libssl-dev`

### Installation

```bash
git clone https://github.com/lamziiii/P2P-sender.git
cd P2P-sender
npm install
npm run dev
```

### Building for Production

```bash
npm run dist
```

The installers are in `src-tauri/target/release/bundle/` (`nsis/` on Windows, `dmg/` on macOS, `appimage/` on Linux).

### Tests and benchmark

```bash
cargo test --lib --manifest-path src-tauri/Cargo.toml
cargo run --release --example groups_test --manifest-path src-tauri/Cargo.toml -- <workdir>
cargo build --release --example bench --manifest-path src-tauri/Cargo.toml
pwsh bench/run.ps1 -Sender v3 -Receiver v3 -File <some big file>
```

`groups_test` runs three nodes (two of them not friends) through invitations, group chat, delivery to a member who was offline, shared-file downloads from another member, and removal.

`bench/run.ps1` runs two headless nodes on the same machine and reports throughput, peak memory and CPU time. With `-Sender v2` it uses the Electron version's core (`../P2P_electron`) for comparison.

## Project layout

- `src/`: React UI (`api.ts` wraps the Tauri commands and events).
- `src-tauri/src/p2p.rs`: P2P core (connections, chat, transfers).
- `src-tauri/src/group.rs`: group log (signed entries, replay, sync state).
- `src-tauri/src/p2p/groups.rs`: group networking (invitations, sync, shared files).
- `src/Groups.tsx`: groups UI.
- `src-tauri/src/lib.rs`: window, tray, notifications, settings, commands.
- `src-tauri/src/notify.rs`: clickable native notifications per platform.

## Built With

- **Frontend**: [React 19](https://react.dev/), [Vite](https://vitejs.dev/)
- **Desktop**: [Tauri 2](https://tauri.app/)
- **P2P Networking**: [iroh](https://github.com/n0-computer/iroh)
- **Style**: Vanilla CSS (Fluent / Windows 11 look)
- **Languages**: Rust, TypeScript, HTML, CSS

## License

This project is licensed under the MIT License - see the [LICENSE](LICENSE) file for details.

---

Built by [lamziiii](https://github.com/lamziiii)
