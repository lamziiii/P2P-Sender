# What's new in P2P Share

## 3.4.9 — 15 November 2026

- **P2P Share in English**: the app follows your system's language, in French or English (any other language gives English). You can choose it in Settings → General.
- **Send with P2P Share**: send a file or a folder without opening the app first. On Windows, right-click it in File Explorer (under "Show more options" on Windows 11); on a Mac, right-click then "Open With" → P2P Share; on Linux, "Open With" in your file manager; on Android, the Share button of your other apps (photos, files…). P2P Share opens and asks who to send it to: a friend, a group or a LocalSend device. A text or a link shared from Android is sent as a message. On Windows and Linux, you can remove the entry in Settings → General.
- **Updates on Android**: the app finds new versions by itself and installs them from Settings → About, without going through the releases page. Android asks you to confirm, and the first time to allow P2P Share to install apps.
- **Intel Macs**: the macOS app now runs on Intel Macs too, not only on Apple Silicon.
- **Invitation links**: when you open a friend's link, P2P Share asks you to confirm before adding them, and warns you if one of your friends already has that name. A web page or another app can no longer add someone to your friends without you.
- **Backups**: they no longer hold your invitation links. After a restore, the ones you had sent no longer work: create new ones.
- **Security**: on Android, `.apk` files are treated as programs, and another app's Share button can no longer make P2P Share send its internal files; after a restart, a folder received automatically asks again before taking programs; a LocalSend device that proved who it is never gets your messages or files unencrypted again; a file removed from a group is no longer sent, and a removed member can no longer finish downloading it; pictures over about 64 million pixels show without a preview, so they no longer freeze the chat; and several protections against a malicious group member.
- Fixes: with the .deb package, the automatic updates setting says that your password is asked.

Updates from the app on Android start with this version: install it once by hand.

## 3.4.8.5 — 5 November 2026

- **Transfers resume after a restart**: a send or a receive in progress with a friend carries on where it stopped when you restart the app or the computer, without starting over. If the file sent was changed meanwhile, it does not resume.
- **An invitation link adds you at once**: when you click a friend's link, P2P Share opens and adds them to your friends, with no question to answer. If the app is not installed, the page offers to install it.
- **Paste to send**: paste a screenshot or copied files (Ctrl+V) into a chat, they are sent as if you had dropped them there.
- **Faster on the local network**: when the firewall of one of the two devices blocks the direct connection, the other one makes it instead. A file started over the internet moves to the local network as soon as your friend is found there. On Android, Wi-Fi stays at full speed during transfers, even with the screen off.
- **Linux**: a `.deb` package for Debian and Ubuntu, besides the AppImage. It installs the app in the applications menu and updates itself, asking for your password.
- **Windows**: on a first install, the installer asks once for permission to open the firewall to the app, on every network, public ones included (school or station Wi-Fi). Installing a new version over the old one no longer puts the desktop shortcut back if you had deleted it (from the next update on).

Resuming after a restart and connecting both ways need 3.4.8.5 on both sides; with a friend on an older version, everything works as before.

## 3.4.8 — 4 November 2026

- **LocalSend devices look like your friends**: a click opens the chat, with the last message, the time and the unread messages. The send button stays on the row, and the "…" menu turns notifications off and deletes the chat. It is only deleted on your side: the messages stay in the LocalSend app of the other device. In the chat, the paperclip sends a file.
- **Send to a friend**: the send button, on their row, offers a file or a folder, like the chat's paperclip.
- Fixes: a pinned window stays on top when you click elsewhere, instead of going behind the other windows.

## 3.4.7 — 4 November 2026

- **The local network can be turned off**: in Settings → Network, a switch stops the app's use of the local network. You are no longer visible there, LocalSend stops, and your friends on the same network are only reached over the internet, without the faster connection. The app restarts to apply the change.
- **Backup moves to Settings → Advanced**, with your P2P ID.

## 3.4.6 — 3 November 2026

