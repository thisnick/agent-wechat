#!/usr/bin/env node
// Compares the interface listings between a base commit and the working tree, classifies
// each change (added, removed, changed, breaking, missing docs) and renders the PR comment.
// Usage: node scripts/interfaces/review.mjs --base <ref> [--comment] [--fail-on-missing-docs]
import { execFileSync } from 'node:child_process';
import { appendFile, readFile, readdir } from 'node:fs/promises';
import { dirname, join, relative } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { upsertComment } from './github.mjs';

export const root = join(dirname(fileURLToPath(import.meta.url)), '../..');
export const MARKER = '<!-- agent-wechat-interfaces -->';
export const APPROVAL_LABEL = 'interfaces-approved';
const MISSING = /^MISSING (HELP|DESCRIPTION|DOC)$/;
const DIFF_LIMIT = 120;

/** Which interface a listing file describes, and the package whose changeset covers it. */
export function surface(file) {
  if (file === 'interfaces/cli/wx.txt') return { kind: 'cli', title: 'wx CLI', packages: ['@agent-wechat/cli'] };
  if (file.startsWith('interfaces/crates/')) return { kind: 'crate', title: `Rust crate ${file.slice('interfaces/crates/'.length, -4)}`, packages: ['@agent-wechat/rust-crates'] };
  if (file === 'interfaces/openclaw/tools.txt') return { kind: 'schema', title: 'OpenClaw tools', packages: ['@agent-wechat/agent-wechat'] };
  if (file === 'interfaces/openclaw/config.txt') return { kind: 'schema', title: 'OpenClaw configuration', packages: ['@agent-wechat/agent-wechat'] };
  if (file === 'interfaces/storage/account.txt') return { kind: 'sql', title: 'Account store schema', packages: ['@agent-wechat/cli'] };
  if (file === 'interfaces/storage/registry.txt') return { kind: 'sql', title: 'Registry schema', packages: ['@agent-wechat/cli'] };
  if (file === 'interfaces/http/routes.txt') return { kind: 'http', title: 'HTTP API', packages: ['@agent-wechat/cli'] };
  if (file === 'interfaces/http/calls-websocket.txt') return { kind: 'ws', title: 'Calls WebSocket', packages: ['@agent-wechat/cli'] };
  if (file === 'interfaces/ui/scenes.txt') return { kind: 'scenes', title: 'UI scenes', packages: ['@agent-wechat/desktop'] };
  if (file === 'interfaces/ui/routes.txt') return { kind: 'routes', title: 'UI routes', packages: ['@agent-wechat/desktop'] };
  return { kind: 'other', title: file, packages: [] };
}

/** Parses `ITEM | ATTRIBUTES | DOC` rows into a map keyed by ITEM. */
export function parseListing(text) {
  const rows = new Map();
  for (const line of text.split('\n')) {
    if (!line.trim() || line.startsWith('#')) continue;
    const first = line.indexOf(' | ');
    const second = line.indexOf(' | ', first + 3);
    if (first < 0 || second < 0) throw new Error(`Malformed listing row: ${line}`);
    rows.set(line.slice(0, first), { line, attributes: line.slice(first + 3, second), doc: line.slice(second + 3) });
  }
  return rows;
}

const tokens = attributes => (attributes === '-' ? [] : attributes.split(', '));
const token = (list, name) => list.find(item => item === name || item.startsWith(`${name} `));
const value = (list, name) => token(list, name)?.slice(name.length + 1);
const isMissing = doc => MISSING.test(doc);

function parentOf(kind, item) {
  if (kind === 'cli') return item.includes(' ') ? item.slice(0, item.lastIndexOf(' ')) : undefined;
  if (kind === 'schema') return item.includes('.') ? item.slice(0, item.lastIndexOf('.')) : undefined;
  // WebSocket fields follow the variant's closing bracket; dots inside the tag are part of its name.
  if (kind === 'ws') return item.lastIndexOf('.') > item.lastIndexOf(']') ? item.slice(0, item.lastIndexOf('.')) : undefined;
  return undefined;
}

