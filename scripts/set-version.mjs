// Set the app version everywhere it is written: package.json and its lock,
// src-tauri/tauri.conf.json, src-tauri/Cargo.toml and Cargo.lock.
//
//   npm run set-version 3.4.8           a release
//   npm run set-version 3.4.8.1         with the optional fourth number
//   npm run set-version 3.5.0-beta.1    a prerelease
//
// Cargo, npm and Tauri take three numbers only: a fourth one is written as
// semver build metadata (3.4.8+1). The tag, CHANGELOG.md, the app and the
// release files show it as 3.4.8.1, and the updater compares it (update.rs).

import { spawnSync } from 'node:child_process';
import { readFileSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';

const root = join(import.meta.dirname, '..');
const shown = process.argv[2] ?? '';
const match = /^(\d+\.\d+\.\d+)(?:\.([1-9]\d*)|-[0-9A-Za-z.-]+)?$/.exec(shown);
if (!match) {
  console.error('Usage : npm run set-version <x.y.z>, <x.y.z.n> (n à partir de 1) ou <x.y.z-beta.n>');
  process.exit(1);
}
const version = match[2] ? `${match[1]}+${match[2]}` : shown;

// Only the version lines change: the rest of each file stays as it is.
const edits = [
  ['package.json', [/^( {2}"version": ")[^"]*"/m]],
  ['package-lock.json', [/^( {2}"version": ")[^"]*"/m, /("packages": \{\s*"": \{[^}]*?"version": ")[^"]*"/]],
  ['src-tauri/tauri.conf.json', [/^( {2}"version": ")[^"]*"/m]],
  // The first version line is the one of [package].
  ['src-tauri/Cargo.toml', [/^(version = ")[^"]*"/m]],
];
for (const [file, patterns] of edits) {
  const path = join(root, file);
  let text = readFileSync(path, 'utf8');
  for (const pattern of patterns) {
    if (!pattern.test(text)) {
      console.error(`${file} : ligne de version introuvable`);
      process.exit(1);
    }
    text = text.replace(pattern, (_, before) => `${before}${version}"`);
  }
  writeFileSync(path, text);
}

// The build runs with --locked: Cargo.lock must carry the new version.
const cargo = spawnSync('cargo update -p p2pshare --offline', {
  cwd: join(root, 'src-tauri'),
  stdio: 'inherit',
  shell: true,
});
if (cargo.status !== 0) process.exit(cargo.status ?? 1);

console.log(`\nVersion ${shown}${version !== shown ? ` (${version} dans les fichiers)` : ''}.`);
console.log(`CHANGELOG.md : la section « ## ${shown} — à venir », puis la date à la sortie ; le tag : v${shown}.`);
