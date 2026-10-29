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
assert.equal(JSON.parse(await readFile(join(pkg, 'package.json'), 'utf8')).version, '2026.10.7');
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
const createTools = await runtime('openclaw-tools-', 'createOpenClawTools');
const calls = [], handoffs = []; let preparations = 0;
const handlers = createWeChatMediaHandlers(async (...args) => {
  calls.push(args[6]); handoffs.push({ to: args[1], text: args[2], key: args[5], options: args[6] });
  await args[7]?.(); return { messageId: 'synthetic-message', meta: { outcome: 'sent' } };
});
const plugin = { ...wechatPlugin, actions: { ...wechatPlugin.actions, prepareSendPayload: args => {
  preparations++; return wechatPlugin.actions.prepareSendPayload(args);
} }, outbound: { ...wechatPlugin.outbound, ...handlers },
  message: { ...createChannelMessageAdapterFromOutbound({ id: 'agent-wechat', outbound: handlers }), durableFinal: wechatPlugin.message.durableFinal } };
const registry = createRegistry(); registry.channels.push({ plugin, pluginId: 'agent-wechat', source: 'synthetic-option-check' });
setActiveRegistry(registry, 'synthetic-option-check', 'default', join(root, 'workspace'));
const fixture = join(root, 'workspace/source.mp4');
await writeFile(fixture, Buffer.from('000000186674797069736f6d0000000069736f6d6d703432', 'hex'), { mode: 0o600 });
const cfg = { channels: { 'agent-wechat': { enabled: true, backend: 'native', native: { mode: 'external', accountId: 'synthetic',
  endpoint: 'http://127.0.0.1:6174', token: 'synthetic-token-'.repeat(3), consumerStateDir: join(root, 'consumer') } } },
  agents: { defaults: { workspace: join(root, 'workspace') } } };
const schema = resolveSchema({ cfg, channel: 'agent-wechat' });
assert.deepEqual(schema.channelData.properties['agent-wechat'].properties.videoQuality.enum, ['normal', 'original']);
assert.equal(schema.channelData.properties['agent-wechat'].properties.mentions.properties.userIds.uniqueItems, true);
const tools = createTools({ config: cfg, workspaceDir: join(root, 'workspace'), agentChannel: 'agent-wechat', currentChannelId: 'g@chatroom',
  requesterSenderId: 'wxid_requester', senderIsOwner: true, computerTransport: null, modelHasVision: false, wrapBeforeToolCallHook: false });