- **Invite your friends with a link**: in the Friends tab (+ button), create an invitation link and send it by email or message. Your friend clicks it, P2P Share opens and asks whether to add you: you are now friends both ways, without copying an ID. A link is for one person and stays valid for 7 days. If it does not open the app, paste it in the Friends tab.
- **Your P2P ID moves to Settings → Advanced**, with adding by ID, for your friends still on 3.4.5 or earlier.
- **Automatic updates**: an update waits for the end of a transfer started during its download, instead of cutting it by restarting.
- **Your friends no longer show twice**: a friend on the same network is no longer also listed among the LocalSend devices nearby (they need 3.4.5 or later).
- **More kinds of programs recognised** (Python scripts, Windows consoles, macOS scripts and workflows…): they are never accepted without asking you, even from a trusted friend.
- Fixes: on a Mac, the window opens where you are, even over a full-screen app, instead of taking you back to the desktop and asking for a second click; an Android phone shows as a phone, not as a Linux computer, on other LocalSend devices.

Invitation links need 3.4.6 on both sides; with a friend still on 3.4.5 or earlier, swap your IDs (Settings → Advanced).

## 3.4.5 — 3 November 2026

- **Automatic updates** (Windows, macOS, Linux): the app finds new versions by itself and offers them above the friend list and in Settings → About, with a button to check. You can also let them install by themselves, once the transfers are over. Only versions signed by the project are installed. On Android, a button opens the releases page.
- **Your friends on the same network, even without internet**: the app announces itself on the local network and connects straight to the friends it finds there, even when the relays are blocked.
- **Your relay first**: with a personal relay, your friends who have it too reach you through it, faster than through the public relays. Your friends who do not have it stay reachable through the public relays, which also take over if yours does not answer.
- **Your relay opens the way to the public relays**: on a network that blocks them (eduroam…), the app reaches them through your relay, and you reach your friends who set nothing up. It always tries directly first. It needs the new version of your relay (P2P-Sender-relay).
- **See how you are connected**: a chat shows whether your friend is on the local network, reached directly or through a relay. When the relays do not answer, the header says so, and whether the local network still works.
- **Clearer settings**, by section: profile, general, chats and notifications, backup, network, about.
- **LocalSend always on**: it also finds your friends on the local network.
- **What's new**: this list shows after each update and stays in Settings → About.
- Fixes: the connection status no longer overflows the header, and long error messages wrap.

Your friends on 3.4 or earlier still exchange with you; to find each other without internet, you both need 3.4.5. Install this version once by hand: the next ones will come by themselves.

## 3.4.0 — 2 November 2026

- **Android**: first version for phones (arm64), which can stay reachable in the background.
- **Chats**: reply to a message, react with an emoji, see when a message was read and when a friend is typing (can be turned off), search a chat, clickable links, images shown with a preview.
- **Folders and transfers**: send a whole folder with its subfolders, drag and drop onto a friend, a group or a LocalSend device, pause and resume, limit the speed, accept a friend's files automatically (programs always ask). Folders of small files go about 20 times faster.
- **Groups**: @ mentions, pinned messages and files, silent groups, shared folders.
- **Notifications**: settings by kind, by chat and by group, and a Do not disturb mode.
- **Your data**: history and identity encrypted on disk, backup and restore of your identity in a file protected by a password.
- **Simpler**: welcome on first launch, nicknames between friends, last message in the list, OneDrive's colours, a window that closes when you click elsewhere (pin it to drop files into it).
- **Lighter**: the app goes to sleep when it does nothing, and groups use about three times less memory.
- Many fixes: safer saving, sends and cancellations that always arrive, more reliable LocalSend and groups.

## 3.3.0 — 18 October 2026

- **LocalSend compatibility**: exchange files with the LocalSend app (phones, other computers) on the same network. Each file received is offered to you before it is saved.
- **Chat with LocalSend devices**: send and receive text messages, kept from one time to the next.
- **Faster transfers on the local network**: between friends on the same network, files go through a direct, encrypted connection, about 1.5 times faster.

## 3.2.0 — 8 October 2026

- **Personal relay**: for networks that block the public relays (eduroam…), enter the address of your own relay in the settings.

## 3.1.0 — 7 October 2026

- **Groups**: group chat, invitations by the members, and shared files that everyone downloads when they want. Messages reach members who were offline too.

## 3.0.0 — 7 October 2026

- **New version, rewritten from scratch**: an installer of about 3 MB instead of 93 MB, much less memory used, and transfers about 1.8 times faster.
- Your ID, your friends, your messages and your settings from version 2 are taken over on first launch.
- Version 3 does not exchange with versions 2.x: your friends must move to version 3 too.