function numeric(list, name) {
  const found = value(list, name);
  return found === undefined ? undefined : Number(found);
}

/** Reasons an attribute change can break existing callers (CLI and schema listings). */
export function attributeBreaks(kind, before, after) {
  const old = tokens(before);
  const now = tokens(after);
  const reasons = [];
  if (!token(old, 'required') && token(now, 'required')) reasons.push('now required');
  const oldValues = value(old, 'values')?.split('|');
  const newValues = value(now, 'values')?.split('|');
  if (newValues && (!oldValues || oldValues.some(v => !newValues.includes(v)))) {
    const removed = oldValues ? oldValues.filter(v => !newValues.includes(v)) : [];
    reasons.push(oldValues ? `allowed values removed: ${removed.join(', ')}` : 'now limited to fixed values');
  }
  if (value(old, 'default') !== value(now, 'default') && value(old, 'default') !== undefined) {
    reasons.push(`default changed from ${value(old, 'default')} to ${value(now, 'default') ?? 'none'}`);
  }
  if (value(old, 'const') !== value(now, 'const') && value(now, 'const') !== undefined) reasons.push('constant value changed');
  if (kind === 'cli') {
    for (const name of ['short', 'alias']) {
      for (const item of old.filter(t => t.startsWith(`${name} `))) if (!now.includes(item)) reasons.push(`${item} removed`);
    }
    const shape = list => (token(list, 'takes') ? 'takes a value' : token(list, 'switch') ? 'switch' : token(list, 'count') ? 'count' : 'none');
    if (shape(old) !== shape(now)) reasons.push(`changed from ${shape(old)} to ${shape(now)}`);
    if (token(old, 'global') && !token(now, 'global')) reasons.push('no longer global');
    if (token(old, 'repeatable') && !token(now, 'repeatable')) reasons.push('no longer repeatable');
    if (value(old, 'position') !== value(now, 'position')) reasons.push(`position changed from ${value(old, 'position')} to ${value(now, 'position')}`);
  }
  if (kind === 'schema') {
    const types = list => list[0] && /^[a-z|]+$/.test(list[0]) && !['required', 'optional', 'secret', 'channel'].includes(list[0]) ? list[0].split('|') : undefined;
    const oldTypes = types(old);
    const newTypes = types(now);
    if (oldTypes && newTypes && oldTypes.some(t => !newTypes.includes(t))) reasons.push(`type changed from ${oldTypes.join('|')} to ${newTypes.join('|')}`);
    if (old.includes('enabled by default') && now.includes('optional')) reasons.push('tool is no longer enabled by default');
    for (const [name, narrower] of [['min', (a, b) => b > a], ['min length', (a, b) => b > a], ['min items', (a, b) => b > a],
      ['max', (a, b) => b < a], ['max length', (a, b) => b < a], ['max items', (a, b) => b < a]]) {
      const a = numeric(old, name);
      const b = numeric(now, name);
      if (b !== undefined && (a === undefined ? true : narrower(a, b))) reasons.push(`${name} narrowed to ${b}`);
    }
    if (value(old, 'pattern') !== value(now, 'pattern') && value(now, 'pattern') !== undefined) reasons.push('pattern changed');
  }
  return reasons;
}

