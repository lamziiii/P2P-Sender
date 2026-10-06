// Headless transfer benchmark using the Electron version's P2P core
// (P2P_electron/dist-electron/p2p.js, Hyperswarm). Same protocol as
// src-tauri/examples/bench.rs:
//
//   node electron-bench.mjs <electronDir> recv <workdir>
//   node electron-bench.mjs <electronDir> send <workdir> <file>
import crypto from 'node:crypto';
import fs from 'node:fs';
import path from 'node:path';
import { pathToFileURL } from 'node:url';

const [electronDir, role, work, file] = process.argv.slice(2);
if (!electronDir || !['recv', 'send'].includes(role) || !work || (role === 'send' && !file)) {
  console.error('usage: electron-bench.mjs <electronDir> <recv|send> <workdir> [file]');
  process.exit(2);
}
const dist = path.join(path.resolve(electronDir), 'dist-electron');
const { P2PNode } = await import(pathToFileURL(path.join(dist, 'p2p.js')).href);
const { JsonStore } = await import(pathToFileURL(path.join(dist, 'store.js')).href);

const CONNECT_TIMEOUT_MS = 90_000;
fs.mkdirSync(work, { recursive: true });
const sending = role === 'send';
const started = Date.now();
const friends = [];
const node = new P2PNode({
  seed: crypto.randomBytes(32),
  friends: () => friends,
  messages: new JsonStore(path.join(work, `${role}-v2-messages.json`), {}),
  downloadDir: () => path.join(work, 'out-v2'),
});
await node.start();
fs.writeFileSync(path.join(work, `${role}.id`), node.id);
console.log(`[v2 ${role}] id ${node.id}`);

const peerFile = path.join(work, sending ? 'recv.id' : 'send.id');
let peer = '';
while (peer.length !== 64) {
  try { peer = fs.readFileSync(peerFile, 'utf8').trim(); } catch { /* not yet */ }
  if (peer.length !== 64) await new Promise(r => setTimeout(r, 100));
}
friends.push({ id: peer, name: 'peer' });
node.addFriend(peer);
const paired = Date.now();

const timeout = setTimeout(() => {
  console.log(`RESULT ${JSON.stringify({ ok: false, error: `pas de connexion après ${CONNECT_TIMEOUT_MS / 1000}s` })}`);
  process.exit(1);
}, CONNECT_TIMEOUT_MS);

let transferStart = 0;
node.on('friend-status', (_id, online) => {
  if (!online) return;
  console.log(`[v2 ${role}] connecté en ${Date.now() - paired} ms`);
  clearTimeout(timeout);
  if (sending && !transferStart) {
    transferStart = Date.now();
    node.sendFiles(peer, [file]);
  }
});
node.on('file-offer', t => {
  transferStart = Date.now();
  node.respondToOffer(t.id, true);
});
node.on('transfer', async t => {
  if (t.status === 'completed') {
    const seconds = (Date.now() - transferStart) / 1000;
    const result = {
      ok: true, role, bytes: t.fileSize, seconds,
      mb_per_s: +(t.fileSize / 1048576 / seconds).toFixed(1), total_s: (Date.now() - started) / 1000,
    };
    console.log(`RESULT ${JSON.stringify(result)}`);
    await new Promise(r => setTimeout(r, 500));
    await node.destroy();
    process.exit(0);
  } else if (['declined', 'cancelled', 'error'].includes(t.status)) {
    console.log(`RESULT ${JSON.stringify({ ok: false, error: `${t.status} ${t.error ?? ''}` })}`);
    process.exit(1);
  }
});
