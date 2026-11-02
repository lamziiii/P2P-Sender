import changelog from '../CHANGELOG.md?raw';

// What a version brings, from CHANGELOG.md (bundled with the app) or from
// the notes of an update: items ("- " lines) and plain notes.
export type Line = { item: boolean; text: string };
export type Release = { version: string; date: string; lines: Line[] };

export function parseLines(text: string): Line[] {
  return text
    .split(/\r?\n/)
    .map(line => line.trim())
    .filter(line => line && !line.startsWith('#'))
    .map(line => (line.startsWith('- ') ? { item: true, text: line.slice(2) } : { item: false, text: line }));
}

// Sections of CHANGELOG.md: "## <version> — <date>", newest first.
function parseChangelog(text: string): Release[] {
  return text.split(/^## /m).slice(1).map(section => {
    const [heading, ...body] = section.split(/\r?\n/);
    const [version, ...date] = heading.split(' — ');
    return { version: version.trim(), date: date.join(' — ').trim(), lines: parseLines(body.join('\n')) };
  });
}

export const RELEASES = parseChangelog(changelog);

const NEWS_SEEN = 'news-seen';

// After an update, once: the news of this version. A first launch (the
// welcome comes instead) only records the version.
export function newsToShow(welcomed: boolean): boolean {
  let seen: string | null = null;
  try {
    seen = localStorage.getItem(NEWS_SEEN);
    localStorage.setItem(NEWS_SEEN, __APP_VERSION__);
  } catch {
    return false;
  }
  return welcomed && seen !== __APP_VERSION__ && RELEASES.some(r => r.version === __APP_VERSION__);
}
