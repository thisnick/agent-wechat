import test from 'node:test';
import assert from 'node:assert/strict';
import { verifyCiPublisher } from './verify-ci-publisher.mjs';

const env = { npm_config_npm_path: '/pinned/bin/npm',
  ACTIONS_ID_TOKEN_REQUEST_URL: 'private-url', ACTIONS_ID_TOKEN_REQUEST_TOKEN: 'private-token' };
const version = { status: 0, stdout: '11.12.1\n' };

test('requires an explicit absolute publisher path before executing anything', async () => {
  for (const npmPath of [undefined, 'npm']) {
    await assert.rejects(verifyCiPublisher({ env: { npm_config_npm_path: npmPath },
      spawn: () => { throw Error('must not execute'); } }), /absolute/);
  }
});
test('rejects a missing or old pinned npm', async () => {
  for (const result of [{ status: null }, { status: 0, stdout: '10.9.9\n' }]) {
    await assert.rejects(verifyCiPublisher({ env, spawn: () => result }), /pinned publisher/);
  }
});
test('checks pnpm publication without sending a package or requesting an OIDC token', async () => {
  let calls = 0;
  assert.equal(await verifyCiPublisher({ env, spawn: (command, args, options) => {
    calls++;
    if (command === env.npm_config_npm_path) return version;
    assert.equal(command, 'pnpm');
    for (const flag of ['publish', '--dry-run', '--no-git-checks', '--ignore-scripts']) {
      assert.ok(args.includes(flag));
    }
    assert.equal(options.env.npm_config_npm_path, env.npm_config_npm_path);
    assert.equal(options.env.ACTIONS_ID_TOKEN_REQUEST_URL, undefined);
    assert.equal(options.env.ACTIONS_ID_TOKEN_REQUEST_TOKEN, undefined);
    assert.equal(options.timeout, 30_000);
    return { status: 0, stderr: 'npm info using npm@11.12.1\n' };
  } }), '11.12.1');
  assert.equal(calls, 2);
});
test('shell npm 11 cannot hide pnpm selecting npm 10 or an unverifiable publisher', async () => {
  for (const result of [{ status: 0, stderr: 'npm info using npm@10.9.9\n' },
    { status: 0, stdout: '{}' }, { status: 1, stderr: 'npm info using npm@11.12.1\n' }]) {
    await assert.rejects(verifyCiPublisher({ env,
      spawn: command => command === env.npm_config_npm_path ? version : result }), /dry-run publisher/);
  }
});
