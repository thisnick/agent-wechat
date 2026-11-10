import test from 'node:test';
import assert from 'node:assert/strict';
import { baseKey, checkUi, desktopKeyLiterals, firstArguments, flatten, keyLiterals, placeholders } from './index.mjs';

const config = {
  locales: 'locales',
  code: ['src'],
  desktopCode: ['rs'],
  registries: { 'src/i18n.ts': /tag: "([^"]+)"/g },
};

/** Runs the check over an in-memory tree: `{ path: text }`. */
function run(tree) {
  const read = path => {
    if (!(path in tree)) throw new Error(`no file ${path}`);
    return tree[path];
  };
  const list = (dir, extensions) => Object.keys(tree).filter(path => path.startsWith(`${dir}/`) && extensions.some(extension => path.endsWith(extension)));
  return checkUi({ read, list, config }).map(({ file, key, message }) => `${file}${key ? `: ${key}` : ''}: ${message}`);
}

const en = { app: { hello: 'Hello {{name}}', items_one: '{{count}} item', items_other: '{{count}} items' }, desktopShell: { trayOpen: 'Open' } };
const code = 'const a = t("app.hello", { name }); const b = t("app.items", { count });';
const base = {
  'locales/en.json': JSON.stringify(en),
  'src/main.ts': code,
  'src/i18n.ts': 'const languages = [{ tag: "en" }, { tag: "zh-CN" }];',
  'rs/main.rs': 'let s = i18n::text("trayOpen");',
};

test('a partial translation passes: missing keys fall back to English', () => {
  assert.deepEqual(run({ ...base, 'locales/zh-CN.json': JSON.stringify({ app: { hello: '你好 {{name}}' } }) }), []);
});

test('English-side errors fail: unknown keys in code and unused English keys', () => {
  const problems = run({ ...base, 'locales/zh-CN.json': '{}', 'src/main.ts': 'const a = t("app.hello", { name }); t("app.gone");' });
  assert.deepEqual(problems, [
    'src/main.ts: app.gone: used in code but missing from en.json',
    'locales/en.json: app.items: not used by any code',
  ]);
});

test('broken translations fail: placeholders, unknown keys, empty text, plural forms, bad JSON', () => {
  const zh = { app: { hello: '你好 {{nam}}', extra: 'x', items_one: '{{count}} 项', items_other: '' } };
  assert.deepEqual(run({ ...base, 'locales/zh-CN.json': JSON.stringify(zh) }), [
    'locales/zh-CN.json: app.hello: placeholders differ from English (missing {{name}}, unknown {{nam}})',
    'locales/zh-CN.json: app.extra: not in en.json (remove it)',
    'locales/zh-CN.json: app.items_one: zh-CN has no "one" plural form',
    'locales/zh-CN.json: app.items_other: empty text (remove the key to show English instead)',
  ]);
  assert.match(run({ ...base, 'locales/zh-CN.json': '{ nope' })[0], /locales\/zh-CN.json: does not parse/);
});

test('languages with more plural forms must translate every form they use', () => {
  const tree = { ...base, 'src/i18n.ts': 'tag: "en" tag: "ru"', 'locales/ru.json': JSON.stringify({ app: { items_one: '{{count}} штука' } }) };
  assert.deepEqual(run(tree), ['locales/ru.json: app.items: missing plural forms: few, many, other']);
});

test('every catalog is registered and every registered language has a catalog', () => {
  assert.deepEqual(run({ ...base, 'locales/ja.json': '{}', 'locales/zh-CN.json': '{}' }), [
    'src/i18n.ts: ja.json is not listed here',
  ]);
  assert.deepEqual(run(base), ['src/i18n.ts: lists zh-CN but locales/zh-CN.json does not exist']);
});

test('desktop keys are read from the Rust text functions', () => {
  const problems = run({ ...base, 'locales/zh-CN.json': '{}', 'rs/main.rs': 'text("trayOpen"); text_with("missing", &[("error", e)]); text_in(language, "alsoMissing");' });
  assert.deepEqual(problems, [
    'rs/main.rs: desktopShell.missing: used by the desktop app but missing from en.json',
    'rs/main.rs: desktopShell.alsoMissing: used by the desktop app but missing from en.json',
  ]);
});

test('helpers', () => {
  assert.deepEqual(flatten({ a: { b: 'x', c: { d: 'y' } } }), { 'a.b': 'x', 'a.c.d': 'y' });
  assert.equal(baseKey('a.items_other'), 'a.items');
  assert.equal(baseKey('a.other'), 'a.other');
  assert.deepEqual(placeholders('{{b}} and {{ a }} and {{b}}'), ['a', 'b']);
  assert.deepEqual(firstArguments('t(x ? "a.b" : "a.c", { n: f(1, 2) }); at("no"); t("d.e")', 't'), ['x ? "a.b" : "a.c"', '"d.e"']);
  assert.deepEqual([...keyLiterals('t("a.b"); event.kind === "media.ready"; t(c ? "x.y" : "x.z")')], ['a.b', 'x.y', 'x.z']);
  assert.deepEqual([...desktopKeyLiterals('action_failed(&id, &p, "replyNotSent", &e); label("previewPhoto")')].sort(), ['previewPhoto', 'replyNotSent']);
});
