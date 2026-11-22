import assert from 'node:assert/strict';
import { describe, it } from 'node:test';

import { dshConfigShape, wechatToolCatalog } from './catalog.ts';
import { DEFAULT_GRANTS } from '../tool-context.ts';

describe('wechatToolCatalog', () => {
  it('lists all seventeen tools from synthetic deps without touching them', () => {
    const tools = wechatToolCatalog();
    assert.equal(tools.length, 17);
    for (const tool of tools) {
      assert.equal(typeof tool.name, 'string');
      assert.match(tool.description, /\S/);
      assert.equal(tool.parameters['type'], 'object');
    }
  });

  it('classifies visibility from the default grants', () => {
    const byName = new Map(wechatToolCatalog().map((tool) => [tool.name, tool.visibility]));
    assert.equal(byName.get('wechat_history'), 'enabled by default');
    assert.equal(byName.get('wechat_media'), 'enabled by default');
    assert.equal(byName.get('wechat_contacts'), 'owner DM only');
    assert.equal(byName.get('wechat_login'), 'owner DM only');
    assert.equal(byName.get('wechat_archive_history'), 'opt-in via tools.grants.archive');
    assert.equal(byName.get('wechat_call'), 'opt-in via calls config');
    // Every default-granted tool maps to a non-archive visibility bucket.
    for (const name of [...DEFAULT_GRANTS.currentChat, ...DEFAULT_GRANTS.owner]) {
      assert.notEqual(byName.get(name), 'opt-in via tools.grants.archive', name);
    }
  });

  it('describes the config surface with typed rows', () => {
    assert.ok(dshConfigShape.length >= 25);
    for (const row of dshConfigShape) {
      assert.match(row.path, /^[a-zA-Z.]+$/);
      assert.ok(row.attributes.length > 0);
      assert.match(row.description, /\S/);
    }
    const paths = new Set(dshConfigShape.map((row) => row.path));
    for (const key of ['dmPolicy', 'native.tokenCredentialRef', 'tools.ownerAllowFrom', 'readPolicy', 'calls.voice.toolPolicy']) {
      assert.equal(paths.has(key), true, key);
    }
  });
});
