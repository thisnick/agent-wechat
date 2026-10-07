import test from 'node:test';
import assert from 'node:assert/strict';
import { verifyNpmReleases } from './verify-npm-release.mjs';

const packages = [{ name: '@agent-wechat/cli', version: '0.15.2' }];
test('registry verification requires the exact published name and version', async () => {
  await verifyNpmReleases(packages, { fetchImpl: async () => ({ ok: true, json: async () => packages[0] }) });
});
test('a false-success publisher cannot pass missing or mismatched registry metadata', async () => {
  for (const metadata of [null, {}, { ...packages[0], version: '0.15.1' }]) {
    await assert.rejects(verifyNpmReleases(packages, { attempts: 1,
      fetchImpl: async () => ({ ok: !!metadata, json: async () => metadata }) }), /publication not verified/);
  }
});
test('registry propagation is retried within an explicit bound', async () => {
  let requests = 0, delays = 0;
  await verifyNpmReleases(packages, { attempts: 3, delay: async () => { delays++; },
    fetchImpl: async () => ({ ok: ++requests === 3, json: async () => packages[0] }) });
  assert.equal(requests, 3); assert.equal(delays, 2);
});
test('invalid package lists fail before accessing the registry', async () => {
  for (const input of [[], null, [{ name: 'other-package', version: '0.15.2' }]]) {
    await assert.rejects(verifyNpmReleases(input, { fetchImpl: () => { throw Error('must not fetch'); } }), /Invalid/);
  }
});
