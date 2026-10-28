#!/usr/bin/env node
// Render versioned Changesets changelogs locally. This script never publishes.
import { mkdir, readFile, writeFile } from 'node:fs/promises';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { parseArgs } from 'node:util';
import { publicPackages } from './native-release-policy.mjs';

const packages = [
  ...publicPackages.map(([directory, name]) => ({ directory, name, private: false })),
  { directory: 'packages/desktop', name: '@agent-wechat/desktop', private: true },
];

const start = '<!-- agent-wechat:changesets:start -->';
const end = '<!-- agent-wechat:changesets:end -->';

export function changelogEntry(markdown, version) {
  const lines = markdown.replace(/\r\n/g, '\n').split('\n');
  let fence, begin, finish, found = 0;
  for (let i = 0; i < lines.length; i++) {
    const marker = lines[i].match(/^ {0,3}(`{3,}|~{3,})(.*)$/);
    if (marker) {
      if (!fence) fence = { character: marker[1][0], length: marker[1].length };
      else if (marker[1][0] === fence.character && marker[1].length >= fence.length && !marker[2].trim()) fence = undefined;
      continue;
    }
    if (fence) continue;
    const heading = lines[i].match(/^##[ \t]+(\S+)[ \t]*$/);
    if (!heading) continue;
    if (begin !== undefined && finish === undefined) finish = i;
    if (heading[1] === version) { found++; begin = i + 1; finish = undefined; }
  }
  if (found !== 1) throw new Error(`Expected one exact changelog entry for ${version}`);
  return lines.slice(begin, finish).join('\n').trim();
}

export function mergeReleaseNotes(existing, managed) {
  const occurrences = marker => existing.split(marker).length - 1;
  const starts = occurrences(start), ends = occurrences(end);
  const block = `${start}\n${managed.trim()}\n${end}`;
  if (!starts && !ends) return `${existing.trim() ? existing.trimEnd() + '\n\n' : ''}${block}\n`;
  if (starts !== 1 || ends !== 1 || existing.indexOf(end) < existing.indexOf(start)) {
    throw new Error('Existing release notes have ambiguous Changesets markers');
  }
  return existing.slice(0, existing.indexOf(start)) + block + existing.slice(existing.indexOf(end) + end.length);
}

export async function renderReleaseNotes({ root = process.cwd(), version, existing = '' } = {}) {
  const cli = JSON.parse(await readFile(join(root, packages[0].directory, 'package.json'), 'utf8'));
  version ??= cli.version;
  if (!/^\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?$/.test(version ?? '')) throw new Error('Invalid release notes version');
  const sections = [];
  for (const { directory, name, private: privatePackage } of packages) {
    const pkg = JSON.parse(await readFile(join(root, directory, 'package.json'), 'utf8'));
    if (pkg.name !== name || Boolean(pkg.private) !== privatePackage || pkg.version !== version) throw new Error('Release notes package/version mismatch');
    const entry = changelogEntry(await readFile(join(root, directory, 'CHANGELOG.md'), 'utf8'), version);
    if (entry) sections.push(`## ${name} ${version}\n\n${entry}`);
  }
  if (!sections.length) throw new Error('Versioned package changelogs contain no release notes; run Changesets versioning first');
  const managed = sections.join('\n\n') + '\n\nNative assets and desktop installers are published on GitHub; npm publication and deployment are separate release steps.';
  return mergeReleaseNotes(existing, managed);
}

if (process.argv[1] && fileURLToPath(import.meta.url) === resolve(process.argv[1])) {
  const { values } = parseArgs({ options: { output: { type: 'string' }, existing: { type: 'string' } } });
  if (!values.output) throw new Error('--output is required');
  const body = await renderReleaseNotes({ version: process.env.RELEASE_VERSION,
    existing: values.existing ? await readFile(values.existing, 'utf8') : '' });
  await mkdir(dirname(resolve(values.output)), { recursive: true });
  await writeFile(values.output, body);
}