/** Reasons a change to the daemon's own SQL schema needs care (sql listings). */
export function sqlBreaks(item, before, after) {
  if (item === 'schema version') {
    return Number(after) > Number(before)
      ? [`schema version raised from ${before} to ${after}: older wx releases refuse a store at this version, so downgrading stops working`]
      : [`schema version changed from ${before} to ${after}`];
  }
  if (item.startsWith('migration ')) return ['existing migration edited: stores that already applied it never get the change'];
  if (!/^table [^ .]+\.[^ ]+$/.test(item)) return [];
  const old = before.split(', ');
  const now = after.split(', ');
  const reasons = [];
  if (old[0] !== now[0]) reasons.push(`column type changed from ${old[0]} to ${now[0]}`);
  if (!old.includes('not null') && now.includes('not null')) reasons.push('column is now NOT NULL');
  const value = (list, name) => list.find(t => t.startsWith(`${name} `));
  if (value(old, 'default') && value(old, 'default') !== value(now, 'default')) reasons.push(`column ${value(old, 'default')} changed to ${value(now, 'default') ?? 'no default'}`);
  if (value(old, 'primary key') !== value(now, 'primary key')) reasons.push('primary key changed');
  return reasons;
}

/** WebSocket rules depend on direction: the daemon accepts `client` messages and clients read `server` messages. */
export function wsChanges(item, before, after) {
  const old = tokens(before);
  const now = tokens(after);
  const reasons = [];
  const notes = [];
  if (item.startsWith('socket ')) {
    for (const name of ['path', 'scope']) if (value(old, name) !== value(now, name)) reasons.push(`socket ${name} changed`);
    if (value(old, 'protocol') !== value(now, 'protocol')) notes.push(`protocol version changed from ${value(old, 'protocol')} to ${value(now, 'protocol')}`);
    return { reasons, notes };
  }
  const attributeWord = /^(required|message|nullable|socket|sockets|rejects|when|min|max|format|values|default|pattern|unique)\b/;
  const typeOf = list => (list[0] && !attributeWord.test(list[0]) ? list[0] : undefined);
  const sockets = list => (value(list, 'socket') ?? value(list, 'sockets'))?.split('|') ?? [];
  const lostSockets = sockets(old).filter(name => sockets(now).length && !sockets(now).includes(name));
  if (typeOf(old) !== typeOf(now)) reasons.push(`type changed from ${typeOf(old) ?? 'none'} to ${typeOf(now) ?? 'none'}`);
  if (item.startsWith('client ')) {
    reasons.push(...attributeBreaks('schema', before, after).filter(reason => !reason.startsWith('type changed')));
    if (old.includes('nullable') && !now.includes('nullable')) reasons.push('null no longer accepted');
    if (!old.includes('rejects unknown fields') && now.includes('rejects unknown fields')) reasons.push('unknown fields now rejected');
    for (const name of lostSockets) reasons.push(`no longer accepted on the ${name} socket`);
    return { reasons: [...new Set(reasons)], notes };
  }
  if (token(old, 'required') && !token(now, 'required')) reasons.push('may now be absent');
  if (!old.includes('nullable') && now.includes('nullable')) reasons.push('may now be null');
  for (const name of lostSockets) reasons.push(`no longer sent on the ${name} socket`);
  const oldValues = value(old, 'values')?.split('|');
  const added = value(now, 'values')?.split('|').filter(v => oldValues && !oldValues.includes(v)) ?? [];
  if (added.length) notes.push(`new values ${added.join(', ')}: clients must handle them`);
  for (const name of sockets(now).filter(name => sockets(old).length && !sockets(old).includes(name))) notes.push(`now also sent on the ${name} socket`);
  return { reasons, notes };
}

/** HTTP route rows: scope and payload types are part of the contract; `untyped` payloads become typed over time. */
export function httpRouteChanges(before, after) {
  const old = tokens(before);
  const now = tokens(after);
  const reasons = [];
  const notes = [];
  if (value(old, 'scope') !== value(now, 'scope')) reasons.push(`token scope changed from ${value(old, 'scope')} to ${value(now, 'scope')}`);
  for (const name of ['request', 'query', 'response']) {
    const a = value(old, name);
    const b = value(now, name);
    if (a === b) continue;
    if (a?.startsWith('untyped') && b && !b.startsWith('untyped')) notes.push(`${name} is now typed as ${b}`);
    else reasons.push(`${name} changed from ${a ?? 'none'} to ${b ?? 'none'}`);
  }
  const sdk = list => list.filter(t => t.startsWith('sdk ')).sort().join('; ');
  if (sdk(old) !== sdk(now)) notes.push(`SDK types changed from ${sdk(old) || 'none'} to ${sdk(now) || 'none'}`);
  return { reasons, notes };
}
// HTTP request fields behave like messages clients send, response fields like messages the server sends.
const asDirection = item => item.replace(/^request /, 'client ').replace(/^response /, 'server ');

