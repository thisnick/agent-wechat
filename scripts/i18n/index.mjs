#!/usr/bin/env node
// Checks translations of the desktop UI.
//
//   pnpm i18n check      fail on English-side errors and broken translations
//
// English (packages/client-ui/locales/en.json) is the source. A translation may be
// incomplete: a missing key shows the English text, so missing keys never fail.
// What fails: code using a key English lacks, English keys no code uses, and a
// translation that would render wrongly (bad JSON, unknown keys, placeholders that
// differ from English, missing plural forms, empty text).
import { readdirSync, readFileSync, statSync } from 'node:fs';
import { basename, dirname, join, relative } from 'node:path';
import { fileURLToPath } from 'node:url';

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
export function checkUi({ read, list, config = ui }) {
  const problems = [];
  const localeFiles = list(config.locales, ['.json']).filter(file => !file.endsWith('.meta.json'));
  const catalogs = {};
  for (const file of localeFiles) {
    const tag = basename(file, '.json');
    try {
      catalogs[tag] = flatten(JSON.parse(read(file)));
    } catch (error) {
      problems.push({ file, message: `does not parse: ${error.message}` });
    }
  }
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
      const source = isPlural ? (en[`${base}_other`] ?? en[key]) : en[key];
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

const fsRead = path => readFileSync(join(root, path), 'utf8');
const fsList = (dir, extensions) => files(join(root, dir), extensions).map(path => relative(root, path));

function main(argv) {
  const [command] = argv;
  if (command !== 'check') {
    console.error('Usage: pnpm i18n check');
    return 2;
  }
  const problems = checkUi({ read: fsRead, list: fsList });
  for (const { file, key, message } of problems) console.error(`${file}${key ? `: ${key}` : ''}: ${message}`);
  if (problems.length) {
    console.error(`\n${problems.length} translation problem${problems.length === 1 ? '' : 's'}. Missing translations are fine; these are not.`);
    return 1;
  }
  console.log('UI translations are consistent with English.');
  return 0;
}

if (process.argv[1] && fileURLToPath(import.meta.url) === process.argv[1]) process.exitCode = main(process.argv.slice(2));
