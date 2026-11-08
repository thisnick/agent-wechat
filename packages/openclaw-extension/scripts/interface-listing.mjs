#!/usr/bin/env node
// Writes interfaces/openclaw/{tools,config}.txt from the built plugin and its manifest.
// Usage: node scripts/interface-listing.mjs [--check]   (run `pnpm build` first)
import { readFile, writeFile, mkdir } from 'node:fs/promises';
import { dirname, join } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

const packageDir = join(dirname(fileURLToPath(import.meta.url)), '..');
const repoDir = join(packageDir, '../..');
const outputDir = join(repoDir, 'interfaces/openclaw');
const missing = 'MISSING DESCRIPTION';

const oneLine = text => String(text ?? '').split(/\s+/).filter(Boolean).join(' ');
const describe = text => oneLine(text) || missing;
const row = (item, attributes, doc) => `${item} | ${attributes.length ? attributes.join(', ') : '-'} | ${doc}`;
const literal = value => (typeof value === 'string' ? value : JSON.stringify(value));

/** Flattens a JSON schema into one row per property path. */
export function schemaRows(prefix, schema, { required = false, secrets = new Set(), path = '' } = {}) {
  const rows = [];
  const attributes = [];
  const branches = schema.anyOf ?? schema.oneOf ?? [];
  const types = [schema.type, ...branches.map(branch => branch.type)].flat().filter(Boolean);
  if (types.length) attributes.push([...new Set(types)].join('|'));
  if (required) attributes.push('required');
  if (schema.enum) attributes.push(`values ${schema.enum.map(literal).join('|')}`);
  if ('const' in schema) attributes.push(`const ${literal(schema.const)}`);
  for (const [key, label] of [['minimum', 'min'], ['maximum', 'max'], ['minLength', 'min length'], ['maxLength', 'max length'],
    ['minItems', 'min items'], ['maxItems', 'max items'], ['pattern', 'pattern'], ['format', 'format']]) {
    if (key in schema) attributes.push(`${label} ${literal(schema[key])}`);
  }
  if (schema.uniqueItems) attributes.push('unique items');
  if ('default' in schema) attributes.push(`default ${literal(schema.default)}`);
  if (secrets.has(path)) attributes.push('secret');
  if (prefix) rows.push(row(prefix, attributes, describe(schema.description)));
  const objects = [schema, ...branches];
  for (const object of objects) {
    const requiredKeys = new Set(object.required ?? []);
    for (const [key, child] of Object.entries(object.properties ?? {})) {
      const childPath = path ? `${path}.${key}` : key;
      rows.push(...schemaRows(prefix ? `${prefix}.${key}` : key, child, { required: requiredKeys.has(key), secrets, path: childPath }));
    }
    if (object.additionalProperties && typeof object.additionalProperties === 'object') {
      rows.push(...schemaRows(`${prefix}.*`, object.additionalProperties, { secrets, path: `${path}.*` }));
    }
    if (object.items && typeof object.items === 'object' && (object.items.properties || object.items.anyOf || object.items.oneOf)) {
      rows.push(...schemaRows(`${prefix}[]`, object.items, { secrets, path: `${path}[]` }));
    }
  }
  return rows;
}

/** A minimal context under which every tool factory is expected to produce its tools. */
function syntheticSetup() {
  const voice = { enabled: true, agentId: 'main', realtime: { provider: 'openai' }, allowConsultSessionPersistence: true };
  const config = { channels: { 'agent-wechat': {
    dmPolicy: 'allowlist', allowFrom: ['u1'], groupPolicy: 'allowlist', groupAllowFrom: ['u1'],
    native: { mode: 'external', endpoint: 'http://127.0.0.1:6174', accountId: 'a', token: 'synthetic-native-token-'.repeat(3), consumerStateDir: '/private/fixture', gatewayFacade: { enabled: true } },
    calls: { voice },
  } } };
  const context = { config, getRuntimeConfig: () => config, messageChannel: 'agent-wechat', nativeChannelId: 'room@chatroom',
    requesterSenderId: 'u1', senderIsOwner: true, agentId: 'main', sessionKey: 'agent:main:wechat:direct:u1', sessionId: 's', assertInvocationCurrent() {} };
  return { config, context };
}