const protocol = rows => [...(rows?.entries() ?? [])].filter(([item]) => item.startsWith('socket ')).map(([item, row]) => `${item}=${value(tokens(row.attributes), 'protocol')}`).sort().join(',');

/** Classifies the changes between two parsed listings of one interface. */
export function compareListing(kind, base, head) {
  const result = { added: [], removed: [], changed: [], breaking: [], missingDocs: [], gaps: [], notes: [], docChanges: 0 };
  for (const [item, row] of head) {
    const old = base?.get(item);
    if (!old) {
      result.added.push(item);
      // Gaps in a listing's first version are its starting debt; only later additions must be documented.
      if (base && isMissing(row.doc)) result.missingDocs.push(item);
      const parent = parentOf(kind, item);
      if (base && parent && base.has(parent) && tokens(row.attributes).includes('required') && !(kind === 'ws' && item.startsWith('server '))) {
        result.breaking.push({ item, reason: kind === 'cli' ? 'new required argument on an existing command' : 'new required field' });
      }
      if (base && kind === 'sql' && /^table [^ .]+\.[^ ]+$/.test(item)) {
        const table = item.slice(0, item.indexOf('.'));
        const parts = row.attributes.split(', ');
        if (base.has(table) && parts.includes('not null') && !parts.some(t => t.startsWith('default '))) result.breaking.push({ item, reason: 'new NOT NULL column without a default on an existing table' });
      }
      if (base && kind === 'http' && item.startsWith('route ') && ['request', 'query', 'response'].some(name => value(tokens(row.attributes), name)?.startsWith('untyped'))) {
        result.gaps.push({ item, reason: 'new route with an untyped payload; give it a Rust request or response type' });
      }
      if (base && kind === 'http' && /^request [^.]+\..+/.test(item) && tokens(row.attributes).includes('required') && base.has(item.slice(0, item.lastIndexOf('.')))) {
        result.breaking.push({ item, reason: 'new required request field' });
      }
      if (base && kind === 'ws') {
        const variant = item.match(/^(server [^[]+)\[[^\]]+\]$/);
        if (variant && [...base.keys()].some(key => key.startsWith(`${variant[1]}[`))) result.notes.push({ item, reason: 'new server message type: clients must ignore types they do not know' });
      }
      if (base && kind === 'crate') {
        const [what, path] = item.split(' ');
        const owner = path?.slice(0, path.lastIndexOf('::'));
        const ownerRow = owner && [...base.entries()].find(([key]) => key.endsWith(` ${owner}`) && /^(struct|enum|union|trait) /.test(key));
        if (ownerRow) {
          const [ownerKey, ownerData] = ownerRow;
          const exhaustive = !ownerData.attributes.includes('#[non_exhaustive]');
          if (what === 'field' && exhaustive && ownerData.attributes.includes('{ public fields }')) result.breaking.push({ item, reason: 'new field on a struct that callers can construct' });
          if (what === 'variant' && exhaustive && ownerKey.startsWith('enum ')) result.breaking.push({ item, reason: 'new variant on an exhaustive enum' });
          if (what === 'fn' && ownerKey.startsWith('trait ') && row.attributes.startsWith('required ')) result.breaking.push({ item, reason: 'new required trait method' });
        }
      }
      continue;
    }
    if (old.line === row.line) continue;
    const reasons = [];
    if (old.attributes !== row.attributes) {
      if (kind === 'crate') reasons.push('signature changed');
      else if (kind === 'sql') reasons.push(...sqlBreaks(item, old.attributes, row.attributes));
      else if (kind === 'ws' || kind === 'http') {
        const change = kind === 'http' && item.startsWith('route ') ? httpRouteChanges(old.attributes, row.attributes) : wsChanges(kind === 'http' ? asDirection(item) : item, old.attributes, row.attributes);
        reasons.push(...change.reasons);
        result.notes.push(...change.notes.map(reason => ({ item, reason })));
      }
      else reasons.push(...attributeBreaks(kind, old.attributes, row.attributes));
    }
    if (old.doc !== row.doc) result.docChanges += 1;
    if (isMissing(row.doc) && !isMissing(old.doc)) result.missingDocs.push(item);
    result.changed.push(item);
    for (const reason of reasons) result.breaking.push({ item, reason });
  }
  for (const item of base?.keys() ?? []) {
    if (head.has(item)) continue;
    result.removed.push(item);
    // Scenes are review coverage, not an interface callers use: removing one is a warning.
    if (kind === 'scenes') {
      result.notes.push({ item, reason: 'scene removed: Argos no longer captures this screen' });
      continue;
    }
    const row = base.get(item);
    // Removing a hidden alias still breaks scripts that use it, so every removal counts.
    result.breaking.push({ item, reason: tokens(row.attributes).some(t => t.endsWith('alias')) ? 'alias removed'
      : kind === 'sql' && item.startsWith('migration ') ? 'existing migration removed'
      : kind === 'ws' && item.startsWith('client ') ? 'removed from messages clients send: older clients that send it are rejected'
      : kind === 'ws' && item.startsWith('server ') ? 'removed from server messages: clients that read it break'
      : kind === 'routes' ? 'route removed: saved links and notifications that use it stop working'
      : kind === 'http' && item.startsWith('route ') ? 'route removed: callers get 404'
      : kind === 'http' && item.startsWith('request ') ? 'removed from requests the daemon accepts'
      : kind === 'http' && item.startsWith('response ') ? 'removed from responses: callers that read it break' : 'removed' });
  }
  if (kind === 'ws' && base && result.breaking.length && protocol(base) === protocol(head)) {
    result.breaking.push({ item: 'protocol version', reason: 'breaking change without a call protocol version bump' });
  }
  if (kind === 'sql' && base) {
    const schemaChanged = [...result.added, ...result.removed, ...result.changed].some(item => item !== 'schema version' && !item.startsWith('migration '));
    if (schemaChanged && base.get('schema version')?.attributes === head.get('schema version')?.attributes) {
      result.breaking.push({ item: 'schema version', reason: 'schema changed without a new schema version: existing stores keep the old schema' });
    }
  }
  return result;
}

