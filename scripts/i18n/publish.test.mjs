import test from 'node:test';
import assert from 'node:assert/strict';
import { MACHINE_NOTICE, allowedPath, changedKeys, count, planWork, prBody, readAsCurrent, withNotice } from './publish.mjs';
import { checkUi } from './index.mjs';
import { frontmatter, hash } from './docs.mjs';

test('the bot may change only translation files of the languages it is translating', () => {
  const scope = { tags: ['zh-CN'], folders: ['zh-cn'] };
  assert.equal(allowedPath('packages/client-ui/locales/zh-CN.json', scope), true);
  assert.equal(allowedPath('docs/src/content/docs/zh-cn/setup/install.mdx', scope), true);
  assert.equal(allowedPath('packages/client-ui/locales/en.json', scope), false);
  assert.equal(allowedPath('packages/client-ui/locales/zh-CN.meta.json', scope), false);
  assert.equal(allowedPath('packages/client-ui/locales/ja.json', scope), false);
  assert.equal(allowedPath('docs/src/content/docs/setup/install.mdx', scope), false);
  assert.equal(allowedPath('docs/src/content/docs/zh-cn/../../../../.github/workflows/x.yml', scope), false);
  assert.equal(allowedPath('.github/workflows/i18n-translate.yml', scope), false);
  assert.equal(allowedPath('scripts/i18n/publish.mjs', scope), false);
});

test('the work list carries the English and current text for missing and stale items', () => {
  const result = {
    'zh-CN': {
      ui: { missing: ['app.items'], stale: ['app.hello'], unstamped: [], machine: [], translated: 1 },
      docs: { folder: 'zh-cn', missing: ['setup/run.mdx'], stale: [], unstamped: [], current: [], orphan: [] },
    },
    ja: { ui: { missing: [], stale: [], unstamped: [], machine: [], translated: 3 }, docs: null },
  };
  const en = { 'app.hello': 'Hello {{name}}', 'app.items_one': '{{count}} item', 'app.items_other': '{{count}} items' };
  const work = planWork(result, { en, catalogs: { 'zh-CN': { 'app.hello': '你好' } }, pages: { 'setup/run.mdx': '# Run' } });
  assert.deepEqual(Object.keys(work), ['zh-CN']);
  assert.equal(count(work), 3);
  assert.deepEqual(work['zh-CN'].ui, [
    { key: 'app.items', state: 'missing', english: { 'app.items_one': '{{count}} item', 'app.items_other': '{{count}} items' }, current: {} },
    { key: 'app.hello', state: 'stale', english: { 'app.hello': 'Hello {{name}}' }, current: { 'app.hello': '你好' } },
  ]);
  assert.equal(work['zh-CN'].docs[0].translation, 'docs/src/content/docs/zh-cn/setup/run.mdx');
});

test('machine-translated pages get a banner once; a reviewed page without one keeps none', () => {
  const page = '---\ntitle: 运行\n---\n\n正文';
  const marked = withNotice(page, '本页由机器翻译。');
  assert.match(marked, /^---\ntitle: 运行\nbanner:\n {2}content: "本页由机器翻译。"\n---\n/);
  assert.equal(withNotice(marked, 'again'), marked);
  assert.equal(frontmatter(marked).fields.title, '运行');
});

test('the pull request body lists counts, dropped items and the back-translation', () => {
  const work = { 'zh-CN': { ui: [{}, {}], docs: [{}] } };
  const body = prBody({ work, report: { dropped: ['zh-CN: app.hello'], restored: ['README.md'] }, source: '12', backTranslation: '### zh-CN\n- `app.hello`: Hello' });
  assert.match(body, /Translations for the English changes in #12/);
  assert.match(body, /\| zh-CN \| 2 \| 1 \|/);
  assert.match(body, /- zh-CN: app.hello/);
  assert.match(body, /- `README.md`/);
  assert.match(body, /<summary>Back-translation/);
  assert.ok(MACHINE_NOTICE.length > 0);
});

test('a re-translated stale string is checked as current, so a broken one is caught', () => {
  const locales = 'packages/client-ui/locales';
  const en = { app: { hello: 'Hello {{who}}' } };
  const tree = {
    [`${locales}/en.json`]: JSON.stringify(en),
    // Claude's update dropped the placeholder; the meta file still records the old English.
    [`${locales}/zh-CN.json`]: JSON.stringify({ app: { hello: '你好' } }),
    [`${locales}/zh-CN.meta.json`]: JSON.stringify({ 'app.hello': { source: hash('Hello {{name}}') } }),
    'src/main.ts': 't("app.hello", { who })',
    'src/i18n.ts': 'tag: "en" tag: "zh-CN"',
  };
  const read = path => {
    if (!(path in tree)) throw new Error(`no file ${path}`);
    return tree[path];
  };
  const list = (dir, extensions) => Object.keys(tree).filter(path => path.startsWith(`${dir}/`) && extensions.some(extension => path.endsWith(extension)));
  const config = { locales, code: ['src'], desktopCode: [], registries: { 'src/i18n.ts': /tag: "([^"]+)"/g } };
  // Read as recorded, the key is stale and its placeholder is not checked.
  assert.deepEqual(checkUi({ read, list, config }), []);
  const work = { 'zh-CN': { ui: [{ key: 'app.hello', current: { 'app.hello': '你好 {{name}}' } }], docs: [] } };
  const problems = checkUi({ read: readAsCurrent(read, work, { 'app.hello': 'Hello {{who}}' }), list, config });
  assert.deepEqual(problems.map(problem => `${problem.key}: ${problem.message}`), ['app.hello: placeholders differ from English (missing {{who}})']);
});

test('only strings Claude changed are checked as current and recorded; untouched ones stay out of date', () => {
  const entry = { ui: [
    { key: 'app.hello', current: { 'app.hello': '你好' } },
    { key: 'app.items', current: { 'app.items_other': '{{count}} 项' } },
    { key: 'app.new', current: {} },
  ] };
  const catalog = { 'app.hello': '你好', 'app.items_other': '共 {{count}} 项', 'app.new': '新', 'app.other': '别的' };
  assert.deepEqual(changedKeys(catalog, entry), ['app.items_other', 'app.new']);
  // A stale key Claude left alone keeps its old record, so it stays exempt and is not dropped.
  const read = path => ({
    'packages/client-ui/locales/zh-CN.meta.json': JSON.stringify({ 'app.hello': { source: 'old' } }),
    'packages/client-ui/locales/zh-CN.json': JSON.stringify({ app: { hello: '你好', new: '新' } }),
  })[path] ?? (() => { throw new Error('no file'); })();
  const meta = JSON.parse(readAsCurrent(read, { 'zh-CN': { ui: entry.ui } }, { 'app.new': 'New' })('packages/client-ui/locales/zh-CN.meta.json'));
  assert.deepEqual(meta, { 'app.hello': { source: 'old' }, 'app.new': { source: hash('New') } });
});
