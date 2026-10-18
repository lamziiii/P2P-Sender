// Build the app, then copy the installer and the executable into release/.
//
// Cargo's working files (several GB) stay out of the project folder: Windows
// refuses to run the build scripts Cargo compiles inside OneDrive, and
// OneDrive would sync them all. Only the finished files land here.

import { spawnSync } from 'node:child_process';
import { copyFileSync, existsSync, mkdirSync, readdirSync } from 'node:fs';
import { homedir } from 'node:os';
import { join } from 'node:path';

const root = join(import.meta.dirname, '..');
const target =
  process.env.CARGO_TARGET_DIR ||
  join(process.env.LOCALAPPDATA || join(homedir(), '.cache'), 'p2pshare-target');

const build = spawnSync(['npx tauri build', ...process.argv.slice(2)].join(' '), {
  cwd: root,
  stdio: 'inherit',
  shell: true,
  env: { ...process.env, CARGO_TARGET_DIR: target },
});
if (build.status !== 0) process.exit(build.status ?? 1);

const out = join(root, 'release');
mkdirSync(out, { recursive: true });
const copied = [];
const copy = (from, name) => {
  copyFileSync(from, join(out, name));
  copied.push(name);
};

const release = join(target, 'release');
for (const exe of ['p2pshare.exe', 'p2pshare']) {
  if (existsSync(join(release, exe))) copy(join(release, exe), exe);
}
for (const [folder, ext] of [['nsis', '.exe'], ['msi', '.msi'], ['dmg', '.dmg'], ['appimage', '.AppImage'], ['deb', '.deb']]) {
  const dir = join(release, 'bundle', folder);
  if (!existsSync(dir)) continue;
  for (const name of readdirSync(dir).filter(n => n.endsWith(ext))) copy(join(dir, name), name);
}

console.log(`\nCopié dans ${out} :\n  ${copied.join('\n  ')}`);