/** Reads the bump level each changeset in the PR gives each package. */
export function parseChangesets(texts) {
  const order = { patch: 1, minor: 2, major: 3 };
  const bumps = new Map();
  for (const text of texts) {
    const front = text.match(/^---\n([\s\S]*?)\n---/);
    for (const line of front?.[1].split('\n') ?? []) {
      const match = line.match(/^\s*["']?([^"':]+)["']?\s*:\s*(patch|minor|major)\s*$/);
      if (match && (order[match[2]] > (order[bumps.get(match[1])] ?? 0))) bumps.set(match[1], match[2]);
    }
  }
  return bumps;
}

const code = text => `\`${text.replaceAll('`', "'")}\``;

/** A one-paragraph overview of a listing, for the first (baseline) version of a file. */
export function overview(kind, head) {
  const items = [...head.keys()];
  if (kind === 'cli') {
    const commands = items.filter(item => !/ (--|-[a-zA-Z]$|<)/.test(item) && !head.get(item).attributes.includes('alias'));
    const groups = new Map();
    for (const command of commands) {
      const parts = command.split(' ');
      if (parts.length < 2) continue;
      groups.set(parts[1], (groups.get(parts[1]) ?? 0) + (parts.length > 2 ? 1 : 0));
    }
    const hidden = commands.filter(command => tokens(head.get(command).attributes).includes('hidden')).length;
    return `${commands.length} commands${hidden ? ` (${hidden} hidden)` : ''}, ${items.length - commands.length} arguments, flags and aliases. Top-level commands: ${[...groups].map(([name, n]) => (n ? `${name} (${n})` : name)).join(', ')}.`;
  }
  if (kind === 'crate') {
    const counts = new Map();
    for (const item of items) { const what = item.split(' ')[0]; counts.set(what, (counts.get(what) ?? 0) + 1); }
    return `Items by kind: ${[...counts].sort((a, b) => b[1] - a[1]).map(([what, n]) => `${what} ${n}`).join(', ')}.`;
  }
  if (kind === 'sql') {
    const tables = items.filter(item => /^table [^ .]+$/.test(item)).length;
    const columns = items.filter(item => /^table [^ .]+\.[^ ]+$/.test(item)).length;
    const indexes = items.filter(item => item.startsWith('index ')).length;
    const migrations = items.filter(item => item.startsWith('migration ')).length;
    return `Schema version ${head.get('schema version')?.attributes ?? '?'}: ${tables} tables, ${columns} columns, ${indexes} indexes, ${migrations} migration files.`;
  }
  if (kind === 'ws') {
    const sockets = items.filter(item => item.startsWith('socket ')).map(item => item.slice(7));
    const count = side => items.filter(item => new RegExp(`^${side} [^[]+\\[[^\\]]+\\]$`).test(item)).length;
    return `Sockets: ${sockets.join(', ')}. ${count('client')} message types clients send, ${count('server')} the daemon sends.`;
  }
  if (kind === 'http') {
    const routes = items.filter(item => item.startsWith('route '));
    const untyped = routes.filter(item => head.get(item).attributes.includes('untyped')).length;
    return `${routes.length} routes, ${untyped} with at least one untyped payload.`;
  }
  if (kind === 'routes') return `Routes: ${items.map(item => item.replace(/^route /, '')).join(', ')}.`;
  if (kind === 'schema') {
    const tools = items.filter(item => /^tool [^.]+$/.test(item));
    if (tools.length) return `Tools: ${tools.map(item => `${item.slice(5)}${tokens(head.get(item).attributes).includes('optional') ? ' (optional)' : ''}`).join(', ')}.`;
    const top = [...new Set(items.map(item => item.split('.')[0]))];
    return `Top-level entries: ${top.join(', ')}.`;
  }
  return '';
}