const messageTool = tools.find(tool => tool.name === 'message');
assert.ok(messageTool, 'the host tools list must include its built-in message tool');
assert.ok(JSON.stringify(messageTool.parameters).includes('mentions'), 'the actual model-facing message schema must expose WeChat mentions');
assert.ok(JSON.stringify(messageTool.parameters).includes('wechat_group_info'));
assert.ok(JSON.stringify(messageTool.parameters).includes('replyTo'));
assert.match(schema.replyTo.description, /same chat/);
const manifest = JSON.parse(await readFile(join(extension, 'openclaw.plugin.json'), 'utf8'));
assert.ok(manifest.contracts.tools.includes('wechat_send_share'), 'plugin-owned send-share tool stays discoverable');
assert.ok(!manifest.contracts.tools.includes('message'), 'the host built-in is not declared as a duplicate plugin tool');
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
const mentionChecks = [];
for (const [name, forceCoreDelivery, requireQueuePersistence, mentions, replyTo] of [
  ['prepared-members', false, false, { userIds: ['wxid_a', 'wxid_b'] }],
  ['prepared-all', false, false, { all: true }],
  ['forced-core-members', true, false, { userIds: ['wxid_a'] }],
  ['queued-members', true, true, { userIds: ['wxid_b'] }],
  ['queued-all', true, true, { all: true }],
  ['prepared-quoted-members', false, false, { userIds: ['wxid_a'] }, 'msg_source'],
  ['forced-core-quoted-all', true, false, { all: true }, 'msg_source'],
  ['queued-quoted-members', true, true, { userIds: ['wxid_b'] }, 'msg_source'],
]) {
  const before = calls.length, beforePreparation = preparations;
  await runMessageAction({ cfg, action: 'send', params: { channel: 'agent-wechat', to: 'g@chatroom', message: 'please check', ...(replyTo && { replyTo }), channelData: { 'agent-wechat': { mentions } } },
    forceCoreDelivery, requireQueuePersistence, agentId: 'main', senderIsOwner: true });
  assert.equal(calls.length, before + 1); assert.deepEqual(calls.at(-1), { mentions, forceDocument: false, ...(replyTo && { replyToId: replyTo }) });
  assert.equal(handoffs.at(-1).to, 'g@chatroom'); assert.equal(handoffs.at(-1).text, 'please check');
  if (requireQueuePersistence) assert.match(handoffs.at(-1).key, /^[a-f0-9]{64}$/);
  assert.equal(preparations - beforePreparation, forceCoreDelivery ? 0 : 1);
  mentionChecks.push(name);
}
for (const [params, expected] of [
  [{ to: 'wxid_dm', mentions: { all: true } }, /group conversation/],
  [{ mentions: { userIds: ['wxid_a', 'wxid_a'] } }, /Invalid WeChat mentions/],
  [{ mentions: { all: true, userIds: ['wxid_a'] } }, /Invalid WeChat mentions/],
  [{ mentions: { userIds: ['A Name'] } }, /Invalid WeChat mentions/],
  [{ mentions: { all: true }, media: fixture }, /group text only/],
  [{ mentions: { all: true }, audioAsVoice: true }, /group text only/],
]) {
  const { mentions, ...extra } = params, before = calls.length;
  for (const forceCoreDelivery of [false, true]) {
    await assert.rejects(runMessageAction({ cfg, action: 'send', forceCoreDelivery, requireQueuePersistence: true, agentId: 'main', senderIsOwner: true,
      params: { channel: 'agent-wechat', to: 'g@chatroom', message: 'hello', channelData: { 'agent-wechat': { mentions } }, ...extra },
      mediaAccess: { localRoots: [join(root, 'workspace')] } }), expected);
    assert.equal(calls.length, before);
  }
}
assert.equal(networkCalls, 0);
const replyChecks = [];
for (const [name, forceCoreDelivery, requireQueuePersistence, to] of [
  ['prepared-group-reply', false, false, 'g@chatroom'],
  ['prepared-dm-reply', false, false, 'wxid_peer'],
  ['forced-core-reply', true, false, 'g@chatroom'],
  ['queued-reply', true, true, 'g@chatroom'],
]) {
  const before = calls.length;
  await runMessageAction({ cfg, action: 'send', params: { channel: 'agent-wechat', to, message: 'Agreed', replyTo: 'msg_source' },
    forceCoreDelivery, requireQueuePersistence, agentId: 'main', senderIsOwner: true });
  assert.equal(calls.length, before + 1);
  assert.deepEqual(calls.at(-1), { forceDocument: false, replyToId: 'msg_source' });
  assert.equal(handoffs.at(-1).text, 'Agreed'); assert.equal(handoffs.at(-1).to, to);
  if (requireQueuePersistence) assert.match(handoffs.at(-1).key, /^[a-f0-9]{64}$/);
  replyChecks.push(name);
}
for (const forceCoreDelivery of [false, true]) {
  const before = calls.length;
  await assert.rejects(runMessageAction({ cfg, action: 'send', forceCoreDelivery, requireQueuePersistence: true, agentId: 'main', senderIsOwner: true,
    params: { channel: 'agent-wechat', to: 'g@chatroom', message: 'Reply', replyTo: 'msg_source', media: fixture },
    mediaAccess: { localRoots: [join(root, 'workspace')] } }), /quoted replies support text only/);
  assert.equal(calls.length, before);
}
assert.equal(networkCalls, 0);
await writeFile(join(root, 'result.json'), JSON.stringify({ runtime: '2026.10.7', checks, mentionChecks, messageToolListed: true,
  replyChecks, schemaIncludesReplies: true, schemaIncludesQuality: true, schemaIncludesMentions: true, queuedConflictRejected: true, queuedMentionsRejected: true, networkCalls, providerSends: 0 }) + '\n', { mode: 0o600 });
