// Optional comparison against the upstream Exparser-based component engine.
// Supply MINI_COMPONENT_DEPS pointing to the reference packages' node_modules.
import assert from 'node:assert/strict';
import { createRequire } from 'node:module';
import { readFile } from 'node:fs/promises';
import { createServer } from 'node:http';
import { fileURLToPath } from 'node:url';
import { join, resolve } from 'node:path';
const dependencies = resolve(process.env.MINI_COMPONENT_DEPS ?? 'node_modules');
const referenceRequire = createRequire(join(dependencies, '__reference__.cjs'));
for (const [name, version] of [['j-component','1.4.10'],['miniprogram-exparser','2.29.1'],['miniprogram-computed','9.0.0'],['mobx-miniprogram','6.12.3'],['mobx-miniprogram-bindings','7.0.1']]) {
  assert.equal(referenceRequire(name + '/package.json').version, version);
}
const { build } = referenceRequire('esbuild');
const { outputFiles } = await build({
  stdin: { contents: await readFile(new URL('./fixtures/native-component-reference.cjs', import.meta.url), 'utf8'), resolveDir: dependencies },
  nodePaths: [dependencies], bundle: true, platform: 'browser', format: 'iife',
  define: { global: 'globalThis' }, write: false,
});
const require = createRequire(fileURLToPath(new URL('../packages/client-ui/package.json', import.meta.url)));
const { chromium } = process.env.PLAYWRIGHT_MODULE ? await import(process.env.PLAYWRIGHT_MODULE) : require('@playwright/test');
const server = createServer((request,response) => {
  response.setHeader('Content-Type',request.url === '/reference.js' ? 'text/javascript' : 'text/html');
  response.end(request.url === '/reference.js' ? outputFiles[0].text : '<!doctype html><main></main><script src="/reference.js"></script>');
});
await new Promise(resolve => server.listen(0,'127.0.0.1',resolve));
let browser;
try {
  browser = await chromium.launch({headless:true, executablePath:process.env.PLAYWRIGHT_CHROMIUM_EXECUTABLE_PATH});
  const page = await browser.newPage();
  const errors = [];
  page.on('pageerror',error=>errors.push(error.message));
  await page.goto('http://127.0.0.1:' + server.address().port);
  await page.waitForFunction(()=>globalThis.probe);
  assert.deepEqual(await page.evaluate(async()=>({initial:probe.component.data.sum,updated:await probe.update(),tap:await probe.tap(),mobx:await probe.mobx(),lifecycle:probe.lifecycle.slice()})),
    {initial:3,updated:6,tap:7,mobx:7,lifecycle:['created','attached']});
  assert.deepEqual(errors,[]);
  console.log('PASS: upstream Exparser computed updates, tap events, MobX bindings and lifecycle');
} finally {
  await browser?.close();
  await new Promise(resolve=>server.close(resolve));
}