/** Renders the PR comment for a set of per-file comparisons. */
export function renderComment({ reports, changesets, baseline, openapi }) {
  const changed = reports.filter(r => r.baseline || r.result.added.length || r.result.removed.length || r.result.changed.length);
  const lines = [MARKER, '## Interface changes', ''];
  if (!changed.length) {
    lines.push('No interface listing changed in this PR. No approval is needed.');
    return lines.join('\n');
  }
  const sum = key => changed.reduce((n, r) => n + r.result[key].length, 0);
  const breaking = changed.flatMap(r => r.result.breaking.map(b => ({ ...b, title: r.surface.title })));
  // oasdiff is the standard OpenAPI compatibility check; its findings count as breaking changes of the HTTP API.
  if (openapi?.breaking) {
    breaking.push({ item: 'openapi.json', reason: 'oasdiff reports breaking changes (see OpenAPI compatibility)', title: 'HTTP API' });
    const http = changed.find(r => r.surface.kind === 'http');
    if (http && !http.result.breaking.length) http.result.breaking.push({ item: 'openapi.json', reason: 'oasdiff' });
  }
  const missing = changed.flatMap(r => [...r.result.missingDocs.map(item => ({ item, title: r.surface.title, reason: '' })),
    ...(r.result.gaps ?? []).map(gap => ({ ...gap, title: r.surface.title }))]);
  if (baseline) {
    lines.push(`**Needs your approval.** This PR adds the interface listings for the first time: ${sum('added')} items across ${changed.length} files.`,
      'There is no earlier listing to compare against, so nothing can be marked breaking. Review the listing files as the new baseline.', '');
  } else {
    lines.push(`**Needs your approval.** ${sum('added')} added, ${sum('changed')} changed, ${sum('removed')} removed across ${changed.length} listing${changed.length === 1 ? '' : 's'}.`,
      breaking.length ? `**${breaking.length} breaking change${breaking.length === 1 ? '' : 's'}.**` : 'No breaking changes detected.', '');
  }
  lines.push(`To approve, add the \`${APPROVAL_LABEL}\` label. A later push that changes the listings removes it.`, '');

  const warnings = [];
  for (const report of changed) for (const note of report.result.notes ?? []) warnings.push(`${report.surface.title}: ${code(note.item)} — ${note.reason}`);
  if (missing.length) warnings.push(`${missing.length} new item${missing.length === 1 ? ' has' : 's have'} no documentation or no type. The **Interface docs** check fails until each has help text, a doc comment, a description or a typed payload.`);
  if (!baseline) {
    for (const report of changed) {
      for (const pkg of report.surface.packages) {
        const bump = changesets.get(pkg);
        const fixedGroup = ['@agent-wechat/cli', '@agent-wechat/agent-wechat', '@agent-wechat/desktop'];
        const covered = bump ?? (fixedGroup.includes(pkg) ? fixedGroup.map(p => changesets.get(p)).find(Boolean) : undefined);
        if (!covered) warnings.push(`${report.surface.title} changed but no changeset names \`${pkg}\`.`);
        else if (report.result.breaking.length && covered !== 'major') warnings.push(`${report.surface.title} has a breaking change but its changeset is \`${covered}\`, not \`major\`.`);
      }
    }
  }
  if (warnings.length) lines.push('### Warnings', ...[...new Set(warnings)].map(w => `- ${w}`), '');
  if (breaking.length) {
    lines.push('### Breaking changes', ...breaking.slice(0, 60).map(b => `- ${b.title}: ${code(b.item)} — ${b.reason}`));
    if (breaking.length > 60) lines.push(`- …and ${breaking.length - 60} more`);
    lines.push('');
  }
  if (missing.length) {
    lines.push('### Missing documentation or types', ...missing.slice(0, 60).map(m => `- ${m.title}: ${code(m.item)}${m.reason ? ` — ${m.reason}` : ''}`));
    if (missing.length > 60) lines.push(`- …and ${missing.length - 60} more`);
    lines.push('');
  }
  if (openapi?.report?.trim()) {
    lines.push('### OpenAPI compatibility (oasdiff)', openapi.breaking ? 'oasdiff found breaking changes in `interfaces/http/openapi.json`:' : 'oasdiff found no breaking changes in `interfaces/http/openapi.json`.', '', openapi.report.trim(), '');
  }
  for (const report of changed) {
    const { result, surface: s, file, head, base } = report;
    if (report.baseline) {
      const undocumented = [...head.values()].filter(row => isMissing(row.doc)).length;
      const docs = s.kind === 'sql' ? '' : undocumented ? `, ${undocumented} without documentation yet` : ', all documented';
      lines.push(`### ${s.title}`, `New listing ${code(file)}: ${head.size} items${docs}. ${overview(s.kind, head)}`.trim(), '');
      continue;
    }
    lines.push(`### ${s.title}`, `${result.added.length} added, ${result.changed.length} changed${result.docChanges ? ` (${result.docChanges} with new documentation)` : ''}, ${result.removed.length} removed in ${code(file)}.`, '');
    const diff = [];
    for (const item of [...result.removed, ...result.changed, ...result.added].sort()) {
      if (base?.has(item)) diff.push(`- ${base.get(item).line}`);
      if (head.has(item)) diff.push(`+ ${head.get(item).line}`);
    }
    lines.push('```diff', ...diff.slice(0, DIFF_LIMIT), '```');
    if (diff.length > DIFF_LIMIT) lines.push(`${diff.length - DIFF_LIMIT} more lines; see the Files changed tab.`);
    lines.push('');
  }
  let body = lines.join('\n');
  if (body.length > 60000) body = `${body.slice(0, 60000)}\n\n…comment truncated; see the Files changed tab.`;
  return body;
}

