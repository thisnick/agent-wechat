// Regenerate the embedded extension from unpacked, pinned upstream packages.
// No package lifecycle scripts or downloads run here.
// Usage: node scripts/build-mini-weui.mjs WEUI_PACKAGE TEMPLATE_COMPILER_PACKAGE
import { readFile, writeFile, readdir, copyFile, chmod } from 'node:fs/promises';
import { createRequire } from 'node:module';
import { resolve, relative, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import assert from 'node:assert/strict';
const [weui, compiler] = process.argv.slice(2).map(value => resolve(value));
assert.ok(weui && compiler, 'Supply the unpacked WeUI and template compiler package directories');
for (const [directory, name, version] of [[weui, 'weui-miniprogram', '1.5.6'], [compiler, 'glass-easel-template-compiler', '1.2.1']]) {
  const metadata = JSON.parse(await readFile(join(directory, 'package.json')));
  assert.equal(metadata.name, name);
  assert.equal(metadata.version, version);
  assert.equal(metadata.license, 'MIT');
}
const require = createRequire(import.meta.url);
const { TmplGroup } = require(compiler);
const group = new TmplGroup();
const base = join(weui, 'miniprogram_dist');
const configs = {}, styles = {}, factories = [];
async function visit(directory) {
  for (const entry of (await readdir(directory, { withFileTypes: true })).sort((a,b) => a.name < b.name ? -1 : a.name > b.name ? 1 : 0)) {
    const path = join(directory, entry.name);
    if (entry.isDirectory()) { await visit(path); continue; }
    const name = relative(base, path).replaceAll('\\', '/');
    if (!/\.(js|json|wxml|wxss|wxs)$/.test(name)) continue;
    const id = 'weui-miniprogram/' + name;
    const source = await readFile(path, 'utf8');
    if (name.endsWith('.wxs')) group.addScript(id.slice(0, -4), source);
    if (name.endsWith('.wxml')) assert.deepEqual(group.addTmpl(id.slice(0, -5), source), []);
    if (name.endsWith('.js')) factories.push('[' + JSON.stringify(id) + ',function(require,module,exports){' + source + '\n}]');
    if (name.endsWith('.json')) configs[id.slice(0, -5)] = JSON.parse(source);
    if (name.endsWith('.wxss')) styles[id.slice(0, -5)] = source;
  }
}
await visit(base);
const output = '// Generated from MIT-licensed weui-miniprogram 1.5.6; see vendor/README.md.\n'
  + 'export const factories = new Map([' + factories.join(',\n') + ']);\n'
  + 'export const configs=' + JSON.stringify(configs) + ';\n'
  + 'export const styles=' + JSON.stringify(styles) + ';\n'
  + 'export const templates=' + group.getTmplGenObjectGroups() + ';\n';
const vendor = fileURLToPath(new URL('../packages/native-client-rust/crates/wechat-miniprogram/runner/vendor/', import.meta.url));
await writeFile(join(vendor, 'weui.js'), output);
await copyFile(join(weui, 'LICENSE'), join(vendor, 'WEUI-LICENSE'));
await chmod(join(vendor, 'WEUI-LICENSE'), 0o644);
