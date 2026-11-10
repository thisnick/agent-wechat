#!/usr/bin/env node
// Translations of the desktop UI and the docs.
//
//   pnpm i18n status [--json]                   what is missing or out of date, per language
//   pnpm i18n check                             fail on English-side errors and broken translations
//   pnpm i18n stamp <lang> [--machine] [items]  record that translations match the current English
//
// English is the source: packages/client-ui/locales/en.json for the UI and the pages in
// docs/src/content/docs/ for the docs. A translation may be incomplete: a missing key
// or page shows English, so missing never fails. What fails: code using a key English
// lacks, English keys no code uses, and a translation that would render wrongly (bad
// JSON, unknown keys, placeholders that differ from English, missing plural forms,
// empty text, docs pages whose code, components, links or anchors differ from English).
//
// Each UI translation has a <lang>.meta.json recording, per key, a hash of the English
// it was translated from (`source`) and, when a machine wrote it, a hash of what it
// wrote (`machine`). A docs page records `sourceHash` in its frontmatter. When the
// English changes, the translation is stale: reported, exempt from placeholder checks,
// and due for an update. A machine translation a person has since edited counts as
// the person's. `stamp` updates the records: with no items, only unrecorded ones;
// name keys or pages to confirm stale translations are still right.
import { readdirSync, readFileSync, statSync, writeFileSync } from 'node:fs';
import { basename, dirname, join, relative } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { compareDocs, docs, frontmatter, hash, stamp as stampPage } from './docs.mjs';

export const root = join(dirname(fileURLToPath(import.meta.url)), '../..');

/** Where the UI catalogs and the code that reads them live, relative to the repository root. */
export const ui = {
  locales: 'packages/client-ui/locales',
  code: ['packages/client-ui/src'],
  // The desktop app reads `desktopShell.*` keys from the same catalogs.
  desktopCode: ['packages/desktop/src-tauri/src'],
  registries: {
    'packages/client-ui/src/i18n.ts': /tag: "([^"]+)"/g,
    'packages/desktop/src-tauri/src/i18n.rs': /\(\s*"([^"]+)",\s*include_str!/g,
  },
};

const pluralSuffix = /_(zero|one|two|few|many|other)$/;

/** Flattens nested catalogs into `a.b.c` keys. */
export function flatten(object, prefix = '') {
  const out = {};
  for (const [key, value] of Object.entries(object ?? {})) {
    const path = prefix ? `${prefix}.${key}` : key;
    if (value !== null && typeof value === 'object' && !Array.isArray(value)) Object.assign(out, flatten(value, path));
    else out[path] = value;
  }
  return out;
}

/** The key code uses for a plural entry: `n.items_one` → `n.items`. */
export const baseKey = key => key.replace(pluralSuffix, '');

/** `{{name}}` placeholders in a text, sorted and unique. */
export const placeholders = text => [...new Set([...String(text).matchAll(/\{\{\s*([\w.]+)\s*\}\}/g)].map(match => match[1]))].sort();

function files(dir, extensions) {
  let out = [];
  for (const name of readdirSync(dir)) {
    const path = join(dir, name);
    if (statSync(path).isDirectory()) out = out.concat(files(path, extensions));
    else if (extensions.some(extension => name.endsWith(extension))) out.push(path);
  }
  return out;
}

/**
 * The source text of the first argument of each call to `name(...)`. A small scanner
 * that tracks brackets and skips over strings, so `t(a ? "x.y" : "x.z", {...})` yields
 * `a ? "x.y" : "x.z"`.
 */
export function firstArguments(source, name) {
  const out = [];
  const call = new RegExp(`(?<![\\w.$])${name}\\(`, 'g');
  for (const match of source.matchAll(call)) {
    let depth = 0;
    let i = match.index + match[0].length;
    const start = i;
    for (; i < source.length; i++) {
      const c = source[i];
      if (c === '"' || c === "'" || c === '`') {
        for (i++; i < source.length && source[i] !== c; i++) if (source[i] === '\\') i++;
      } else if ('([{'.includes(c)) depth++;
      else if (')]}'.includes(c)) {
        if (depth === 0) break;
        depth--;
      } else if (c === ',' && depth === 0) break;
    }
    out.push(source.slice(start, i));
  }
  return out;
}

const literals = text => [...text.matchAll(/"((?:[^"\\]|\\.)*)"/g)].map(match => match[1]);

/** Keys passed to `t(...)` and `richText(...)` in UI code. */
export function keyLiterals(source) {
  return new Set(['t', 'richText'].flatMap(name => firstArguments(source, name)).flatMap(literals).filter(text => /^[a-z][A-Za-z0-9]*(\.[A-Za-z0-9_]+)+$/.test(text)));
}