async function collect() {
  const manifest = JSON.parse(await readFile(join(packageDir, 'openclaw.plugin.json'), 'utf8'));
  const { default: plugin } = await import(pathToFileURL(join(packageDir, 'dist/index.js')).href);
  const { config, context } = syntheticSetup();
  const calls = [];
  const api = new Proxy({ config, runtime: {}, logger: { info() {}, warn() {}, error() {}, debug() {} } }, {
    get(target, key) {
      if (key in target) return target[key];
      return (...args) => { calls.push([key, args]); return () => {}; };
    },
  });
  plugin.register(api);
  const tools = [];
  for (const [, [factory, options = {}]] of calls.filter(([name]) => name === 'registerTool')) {
    const created = typeof factory?.create === 'function' ? factory.create(context)
      : typeof factory === 'function' ? factory(context) : [factory];
    const list = [created].flat().filter(Boolean);
    const declared = options.names ?? (options.name ? [options.name] : []);
    if (!list.length) throw new Error(`Tool factory for ${declared.join(', ') || 'an unnamed tool'} produced no tools under the listing context; update syntheticSetup()`);
    for (const tool of list) {
      if (declared.length && !declared.includes(tool.name)) throw new Error(`Tool ${tool.name} is not in its registration names (${declared.join(', ')})`);
      tools.push({ tool, optional: Boolean(options.optional) });
    }
  }
  const routes = calls.filter(([name]) => name === 'registerHttpRoute').map(([, [route]]) => route);
  const policies = calls.filter(([name]) => name === 'registerTrustedToolPolicy').map(([, [policy]]) => policy);
  return { manifest, tools, routes, policies };
}

export async function render() {
  const { manifest, tools, routes, policies } = await collect();
  const problems = [];
  const names = tools.map(({ tool }) => tool.name);
  const declared = manifest.contracts?.tools ?? [];
  for (const name of names) if (!declared.includes(name)) problems.push(`${name} is registered but missing from openclaw.plugin.json contracts.tools`);
  for (const name of declared) if (!names.includes(name)) problems.push(`${name} is in openclaw.plugin.json contracts.tools but no factory registers it`);
  for (const { tool, optional } of tools) {
    const metadata = Boolean(manifest.toolMetadata?.[tool.name]?.optional);
    if (metadata !== optional) problems.push(`${tool.name}: registration optional=${optional} but toolMetadata optional=${metadata}`);
  }
  const docs = await readFile(join(repoDir, 'docs/src/content/docs/openclaw/tools.mdx'), 'utf8');
  for (const name of names) if (!docs.includes(name)) problems.push(`${name} is not described in docs/src/content/docs/openclaw/tools.mdx`);

  const toolRows = [];
  for (const { tool, optional } of tools) {
    const attributes = [optional ? 'optional' : 'enabled by default'];
    if (tool.catalogMode) attributes.push(`catalog ${tool.catalogMode}`);
    toolRows.push(row(`tool ${tool.name}`, attributes, describe(tool.description)));
    toolRows.push(...schemaRows(`tool ${tool.name}`, tool.parameters ?? {}).slice(1));
  }
  for (const policy of policies) toolRows.push(row(`policy ${policy.id}`, ['trusted tool policy'], describe(policy.description)));
  for (const route of routes) {
    const attributes = [route.match && `match ${route.match}`, route.auth && `auth ${route.auth}`,
      route.gatewayRuntimeScopeSurface && `scope ${route.gatewayRuntimeScopeSurface}`].filter(Boolean);
    toolRows.push(row(`http ${route.path}`, attributes, '-'));
  }

  const secrets = new Set((manifest.configContracts?.secretInputs?.paths ?? []).map(entry => entry.path));
  const configRows = [...schemaRows('plugin', manifest.configSchema ?? {}, { secrets }).slice(1)];
  for (const [id, channel] of Object.entries(manifest.channelConfigs ?? {})) {
    configRows.push(row(`channel ${id}`, ['channel'], describe(channel.description)));
    configRows.push(...schemaRows(`channel ${id}`, channel.schema ?? {}).slice(1));
  }
  for (const id of manifest.legacyPluginIds ?? []) configRows.push(row(`legacy-id ${id}`, ['legacy plugin id'], `Migrates to ${manifest.id}`));

  const header = kind => `# OpenClaw plugin ${kind}. Generated by packages/openclaw-extension/scripts/interface-listing.mjs; do not edit.\n# Run \`pnpm interfaces\` after changing them. Format: ITEM | ATTRIBUTES | DESCRIPTION\n`;
  const file = (kind, rows) => `${header(kind)}${[...rows].sort().join('\n')}\n`;
  return { problems, files: { 'tools.txt': file('tools', toolRows), 'config.txt': file('configuration', configRows) } };
}

if (import.meta.url === pathToFileURL(process.argv[1]).href) {
  const check = process.argv.includes('--check');
  const { problems, files } = await render();
  if (problems.length) {
    console.error(`OpenClaw interface problems:\n${problems.map(problem => `- ${problem}`).join('\n')}`);
    process.exitCode = 1;
  }
  let stale = false;
  await mkdir(outputDir, { recursive: true });
  for (const [name, content] of Object.entries(files)) {
    const path = join(outputDir, name);
    const current = await readFile(path, 'utf8').catch(() => '');
    if (current.replaceAll('\r\n', '\n') === content) continue;
    if (check) { stale = true; console.error(`interfaces/openclaw/${name} does not match the plugin. Run \`pnpm interfaces\`.`); }
    else await writeFile(path, content);
  }
  if (stale) process.exitCode = 1;
}
