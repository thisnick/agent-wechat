#!/usr/bin/env node
// The translation bot's steps, run by .github/workflows/i18n-translate.yml.
//
//   node scripts/i18n/publish.mjs plan <work.json>       write what needs translating; prints the count
//   node scripts/i18n/publish.mjs finish <work.json>     keep only valid locale edits and record them
//   node scripts/i18n/publish.mjs publish <work.json>    push the branch and open or update the pull request
//
// Between plan and finish, Claude edits the translation files listed in work.json.
// finish trusts nothing it wrote: files outside the translations are restored, and a
// translation that fails `pnpm i18n check` is removed so English shows instead.
import { execFileSync } from 'node:child_process';
import { existsSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { baseKey, checkUi, flatten, loadUi, root, stampLanguage, status, ui } from './index.mjs';
import { compareDocs, docs, hash, stamp } from './docs.mjs';

/** Shown on machine-translated docs pages until a person reviews them. */
export const MACHINE_NOTICE = 'This page was translated by machine and not yet reviewed by a person.';

/**
 * Which files the bot may change: a translation catalog (never en.json or its meta
 * file is written by finish, not Claude) or a page in a translated docs folder.
 */
export function allowedPath(path, { tags, folders }) {
  const catalog = path.match(new RegExp(`^${ui.locales}/([^/]+)\\.json$`));
  if (catalog) return catalog[1] !== 'en' && tags.includes(catalog[1]);
  const page = path.match(new RegExp(`^${docs.content}/([^/]+)/.+\\.mdx?$`));
  return !!page && folders.includes(page[1]);
}

/**
 * The work list from `status`: per language, UI keys and docs pages that are missing
 * or out of date, with the English text and the current translation.
 */
export function planWork(result, { en, catalogs, pages }) {
  const work = {};
  for (const [tag, { ui: u, docs: d }] of Object.entries(result)) {
    const entry = { ui: [], docs: [] };
    for (const [key, state] of [...(u?.missing ?? []).map(key => [key, 'missing']), ...(u?.stale ?? []).map(key => [key, 'stale'])]) {
      const forms = Object.keys(en).filter(name => baseKey(name) === key);
      const current = Object.entries(catalogs[tag] ?? {}).filter(([name]) => baseKey(name) === key);
      entry.ui.push({ key, state, english: Object.fromEntries(forms.map(name => [name, en[name]])), current: Object.fromEntries(current) });
    }
    for (const [page, state] of [...(d?.missing ?? []).map(page => [page, 'missing']), ...(d?.stale ?? []).map(page => [page, 'stale'])]) {
      entry.docs.push({ page, state, english: `${docs.content}/${page}`, translation: `${docs.content}/${d.folder}/${page}`, sourceHash: hash(pages[page]) });
    }
    if (entry.ui.length || entry.docs.length) work[tag] = { folder: d?.folder ?? null, ...entry };
  }
  return work;
}

export const count = work => Object.values(work).reduce((sum, entry) => sum + entry.ui.length + entry.docs.length, 0);

const git = (...args) => execFileSync('git', args, { cwd: root, encoding: 'utf8' }).trim();
/** Paths with uncommitted changes, including untracked files. */
const changedPaths = () => execFileSync('git', ['status', '--porcelain', '-z', '--untracked-files=all'], { cwd: root, encoding: 'utf8' })
  .split('\0').filter(Boolean).map(entry => entry.slice(3));
const read = path => readFileSync(join(root, path), 'utf8');
const list = dir => git('ls-files', '--cached', '--others', '--exclude-standard', dir).split('\n').filter(Boolean);
const listExt = (dir, extensions) => list(dir).filter(path => extensions.some(extension => path.endsWith(extension)));

async function plan(file) {
  const result = await status();
  const { catalogs } = loadUi({ read: path => read(path), list: listExt });
  const pages = Object.fromEntries(listExt(docs.content, ['.md', '.mdx']).map(path => [path.slice(docs.content.length + 1), read(path)]));
  const work = planWork(result, { en: catalogs.en, catalogs, pages });
  writeFileSync(file, `${JSON.stringify(work, null, 2)}\n`);
  console.log(count(work));
}

/**
 * Catalog keys (with plural suffixes) for the work list's UI items whose text Claude
 * changed. An item it left alone keeps its old record: still missing or out of date.
 */
export function changedKeys(catalog, entry) {
  return Object.keys(catalog).filter(name => entry.ui.some(item => baseKey(name) === item.key && item.current?.[name] !== catalog[name]));
}

/**
 * A file reader for `checkUi` that treats the UI keys Claude changed as up to date:
 * their meta entries record the current English. Without it, a key Claude re-translated
 * would still look stale and skip the placeholder and plural checks.
 */
export function readAsCurrent(read, work, en) {
  return path => {
    const tag = path.startsWith(`${ui.locales}/`) && path.endsWith('.meta.json') ? path.slice(ui.locales.length + 1, -'.meta.json'.length) : null;
    if (!tag || !work[tag]) return read(path);
    let meta = {};
    try {
      meta = JSON.parse(read(path));
    } catch {
      // No record yet.
    }
    let catalog = {};
    try {
      catalog = flatten(JSON.parse(read(`${ui.locales}/${tag}.json`)));
    } catch {
      // A catalog that does not parse is reported by checkUi itself.
    }
    for (const key of changedKeys(catalog, work[tag])) meta[key] = { source: hash(en[key] ?? en[`${baseKey(key)}_other`] ?? '') };
    return JSON.stringify(meta);
  };
}

/** Adds the machine-translation notice to a docs page's frontmatter (Starlight's `banner`). */
export function withNotice(text, notice) {
  const match = text.match(/^---\n([\s\S]*?)\n---\n?/);
  if (!match || /^banner:/m.test(match[1])) return text;
  return `---\n${match[1]}\nbanner:\n  content: ${JSON.stringify(notice)}\n---\n${text.slice(match[0].length)}`;
}

async function finish(file) {
  const work = JSON.parse(readFileSync(file, 'utf8'));
  const tags = Object.keys(work);
  const folders = Object.values(work).map(entry => entry.folder).filter(Boolean);
  const report = { restored: [], dropped: [] };
  // 1. Anything outside the translation files goes back to how it was.
  for (const path of changedPaths()) {
    if (allowedPath(path, { tags, folders })) continue;
    if (git('ls-files', path)) git('checkout', '--', path);
    else rmSync(join(root, path), { force: true });
    report.restored.push(path);
  }
  // 2. Drop translations that would render wrongly; English shows instead.
  const en = loadUi({ read, list: listExt }).catalogs.en ?? {};
  const problems = checkUi({ read: readAsCurrent(read, work, en), list: listExt });
  for (const { file: path, key } of problems) {
    const tag = path.match(/([^/]+)\.json$/)?.[1];
    if (!key || !tags.includes(tag)) continue;
    const catalog = JSON.parse(read(path));
    const parts = key.split('.');
    const parent = parts.slice(0, -1).reduce((node, part) => node?.[part], catalog);
    if (parent) {
      const base = baseKey(parts.at(-1));
      for (const name of Object.keys(parent)) if (baseKey(name) === base) delete parent[name];
      writeFileSync(join(root, path), `${JSON.stringify(catalog, null, 2)}\n`);
      report.dropped.push(`${tag}: ${key}`);
    }
  }
  const pages = Object.fromEntries(listExt(docs.content, ['.md', '.mdx']).map(path => [path.slice(docs.content.length + 1), read(path)]));
  const stampedPages = new Set(Object.values(work).flatMap(entry => entry.docs.map(item => item.translation)));
  // Pages Claude wrote are compared as if current, so their structure is checked.
  for (const item of Object.values(work).flatMap(entry => entry.docs)) {
    const rel = item.translation.slice(docs.content.length + 1);
    if (pages[rel]) pages[rel] = stamp(pages[rel], item.sourceHash);
  }
  for (const { file: path, message } of compareDocs(pages, folders).problems) {
    if (!stampedPages.has(path)) continue;
    if (git('ls-files', path)) git('checkout', '--', path);
    else rmSync(join(root, path), { force: true });
    report.dropped.push(`${path}: ${message}`);
  }
  // 3. Record what is left as machine-translated from the current English.
  const notices = existsSync(join(process.env.RUNNER_TEMP ?? '/tmp', 'notices.json')) ? JSON.parse(readFileSync(join(process.env.RUNNER_TEMP ?? '/tmp', 'notices.json'), 'utf8')) : {};
  for (const [tag, entry] of Object.entries(work)) {
    const catalog = existsSync(join(root, `${ui.locales}/${tag}.json`)) ? flatten(JSON.parse(read(`${ui.locales}/${tag}.json`))) : {};
    const keys = changedKeys(catalog, entry);
    const written = entry.docs.filter(item => existsSync(join(root, item.translation)) && git('status', '--porcelain', item.translation));
    for (const item of written) writeFileSync(join(root, item.translation), withNotice(read(item.translation), notices[tag] || MACHINE_NOTICE));
    const items = [...keys, ...written.map(item => item.page)];
    if (items.length) await stampLanguage(tag, { machine: true, items });
  }
  writeFileSync(file.replace(/\.json$/, '.report.json'), `${JSON.stringify(report, null, 2)}\n`);
  console.log(JSON.stringify(report));
}

/** The pull request body: what changed, what was dropped and how to review. */
export function prBody({ work, report, source, backTranslation }) {
  const lines = [
    source ? `Translations for the English changes in #${source}. Merge this into that branch before merging it.` : 'Translations for English changes on main.',
    '',
    'Written by Claude (Haiku) and checked by `pnpm i18n check`; nothing here has been reviewed by a person yet. Docs pages carry a "translated by machine" banner until someone reviews and removes it.',
    '',
    '| Language | UI strings | Docs pages |',
    '| --- | --- | --- |',
    ...Object.entries(work).map(([tag, entry]) => `| ${tag} | ${entry.ui.length} | ${entry.docs.length} |`),
  ];
  if (report.dropped.length) lines.push('', 'Left in English because the translation failed the check:', '', ...report.dropped.map(item => `- ${item}`));
  if (report.restored.length) lines.push('', 'Edits outside the translation files were discarded:', '', ...report.restored.map(item => `- \`${item}\``));
  if (backTranslation) lines.push('', '<details><summary>Back-translation into English, for reviewers who do not read the language</summary>', '', backTranslation.trim(), '', '</details>');
  lines.push('', 'This branch is rebuilt whenever the English changes again; edits made here are replaced. Merge first, then edit.');
  return lines.join('\n');
}

function gh(...args) {
  return execFileSync('gh', args, { cwd: root, encoding: 'utf8', env: process.env }).trim();
}

async function publish(file) {
  const work = JSON.parse(readFileSync(file, 'utf8'));
  const report = JSON.parse(readFileSync(file.replace(/\.json$/, '.report.json'), 'utf8'));
  const changed = changedPaths().length > 0;
  const source = process.env.SOURCE_PR || '';
  const base = process.env.SOURCE_BRANCH || 'main';
  const branch = source ? `i18n/pr-${source}` : 'i18n/sync';
  const backFile = join(process.env.RUNNER_TEMP ?? '/tmp', 'back-translation.md');
  const backTranslation = existsSync(backFile) ? readFileSync(backFile, 'utf8') : '';
  const open = JSON.parse(gh('pr', 'list', '--head', branch, '--state', 'open', '--json', 'number') || '[]');
  if (!changed) {
    console.log('No translation changes to publish.');
    for (const { number } of open) gh('pr', 'close', String(number), '--comment', 'The English no longer needs these translations.', '--delete-branch');
    return;
  }
  git('config', 'user.name', 'github-actions[bot]');
  git('config', 'user.email', '41898282+github-actions[bot]@users.noreply.github.com');
  git('checkout', '-B', branch);
  git('add', '-A');
  git('commit', '-m', source ? `Translate the English changes in #${source}` : 'Translate English changes on main');
  git('push', '--force', 'origin', `HEAD:refs/heads/${branch}`);
  const body = prBody({ work, report, source, backTranslation });
  const title = source ? `Translations for #${source}` : 'Translations for English changes on main';
  if (open.length) gh('pr', 'edit', String(open[0].number), '--body', body, '--title', title);
  else gh('pr', 'create', '--head', branch, '--base', base, '--title', title, '--body', body);
  if (!source) {
    // A stacked translation pull request whose source merged first now targets main; this one replaces it.
    for (const pr of JSON.parse(gh('pr', 'list', '--base', 'main', '--state', 'open', '--json', 'number,headRefName'))) {
      if (pr.headRefName.startsWith('i18n/pr-')) gh('pr', 'close', String(pr.number), '--comment', 'Replaced by the translations pull request for main.', '--delete-branch');
    }
  }
}

async function main([command, file]) {
  if (!file) {
    console.error('Usage: node scripts/i18n/publish.mjs plan|finish|publish <work.json>');
    return 2;
  }
  if (command === 'plan') await plan(file);
  else if (command === 'finish') await finish(file);
  else if (command === 'publish') await publish(file);
  else return 2;
  return 0;
}

if (process.argv[1] && fileURLToPath(import.meta.url) === process.argv[1]) process.exitCode = await main(process.argv.slice(2));
