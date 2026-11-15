<p align="center">
  <img src="icon.png" width="96" alt="P2P Share">
</p>

<h1 align="center">P2P Share</h1>

<p align="center">Chat and send files to your friends directly, without a server in between.<br>Windows · macOS · Linux · Android</p>

P2P Share is a small app built with [Tauri](https://tauri.app/), Rust and React. Connections go through [iroh](https://github.com/n0-computer/iroh) (QUIC, end-to-end encrypted, crosses NATs); on the same network, files take a faster direct path. It also talks to the [LocalSend](https://localsend.org) app.

## Install

Download the installer for your system from the [latest release](https://github.com/lamziiii/P2P-Sender/releases/latest), then the app updates itself.

| System | File | Note |
|---|---|---|
| Windows | `P2PShare_*_x64-setup.exe` | Not signed yet: SmartScreen may ask you to confirm. |
| macOS (Apple Silicon and Intel) | `P2PShare_*_universal.dmg` | Not notarized: right-click the app → Open the first time. Lives in the menu bar. |
| Linux | `P2PShare_*_amd64.deb` or `P2PShare_*_amd64.AppImage` | The AppImage needs FUSE 2 (`libfuse2`). |
| Android (arm64) | `P2PShare_*_android-arm64.apk` | Allow your browser or file manager to install apps. |

## Features

- **Chat**: encrypted messages with delivery and read receipts, delivered when a friend comes back online; replies, reactions, typing, search, pictures with previews.
- **Files and folders** of any size, several at once, accepted by the receiver first; drag and drop or paste; pause, resume, speed limit; transfers resume after a network drop or a restart.
- **Send with P2P Share**: right-click a file in Windows Explorer, "Open with" on macOS and Linux, or the Share button on Android, and pick who gets it.
- **Groups** of up to 32 members (who need not be friends with each other), with a shared file repository, @mentions and pinned items.
- **LocalSend**: send and receive with LocalSend devices on the same network, each with its own chat.
- **Local network**: friends on the same network exchange directly, even without internet.
- **Privacy**: your ID is your public key; only friends, group members and holders of your invitation links can connect. History and identity are encrypted on disk, with a password-protected backup.
- **Your own relay** for networks that block iroh's public relays (eduroam and some campuses), with [P2P-Sender-relay](https://github.com/lamziiii/P2P-Sender-relay).
- **The app**: French or English, tray app with dark and light themes, clickable notifications, Do Not Disturb, signed automatic updates, about 9 MB of memory while hidden.

What changed in each version: [CHANGELOG.md](CHANGELOG.md) (French) and [CHANGELOG.en.md](CHANGELOG.en.md).

## How it works

- **Identity**: each device has an ed25519 key; its public key is the ID. Every connection is checked against it. Friends add each other with an invitation link (`https://lamziiii.github.io/P2P-Sender/ami/#…`, which opens `p2pshare://`; the code is single-use and kept 7 days) or by ID.
- **Local network**: friends exchange a certificate fingerprint over iroh, then send files over TCP + TLS pinned to it (`src-tauri/src/p2p/lan.rs`). If that path fails, the transfer goes on through iroh from where it stopped.
- **LocalSend**: protocol v2.2 over HTTPS on port 53317 (`src-tauri/src/localsend.rs`). Devices that proved their certificate are never sent to in clear.
- **Groups**: a signed, append-only log per group (`src-tauri/src/group.rs`); members exchange the entries they miss when they connect. Shared files are downloaded from any member who has them and checked against their BLAKE3 hash.
- **Relay**: a self-hosted relay comes first when it answers; other relays are reached through its tunnel when the network blocks them (`src-tauri/src/p2p/tunnel.rs`).
- **Updates**: the app reads `latest.json` from the latest release; desktop files are signed with the updater key (`src-tauri/src/update.rs`), the Android APK is checked by the system installer.

## Development

Needs [Node.js](https://nodejs.org/) 20.19+ and [Rust](https://rustup.rs/) stable (Linux: `libwebkit2gtk-4.1-dev libayatana-appindicator3-dev librsvg2-dev libxdo-dev libssl-dev`).

```bash
npm install
npm run dev        # run
npm run dist       # build: app and installer in release/
npm run lint
cargo test --lib --manifest-path src-tauri/Cargo.toml
```

Cargo builds in `$CARGO_TARGET_DIR`, else `%LOCALAPPDATA%\p2pshare-target` (Windows) or `~/.cache/p2pshare-target`. End-to-end examples (`groups_test`, `nearby_test`, `invite_test`, `relay_test`, `tunnel_test`, `scenarios`, `localsend_*`, `bench`) are in `src-tauri/examples/`; each one says how to run it at the top of its file.

**Languages**: every text exists in French and English (`tr('…', '…')` in `src/i18n.ts`, `lang::tr` in `src-tauri/src/lang.rs`, `Lang.tr` in `Lang.kt`), and each version has a section in both changelogs.

**Release**: `npm run set-version x.y.z` (or `x.y.z.n`), date the version's section in `CHANGELOG.md`, then push the tag `vx.y.z`. GitHub Actions (`.github/workflows/build.yml`) builds Windows, macOS (universal), Linux (AppImage and .deb) and the APK, signs the update files and publishes the release with `latest.json`. A stable release needs the secrets `TAURI_SIGNING_PRIVATE_KEY` and `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` (from `npx tauri signer generate`; losing the key blocks updates for good) and `ANDROID_KEYSTORE_BASE64`, `ANDROID_KEYSTORE_PASSWORD`, `ANDROID_KEY_ALIAS`, `ANDROID_KEY_PASSWORD` (same thing for the APK). Optional code signing: Windows through [SignPath](https://signpath.io) (secret `SIGNPATH_API_TOKEN`, variables `SIGNPATH_ORGANIZATION_ID`, `SIGNPATH_PROJECT_SLUG`, `SIGNPATH_SIGNING_POLICY_SLUG`), macOS with a Developer ID (`APPLE_CERTIFICATE`, `APPLE_CERTIFICATE_PASSWORD`, `APPLE_SIGNING_IDENTITY`, and for notarization `APPLE_ID`, `APPLE_PASSWORD`, `APPLE_TEAM_ID`).

## Layout

- `src/`: React UI (`api.ts` wraps the Tauri commands, `i18n.ts` the two languages).
- `src-tauri/src/p2p.rs`: connections, chat, transfers; `p2p/lan.rs`, `p2p/resume.rs`, `p2p/groups.rs`, `p2p/tunnel.rs`.
- `src-tauri/src/group.rs`: group log; `localsend.rs`: LocalSend; `share.rs`: "Send with P2P Share"; `update.rs`: updates; `lib.rs`: window, tray, settings, commands.
- `src-tauri/gen/android`: the Android app (manifest, Kotlin plugin and service).
- `docs/ami/`: the invitation page (GitHub Pages).

## License

MIT, see [LICENSE](LICENSE). Built by [lamziiii](https://github.com/lamziiii).