/**
 * Keys passed to the desktop app's text functions, relative to `desktopShell.`.
 * `text_in(language, key)` takes the key second; the others take it first.
 */
export function desktopKeyLiterals(source) {
  const found = new Set();
  for (const name of ['text', 'text_with', 'label', 'notification_text'])
    for (const arg of firstArguments(source, name)) for (const text of literals(arg)) found.add(text);
  for (const match of source.matchAll(/(?<![\w.])text_in\(\s*[^,]+,\s*"(\w+)"/g)) found.add(match[1]);
  // Failure alerts pass the title key as the third argument.
  for (const match of source.matchAll(/action_failed\(\s*[^,]+,\s*[^,]+,\s*"(\w+)"/g)) found.add(match[1]);
  return found;
}

/**
 * Checks the UI catalogs. `read(path)` returns a file's text, relative to the repository
 * root, and `list(dir, extensions)` lists files; both are swappable for tests.
 * Returns a list of problems, each `{ file, key?, message }`.
 */
/** Reads every UI catalog and its meta file. Parse errors are returned as problems. */
export function loadUi({ read, list, config = ui }) {
  const problems = [];
  const catalogs = {};
  const metas = {};
  for (const file of list(config.locales, ['.json']).filter(file => !file.endsWith('.meta.json'))) {
    const tag = basename(file, '.json');
    try {
      catalogs[tag] = flatten(JSON.parse(read(file)));
    } catch (error) {
      problems.push({ file, message: `does not parse: ${error.message}` });
      continue;
    }
    if (tag === 'en') continue;
    const metaFile = `${config.locales}/${tag}.meta.json`;
    try {
      metas[tag] = JSON.parse(read(metaFile));
    } catch (error) {
      metas[tag] = {};
      if (!/no file|ENOENT/.test(error.message)) problems.push({ file: metaFile, message: `does not parse: ${error.message}` });
    }
  }
  return { catalogs, metas, problems };
}

/** The English text a translated key was made from: plural forms compare with `_other`. */
const sourceText = (en, key) => en[key] ?? en[`${baseKey(key)}_other`];

/** Whether a translated key's recorded English differs from the current English. */
const isStale = (meta, en, key) => meta?.[key]?.source !== undefined && meta[key].source !== hash(sourceText(en, key) ?? '');

export function checkUi({ read, list, config = ui }) {
  const { catalogs, metas, problems } = loadUi({ read, list, config });
  const en = catalogs.en;
  if (!en) return [...problems, { file: `${config.locales}/en.json`, message: 'English catalog is missing' }];

  const enBases = new Set(Object.keys(en).map(baseKey));

  // English side: every key code uses exists, and every English key is used.
  const used = new Set();
  for (const file of config.code.flatMap(dir => list(dir, ['.ts', '.tsx']))) {
    for (const key of keyLiterals(read(file))) {
      used.add(key);
      if (!enBases.has(key)) problems.push({ file, key, message: 'used in code but missing from en.json' });
    }
  }
  // i18n.rs defines the lookup and tests it with made-up keys.
  for (const file of config.desktopCode.flatMap(dir => list(dir, ['.rs'])).filter(file => !file.endsWith('/i18n.rs'))) {
    for (const key of desktopKeyLiterals(read(file))) {
      const full = `desktopShell.${key}`;
      used.add(full);
      if (!enBases.has(full)) problems.push({ file, key: full, message: 'used by the desktop app but missing from en.json' });
    }
  }
  for (const key of enBases) {
    if (!used.has(key)) problems.push({ file: `${config.locales}/en.json`, key, message: 'not used by any code' });
  }
  for (const [key, value] of Object.entries(en)) {
    if (typeof value !== 'string' || !value.trim()) problems.push({ file: `${config.locales}/en.json`, key, message: 'English text is empty' });
  }

  // Every catalog is registered in the UI and the desktop app, and nothing else is.
  const tags = Object.keys(catalogs).sort();
  for (const [file, pattern] of Object.entries(config.registries)) {
    const listed = [...read(file).matchAll(pattern)].map(match => match[1]).sort();
    for (const tag of tags) if (!listed.includes(tag)) problems.push({ file, message: `${tag}.json is not listed here` });
    for (const tag of listed) if (!tags.includes(tag)) problems.push({ file, message: `lists ${tag} but ${config.locales}/${tag}.json does not exist` });
  }

  // Translations: only render-breaking problems fail.
  for (const [tag, catalog] of Object.entries(catalogs)) {
    if (tag === 'en') continue;
    const file = `${config.locales}/${tag}.json`;
    let categories;
    try {
      categories = new Intl.PluralRules(tag).resolvedOptions().pluralCategories;
    } catch {
      problems.push({ file, message: `${tag} is not a valid language tag` });
      continue;
    }
    const translatedPlurals = new Map();
    for (const [key, value] of Object.entries(catalog)) {
      const base = baseKey(key);
      const isPlural = base !== key;
      if (!enBases.has(base) || (!isPlural && !(key in en))) {
        problems.push({ file, key, message: 'not in en.json (remove it)' });
        continue;
      }
      if (isPlural) translatedPlurals.set(base, [...(translatedPlurals.get(base) ?? []), key.slice(base.length + 1)]);
      if (typeof value !== 'string' || !value.trim()) {
        problems.push({ file, key, message: 'empty text (remove the key to show English instead)' });
        continue;
      }
      // A stale translation is reported by `status`, not failed: English may have renamed a placeholder.
      if (isStale(metas[tag], en, key)) continue;
      const source = sourceText(en, key);
      const expected = placeholders(source);
      const actual = placeholders(value);
      // A plural form may leave out {{count}}, as in "one item".
      const missing = expected.filter(name => !actual.includes(name) && !(isPlural && name === 'count'));
      const extra = actual.filter(name => !expected.includes(name));
      if (missing.length || extra.length)
        problems.push({ file, key, message: `placeholders differ from English (${[...missing.map(n => `missing {{${n}}}`), ...extra.map(n => `unknown {{${n}}}`)].join(', ')})` });
      if (isPlural) {
        const form = key.slice(base.length + 1);
        if (!categories.includes(form)) problems.push({ file, key, message: `${tag} has no "${form}" plural form` });
      }
    }
    for (const [base, forms] of translatedPlurals) {
      const missingForms = categories.filter(form => !forms.includes(form));
      if (missingForms.length) problems.push({ file, key: base, message: `missing plural forms: ${missingForms.join(', ')}` });
    }
  }
  return problems;
}

/**
 * Per UI language: `missing` English keys with no translation, `stale` translations
 * whose English changed, `unstamped` translations with no record, `machine` keys a
 * machine wrote and no one has edited since. Keys are as code uses them (no plural suffix).
 */
export function statusUi({ read, list, config = ui }) {
  const { catalogs, metas } = loadUi({ read, list, config });
  const en = catalogs.en ?? {};
  const bases = [...new Set(Object.keys(en).map(baseKey))];
  const out = {};
  for (const [tag, catalog] of Object.entries(catalogs)) {
    if (tag === 'en') continue;
    const meta = metas[tag] ?? {};
    const keysFor = base => Object.keys(catalog).filter(key => baseKey(key) === base);
    const entry = { translated: 0, missing: [], stale: [], unstamped: [], machine: [] };
    for (const base of bases) {
      const keys = keysFor(base);
      if (!keys.length) {
        entry.missing.push(base);
        continue;
      }
      entry.translated++;
      if (keys.some(key => isStale(meta, en, key))) entry.stale.push(base);
      else if (keys.some(key => meta[key]?.source === undefined)) entry.unstamped.push(base);
      if (keys.every(key => meta[key]?.machine && meta[key].machine === hash(catalog[key]))) entry.machine.push(base);
    }
    out[tag] = entry;
  }
  return out;
}

/** Docs languages from docs/locales.mjs: `{ folder: lang }`. */
async function docsLocaleMap() {
  const { locales, translatedLocales } = await import(pathToFileURL(join(root, 'docs/locales.mjs')).href);
  return Object.fromEntries(translatedLocales.map(folder => [folder, locales[folder].lang]));
}

function readDocs(list, read) {
  return Object.fromEntries(list(docs.content, ['.md', '.mdx']).map(path => [relative(docs.content, path).replaceAll('\\', '/'), read(path)]));
}

const fsRead = path => readFileSync(join(root, path), 'utf8');
const fsList = (dir, extensions) => files(join(root, dir), extensions).map(path => relative(root, path));
const print = ({ file, key, message }) => console.error(`${file}${key ? `: ${key}` : ''}: ${message}`);

/** Status of every language, UI and docs, keyed by language tag. */
export async function status({ read = fsRead, list = fsList } = {}) {
  const folders = await docsLocaleMap();
  const pages = readDocs(list, read);
  const docsStatus = compareDocs(pages, Object.keys(folders)).status;
  const uiStatus = statusUi({ read, list });
  const tags = [...new Set([...Object.keys(uiStatus), ...Object.values(folders)])].sort();
  return Object.fromEntries(tags.map(tag => {
    const folder = Object.keys(folders).find(name => folders[name] === tag);
    return [tag, { ui: uiStatus[tag] ?? null, docs: folder ? { folder, ...docsStatus[folder] } : null }];
  }));
}

async function check() {
  const problems = checkUi({ read: fsRead, list: fsList });
  const folders = await docsLocaleMap();
  problems.push(...compareDocs(readDocs(fsList, fsRead), Object.keys(folders)).problems);
  problems.forEach(print);
  if (problems.length) {
    console.error(`\n${problems.length} translation problem${problems.length === 1 ? '' : 's'}. Missing or out-of-date translations are fine; these are not.`);
    return 1;
  }
  console.log('Translations are consistent with English.');
  return 0;
}

async function showStatus(json) {
  const result = await status();
  if (json) {
    console.log(JSON.stringify(result, null, 2));
    return 0;
  }
  const list = (label, items) => items.length && console.log(`  ${label}: ${items.join(', ')}`);
  for (const [tag, { ui: u, docs: d }] of Object.entries(result)) {
    console.log(`${tag}`);
    if (u) {
      console.log(`  UI: ${u.translated} translated (${u.machine.length} by machine), ${u.missing.length} missing, ${u.stale.length} out of date, ${u.unstamped.length} unrecorded`);
      list('UI missing', u.missing);
      list('UI out of date', u.stale);
    }
    if (d) {
      console.log(`  Docs (${d.folder}/): ${d.current.length + d.unstamped.length} current, ${d.missing.length} missing, ${d.stale.length} out of date, ${d.unstamped.length} unrecorded`);
      list('Docs missing', d.missing);
      list('Docs out of date', d.stale);
    }
  }
  return 0;
}

/**
 * Records that translations match the current English. `items` are UI keys or docs
 * pages (`setup/install.mdx`); with none, only translations with no record are stamped.
 */
export async function stampLanguage(tag, { machine = false, items = [] } = {}, { read = fsRead, list = fsList, write = (path, text) => writeFileSync(join(root, path), text) } = {}) {
  const changed = [];
  const { catalogs, metas } = loadUi({ read, list });
  const catalog = catalogs[tag];
  if (catalog) {
    const en = catalogs.en;
    const meta = { ...metas[tag] };
    for (const key of Object.keys(catalog)) {
      const named = items.includes(key) || items.includes(baseKey(key));
      if (items.length ? !named : meta[key]?.source !== undefined) continue;
      meta[key] = { source: hash(sourceText(en, key) ?? ''), ...(machine ? { machine: hash(catalog[key]) } : {}) };
      changed.push(key);
    }
    for (const key of Object.keys(meta)) if (!(key in catalog)) delete meta[key];
    const sorted = Object.fromEntries(Object.keys(meta).sort().map(key => [key, meta[key]]));
    // One line per key keeps diffs readable.
    const lines = Object.entries(sorted).map(([key, value]) => `  ${JSON.stringify(key)}: ${JSON.stringify(value)}`);
    write(`${ui.locales}/${tag}.meta.json`, `{\n${lines.join(',\n')}\n}\n`);
  }
  const folders = await docsLocaleMap();
  const folder = Object.keys(folders).find(name => folders[name] === tag);
  if (folder) {
    const pages = readDocs(list, read);
    for (const [path, text] of Object.entries(pages)) {
      if (!path.startsWith(`${folder}/`)) continue;
      const page = path.slice(folder.length + 1);
      if (!(page in pages)) continue;
      const named = items.includes(page) || items.includes(path);
      if (items.length ? !named : frontmatter(text).fields.sourceHash) continue;
      write(`${docs.content}/${path}`, stampPage(text, hash(pages[page])));
      changed.push(path);
    }
  }
  if (!catalog && !folder) throw new Error(`No UI catalog or docs folder for ${tag}`);
  return changed;
}

async function main(argv) {
  const [command, ...rest] = argv;
  if (command === 'check') return check();
  if (command === 'status') return showStatus(rest.includes('--json'));
  if (command === 'stamp' && rest[0] && !rest[0].startsWith('--')) {
    const [tag, ...args] = rest;
    const changed = await stampLanguage(tag, { machine: args.includes('--machine'), items: args.filter(arg => !arg.startsWith('--')) });
    console.log(changed.length ? `Recorded ${changed.length} translation${changed.length === 1 ? '' : 's'} as matching the current English.` : 'Nothing to record.');
    return 0;
  }
  console.error('Usage: pnpm i18n status [--json] | check | stamp <lang> [--machine] [keys or pages]');
  return 2;
}

if (process.argv[1] && fileURLToPath(import.meta.url) === process.argv[1]) process.exitCode = await main(process.argv.slice(2));
