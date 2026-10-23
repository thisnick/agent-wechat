import assert from 'node:assert/strict';
import { readFile, readdir, realpath, writeFile } from 'node:fs/promises';
import { join } from 'node:path';
import { pathToFileURL } from 'node:url';

// Run in a child with private HOME/state. The actual pinned message builder and
// queue execute; the provider callback records options and performs no I/O.
const [extension, root] = process.argv.slice(2);
assert.equal(process.env.HOME, join(root, 'home')); assert.equal(process.env.OPENCLAW_STATE_DIR, join(root, 'state'));
let networkCalls = 0;
globalThis.fetch = async () => { networkCalls++; throw Error('Network forbidden in synthetic option check'); };
const pkg = await realpath(join(extension, 'node_modules/openclaw'));
assert.equal(JSON.parse(await readFile(join(pkg, 'package.json'), 'utf8')).version, '2026.10.2');
const files = await readdir(join(pkg, 'dist'));
async function runtime(prefix, functionName) {
  for (const file of files.filter(file => file.startsWith(prefix) && file.endsWith('.mjs'))) {
    const module = await import(pathToFileURL(join(pkg, 'dist', file)));
    const fn = Object.values(module).find(value => typeof value === 'function' && value.name === functionName);
    if (fn) return fn;
  }
  throw Error('Pinned outbound runtime function unavailable');
}
const { wechatPlugin } = await import(pathToFileURL(join(extension, 'src/channel.ts')));
const { createWeChatMediaHandlers } = await import(pathToFileURL(join(extension, 'src/outbound-options.ts')));
const { createChannelMessageAdapterFromOutbound } = await import(pathToFileURL(join(pkg, 'dist/plugin-sdk/channel-outbound.js')));
const createRegistry = await runtime('registry-empty-', 'createEmptyPluginRegistry');
const runMessageAction = await runtime('message-action-runner-', 'runMessageAction');
const setActiveRegistry = await runtime('runtime-', 'setActivePluginRegistry');
const resolveSchema = await runtime('message-action-discovery-', 'resolveChannelMessageToolSchemaProperties');
const calls = []; let preparations = 0;
const handlers = createWeChatMediaHandlers(async (...args) => {
  calls.push(args[6]); await args[7]?.(); return { messageId: 'synthetic-message', meta: { outcome: 'sent' } };
});
const plugin = { ...wechatPlugin, actions: { ...wechatPlugin.actions, prepareSendPayload: args => {
  preparations++; return wechatPlugin.actions.prepareSendPayload(args);
} }, outbound: { ...wechatPlugin.outbound, ...handlers },
  message: createChannelMessageAdapterFromOutbound({ id: 'agent-wechat', outbound: handlers }) };
const registry = createRegistry(); registry.channels.push({ plugin, pluginId: 'agent-wechat', source: 'synthetic-option-check' });
setActiveRegistry(registry, 'synthetic-option-check', 'default', join(root, 'workspace'));
const fixture = join(root, 'workspace/source.mp4');
await writeFile(fixture, Buffer.from('000000186674797069736f6d0000000069736f6d6d703432', 'hex'), { mode: 0o600 });
const cfg = { channels: { 'agent-wechat': { enabled: true, backend: 'native', native: { mode: 'external', accountId: 'synthetic',
  endpoint: 'http://127.0.0.1:6174', token: 'synthetic-token-'.repeat(3), consumerStateDir: join(root, 'consumer') } } },
  agents: { defaults: { workspace: join(root, 'workspace') } } };
const schema = resolveSchema({ cfg, channel: 'agent-wechat' });
assert.deepEqual(schema.channelData.properties['agent-wechat'].properties.videoQuality.enum, ['normal', 'original']);
const checks = [];
for (const [name, forceCoreDelivery, requireQueuePersistence, params, expected] of [
  ['prepared-normal', false, false, { channelData: { 'agent-wechat': { videoQuality: 'normal' } } }, { videoQuality: 'normal', forceDocument: false }],
  ['prepared-original', false, false, { channelData: { 'agent-wechat': { videoQuality: 'original' } } }, { videoQuality: 'original', forceDocument: false }],
  ['forced-core-original', true, false, { channelData: { 'agent-wechat': { videoQuality: 'original' } } }, { videoQuality: 'original', forceDocument: false }],
  ['document-alias', true, false, { asDocument: true }, { forceDocument: true }],
  ['queued-original', true, true, { channelData: { 'agent-wechat': { videoQuality: 'original' } } }, { videoQuality: 'original', forceDocument: false }],
  ['queued-document-alias', true, true, { asDocument: true }, { forceDocument: true }],
]) {
  const before = calls.length, beforePreparation = preparations;
  await runMessageAction({ cfg, action: 'send', params: { channel: 'agent-wechat', to: 'wxid_synthetic_target', media: fixture, ...params },
    forceCoreDelivery, requireQueuePersistence, agentId: 'main', senderIsOwner: true, mediaAccess: { localRoots: [join(root, 'workspace')] } });
  assert.equal(calls.length, before + 1); assert.deepEqual(calls.at(-1), expected);
  assert.equal(preparations - beforePreparation, forceCoreDelivery ? 0 : 1);
  checks.push(name);
}
const before = calls.length;
await assert.rejects(runMessageAction({ cfg, action: 'send', forceCoreDelivery: true, requireQueuePersistence: true, agentId: 'main', senderIsOwner: true,
  params: { channel: 'agent-wechat', to: 'wxid_synthetic_target', media: fixture, asDocument: true, channelData: { 'agent-wechat': { videoQuality: 'original' } } },
  mediaAccess: { localRoots: [join(root, 'workspace')] } }), /cannot also request/);
assert.equal(calls.length, before); assert.equal(networkCalls, 0);
await writeFile(join(root, 'result.json'), JSON.stringify({ runtime: '2026.10.2', checks, schemaIncludesQuality: true, queuedConflictRejected: true, networkCalls, providerSends: 0 }) + '\n', { mode: 0o600 });