function git(args) {
  return execFileSync('git', args, { cwd: root, encoding: 'utf8', maxBuffer: 64 * 1024 * 1024 });
}

async function headListings(dir = join(root, 'interfaces')) {
  const files = [];
  for (const entry of await readdir(dir, { withFileTypes: true }).catch(() => [])) {
    const path = join(dir, entry.name);
    if (entry.isDirectory()) files.push(...await headListings(path));
    else if (entry.name.endsWith('.txt')) files.push(relative(root, path));
  }
  return files;
}

/** Builds per-file reports comparing `baseRef` with the working tree. */
export async function review(baseRef) {
  const mergeBase = git(['merge-base', baseRef, 'HEAD']).trim();
  const baseFiles = git(['ls-tree', '-r', '--name-only', mergeBase, '--', 'interfaces/']).split('\n').filter(f => f.endsWith('.txt'));
  const baseline = baseFiles.length === 0;
  const files = [...new Set([...baseFiles, ...await headListings()])].sort();
  const reports = [];
  for (const file of files) {
    const s = surface(file);
    const base = baseFiles.includes(file) ? parseListing(git(['show', `${mergeBase}:${file}`])) : undefined;
    const head = parseListing(await readFile(join(root, file), 'utf8').catch(() => ''));
    reports.push({ file, surface: s, base, head, baseline: !base, result: compareListing(s.kind, base, head) });
  }
  const changesetFiles = git(['diff', '--name-only', '--diff-filter=AM', `${mergeBase}...HEAD`, '--', '.changeset/']).split('\n').filter(f => f.endsWith('.md') && !f.endsWith('README.md'));
  const changesets = parseChangesets(await Promise.all(changesetFiles.map(f => readFile(join(root, f), 'utf8'))));
  return { reports, changesets, baseline };
}

