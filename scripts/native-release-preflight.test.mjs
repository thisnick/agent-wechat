import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtemp, writeFile, rm } from 'node:fs/promises';
import { join } from 'node:path';
import { tmpdir } from 'node:os';
import { validateNativeRelease } from './native-release-preflight.mjs';
const sourceCommit = 'a'.repeat(40);
async function fixture(t, handler) {
  const root = await mkdtemp(join(tmpdir(), 'native-release-preflight-'));
  t.after(() => rm(root, { recursive: true, force: true }));
  await writeFile(join(root, 'package.json'), JSON.stringify({ name: '@agent-wechat/cli', version: '0.17.0' }));
  return { root, version: '0.17.0', sourceCommit, token: 'fixture-github-token', fetchImpl: handler };
}
const missing = () => new Response(null, { status: 404 });
test('native release refuses an already published npm version before checking tags', async t => {
  let requests = 0;
  const options = await fixture(t, async url => { requests++; assert.ok(String(url).startsWith('https://registry.npmjs.org/')); return Response.json({ name: '@agent-wechat/cli', version: '0.17.0' }); });
  await assert.rejects(validateNativeRelease(options), /unpublished CLI version/); assert.equal(requests, 1);
});
test('native release only admits missing or exact-commit lightweight version tags', async t => {
  for (const sha of [null, sourceCommit, 'b'.repeat(40)]) {
    const options = await fixture(t, async (url, init) => {
      assert.equal(init.redirect, 'error');
      if (String(url).startsWith('https://registry.npmjs.org/')) { assert.equal(init.headers, undefined); return missing(); }
      assert.equal(init.headers.Authorization, 'Bearer fixture-github-token');
      return sha ? Response.json({ object: { type: 'commit', sha } }) : missing();
    });
    if (sha && sha !== sourceCommit) await assert.rejects(validateNativeRelease(options), /different source commit/);
    else assert.equal((await validateNativeRelease(options)).tagExists, sha !== null);
  }
});
test('annotated version tags must peel to the exact native source commit', async t => {
  for (const sha of [sourceCommit, 'b'.repeat(40)]) {
    const options = await fixture(t, async url => {
      if (String(url).startsWith('https://registry.npmjs.org/')) return missing();
      return Response.json({ object: String(url).includes('/git/ref/') ? { type: 'tag', sha: 'c'.repeat(40) } : { type: 'commit', sha } });
    });
    if (sha === sourceCommit) assert.equal((await validateNativeRelease(options)).tagExists, true);
    else await assert.rejects(validateNativeRelease(options), /different source commit/);
  }
});
test('release checks fail closed on registry/API errors and mismatched requested version', async t => {
  const options = await fixture(t, async () => new Response(null, { status: 503 }));
  await assert.rejects(validateNativeRelease(options), /HTTP 503/);
  await assert.rejects(validateNativeRelease({ ...options, version: '0.16.0' }), /Version differs/);
  options.fetchImpl = async url => String(url).startsWith('https://registry.npmjs.org/') ? missing() : new Response(null, { status: 403 });
  await assert.rejects(validateNativeRelease(options), /HTTP 403/);
});
