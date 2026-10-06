# v2 (Electron + Hyperswarm) vs v3 (Tauri + iroh)

Measured on 2026-10-05, Windows 11, both nodes on the same machine
(`bench/run.ps1`, random data, received files checked by SHA-256).

## Size

| | v2.1.0 | v3.0.0 |
|---|---|---|
| Installer (NSIS) | 93.4 MB | 2.9 MB |
| Installed app | 303 MB | 7.7 MB (+ WebView2, shipped with Windows) |

## Transfer of 1 GiB

| | v2 run 1 | v2 run 2 | v3 run 1 | v3 run 2 |
|---|---|---|---|---|
| Throughput | 64.5 MB/s | 58.9 MB/s | 119.9 MB/s | 106.8 MB/s |
| Sender CPU | 16.6 s | 18.1 s | 10.6 s | 12.0 s |
| Receiver CPU | 13.8 s | 14.6 s | 7.8 s | 10.0 s |
| Sender peak RAM | 123 MB | 124 MB | 68 MB | 88 MB |
| Receiver peak RAM | 69 MB | 69 MB | 30 MB | 30 MB |

The headless v2 node runs on plain Node.js, so these RAM figures exclude
Electron's Chromium processes.

## Connection time

| | v2 | v3 |
|---|---|---|
| Friend already online (realistic case) | 0.8-1.0 s | 0.2-0.8 s |
| Both nodes started at the same second | 0.7 s | 4.2 s |

A freshly started iroh node needs a few seconds to publish its address; the
v3 retries every second for the first 10 seconds after a failed dial.

## Whole app at rest (window open, 25 s after launch)

| | v2 (fresh build) | v3 |
|---|---|---|
| Processes | 4 | 7 (1 app + 6 WebView2) |
| Private memory | 142 MB | 108-111 MB |
| Working set | 300 MB | 305-339 MB (includes WebView2 pages shared with Edge) |

## Whole app hidden in the tray

Since the window is destroyed when hidden (and rebuilt when shown), the web
view is only alive while the window is open:

| | v2 | v3 |
|---|---|---|
| Window closed to the tray | 142 MB private (Chromium stays loaded) | 9 MB private, 36 MB working set, 1 process |
| Started at login (hidden) | same as open | 7 MB private, 24 MB working set |
| Reopening | instant | the web view is rebuilt (back on the Friends tab) |

## Compatibility

v2 → v3 and v3 → v2: no connection after 90 s, in both directions. The
networks differ (Hyperswarm DHT vs iroh). The identity is compatible: the
same `identity.key` gives the same ID in both versions (checked against
`hyperdht.keyPair`), and v3 imports it on first launch.