if (import.meta.url === pathToFileURL(process.argv[1]).href) {
  const argv = process.argv.slice(2);
  const baseRef = argv[argv.indexOf('--base') + 1];
  if (!argv.includes('--base') || !baseRef) throw new Error('Usage: review.mjs --base <ref> [--comment] [--fail-on-missing-docs]');
  const outcome = await review(baseRef);
  const reportFile = argv.includes('--oasdiff') ? argv[argv.indexOf('--oasdiff') + 1] : undefined;
  const openapi = reportFile ? { report: await readFile(reportFile, 'utf8').catch(() => ''), breaking: argv.includes('--oasdiff-breaking') } : undefined;
  const body = renderComment({ ...outcome, openapi });
  console.log(body);
  if (process.env.GITHUB_STEP_SUMMARY) await appendFile(process.env.GITHUB_STEP_SUMMARY, `${body}\n`);
  const changed = outcome.reports.some(r => r.baseline || r.result.added.length || r.result.removed.length || r.result.changed.length);
  if (argv.includes('--comment')) {
    await upsertComment({ marker: MARKER, body, createIfMissing: changed }).catch(error => {
      // Fork PRs get a read-only token; the job summary still carries the report.
      console.warn(`Could not post the PR comment: ${error.message}`);
    });
  }
  const missing = outcome.reports.flatMap(r => [...r.result.missingDocs, ...(r.result.gaps ?? []).map(gap => `${gap.item} (${gap.reason})`)]);
  if (argv.includes('--fail-on-missing-docs') && missing.length) {
    console.error(`${missing.length} new interface item(s) lack documentation:\n${missing.map(item => `- ${item}`).join('\n')}`);
    process.exitCode = 1;
  }
}
