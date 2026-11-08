import test from 'node:test';
import assert from 'node:assert/strict';
import { compareListing, parseChangesets, parseListing, renderComment, surface } from './review.mjs';
import { decide } from './approval.mjs';

const listing = rows => parseListing(`# header\n${rows.join('\n')}\n`);
const compare = (kind, before, after) => compareListing(kind, before && listing(before), listing(after));
const reasons = result => result.breaking.map(b => `${b.item}: ${b.reason}`);

const cli = [
  'wx chats | - | Find chats',
  'wx chats list | - | List chats',
  'wx chats list --limit | takes <LIMIT>, default 50 | Return at most this many chats',
  'wx chats list --kind | takes <KIND>, values direct|group | Only list this kind of chat',
  'wx chats list --all | switch, short -a, alias --everything | Include archived chats',
  'wx chat | hidden alias | Alias of wx chats',
];

test('CLI removals, new required arguments and narrowed values are breaking', () => {
  const result = compare('cli', cli, [
    'wx chats | - | Find chats',
    'wx chats list | - | List chats',
    'wx chats list --limit | takes <N>, default 20 | Return at most this many chats',
    'wx chats list --kind | takes <KIND>, values direct | Only list this kind of chat',
    'wx chats list --all | switch | Include archived chats',
    'wx chats list <QUERY> | position 1, required | Text to match',
  ]);
  assert.deepEqual(reasons(result).sort(), [
    'wx chat: alias removed',
    'wx chats list --all: alias --everything removed',
    'wx chats list --all: short -a removed',
    'wx chats list --kind: allowed values removed: group',
    'wx chats list --limit: default changed from 50 to 20',
    'wx chats list <QUERY>: new required argument on an existing command',
  ]);
});

test('CLI additions and help-text edits need approval but are not breaking', () => {
  const result = compare('cli', cli, [...cli.filter(row => !row.startsWith('wx chats list --limit')),
    'wx chats list --limit | takes <LIMIT>, default 50 | Show at most this many chats',
    'wx chats list --cursor | takes <CURSOR> | Continue from this page cursor',
    'wx chats find | - | MISSING HELP',
  ]);
  assert.deepEqual(result.breaking, []);
  assert.deepEqual(result.added.sort(), ['wx chats find', 'wx chats list --cursor']);
  assert.deepEqual(result.changed, ['wx chats list --limit']);
  assert.deepEqual(result.missingDocs, ['wx chats find']);
});

test('a first listing is a baseline: everything is added and nothing is breaking', () => {
  const result = compare('cli', undefined, [...cli, 'wx chats get | - | MISSING HELP']);
  assert.equal(result.added.length, cli.length + 1);
  assert.deepEqual(result.breaking, []);
  assert.deepEqual(result.missingDocs, []);
});

test('schema listings flag removed, retyped, narrowed and newly required parameters', () => {
  const tools = [
    'tool wechat_history | enabled by default | Read history',
    'tool wechat_history.limit | integer, min 1, max 50, default 20 | Page size',
    'tool wechat_history.cursor | string | Page cursor',
    'tool wechat_history.kind | string, values text|media | Message kind',
  ];
  const result = compare('schema', tools, [
    'tool wechat_history | optional | Read history',
    'tool wechat_history.limit | string, min 1, max 20, default 20 | Page size',
    'tool wechat_history.kind | string, values text|media|link | Message kind',
    'tool wechat_history.query | string, required | Text to find',
  ]);
  assert.deepEqual(reasons(result).sort(), [
    'tool wechat_history.cursor: removed',
    'tool wechat_history.limit: max narrowed to 20',
    'tool wechat_history.limit: type changed from integer to string',
    'tool wechat_history.query: new required field',
    'tool wechat_history: tool is no longer enabled by default',
  ]);
});

test('crate listings flag signature changes and additions that break exhaustive types', () => {
  const before = [
    'struct demo::Open | struct { public fields } | Options',
    'field demo::Open::path | std::path::PathBuf | Where',
    'enum demo::Kind | enum | Kinds',
    'variant demo::Kind::A | - | A',
    'enum demo::Event | #[non_exhaustive] enum | Events',
    'variant demo::Event::Start | - | Start',
    'trait demo::Sink | trait | Sinks',
    'fn demo::Sink::write | required fn(&mut self, bytes: &[u8]) | Write',
    'fn demo::open | fn(path: &std::path::Path) -> bool | Open',
    'fn demo::close | fn() | Close',
  ];
  const result = compare('crate', before, [
    ...before.filter(row => !row.startsWith('fn demo::open') && !row.startsWith('fn demo::close')),
    'fn demo::open | fn(path: &std::path::Path) -> std::io::Result<()> | Open',
    'field demo::Open::mode | u32 | Mode',
    'variant demo::Kind::B | - | B',
    'variant demo::Event::Stop | - | Stop',
    'fn demo::Sink::flush | required fn(&mut self) | Flush',
    'fn demo::Sink::name | provided fn(&self) -> &str | Name',
    'fn demo::reopen | fn() | MISSING DOC',
  ]);
  assert.deepEqual(reasons(result).sort(), [
    'field demo::Open::mode: new field on a struct that callers can construct',
    'fn demo::Sink::flush: new required trait method',
    'fn demo::close: removed',
    'fn demo::open: signature changed',
    'variant demo::Kind::B: new variant on an exhaustive enum',
  ]);
  assert.deepEqual(result.missingDocs, ['fn demo::reopen']);
});

test('existing documentation gaps are allowed; new and regressed ones are not', () => {
  const result = compare('crate', ['fn demo::a | fn() | MISSING DOC', 'fn demo::b | fn() | Does b'],
    ['fn demo::a | fn() | MISSING DOC', 'fn demo::b | fn() | MISSING DOC', 'fn demo::c | fn() | MISSING DOC']);
  assert.deepEqual(result.missingDocs.sort(), ['fn demo::b', 'fn demo::c']);
});

test('the comment explains a baseline without calling anything breaking', () => {
  const head = listing(cli);
  const file = 'interfaces/cli/wx.txt';
  const body = renderComment({ baseline: true, changesets: new Map(), reports: [{ file, surface: surface(file), head, baseline: true, result: compareListing('cli', undefined, head) }] });
  assert.match(body, /adds the interface listings for the first time/);
  assert.match(body, /interfaces-approved/);
  assert.doesNotMatch(body, /Breaking changes/);
});

test('the comment lists breaking changes, missing docs and changeset gaps with a diff', () => {
  const file = 'interfaces/cli/wx.txt';
  const base = listing(cli);
  const head = listing([...cli.slice(0, 2), 'wx chats list --new | switch | MISSING HELP']);
  const body = renderComment({ baseline: false, changesets: new Map([['@agent-wechat/agent-wechat', 'patch']]),
    reports: [{ file, surface: surface(file), base, head, baseline: false, result: compareListing('cli', base, head) }] });
  assert.match(body, /### Breaking changes/);
  assert.match(body, /`wx chats list --limit` — removed/);
  assert.match(body, /### Missing documentation/);
  assert.match(body, /breaking change but its changeset is `patch`/);
  assert.match(body, /```diff\n- wx chat \| hidden alias \| Alias of wx chats/);
  assert.match(body, /\+ wx chats list --new \| switch \| MISSING HELP/);
});

test('a PR without listing changes gets a short note', () => {
  const head = listing(cli);
  const file = 'interfaces/cli/wx.txt';
  const body = renderComment({ baseline: false, changesets: new Map(), reports: [{ file, surface: surface(file), base: head, head, baseline: false, result: compareListing('cli', head, head) }] });
  assert.match(body, /No interface listing changed/);
});

test('changesets report the highest bump per package', () => {
  const bumps = parseChangesets(['---\n"@agent-wechat/cli": patch\n---\n\nHelp text', "---\n'@agent-wechat/cli': minor\n\"@agent-wechat/rust-crates\": major\n---\n"]);
  assert.equal(bumps.get('@agent-wechat/cli'), 'minor');
  assert.equal(bumps.get('@agent-wechat/rust-crates'), 'major');
});

test('approval follows the label and resets when the listings change afterwards', () => {
  assert.equal(decide({ changed: false, labeled: false }).pass, true);
  assert.deepEqual(decide({ changed: true, labeled: false, currentTree: 'aaa' }), { pass: false, action: 'none', message: 'Interface listings changed. Waiting for the interfaces-approved label.' });
  assert.equal(decide({ changed: true, labeled: true, labelAdded: true, currentTree: 'aaa' }).action, 'record');
  assert.equal(decide({ changed: true, labeled: true, approvedTree: 'aaa', currentTree: 'aaa' }).pass, true);
  const reset = decide({ changed: true, labeled: true, approvedTree: 'aaa', currentTree: 'bbb' });
  assert.equal(reset.pass, false);
  assert.equal(reset.action, 'reset');
  assert.equal(decide({ changed: true, labeled: true, currentTree: 'bbb' }).action, 'reset');
});

test('SQL schema listings flag migrations, version bumps and risky column changes', () => {
  const before = [
    'schema version | 12 | -',
    'migration account/V012__calls.sql | sha256 aaaa | -',
    'table calls | CREATE TABLE calls(id TEXT PRIMARY KEY,state TEXT) | -',
    'table calls.id | TEXT, primary key 1 | -',
    'table calls.state | TEXT | -',
    'table calls.retries | INTEGER, not null, default 0 | -',
  ];
  const migrated = compare('sql', before, [
    'schema version | 13 | -',
    'migration account/V012__calls.sql | sha256 bbbb | -',
    'migration account/V013__more.sql | sha256 cccc | -',
    'table calls | CREATE TABLE calls(id TEXT PRIMARY KEY,state TEXT NOT NULL, owner TEXT NOT NULL) | -',
    'table calls.id | TEXT, primary key 1 | -',
    'table calls.state | TEXT, not null | -',
    'table calls.owner | TEXT, not null | -',
  ]);
  assert.deepEqual(reasons(migrated).sort(), [
    'migration account/V012__calls.sql: existing migration edited: stores that already applied it never get the change',
    'schema version: schema version raised from 12 to 13: older wx releases refuse a store at this version, so downgrading stops working',
    'table calls.owner: new NOT NULL column without a default on an existing table',
    'table calls.retries: removed',
    'table calls.state: column is now NOT NULL',
  ]);
  const unversioned = compare('sql', before, [...before, 'index calls_state | CREATE INDEX calls_state ON calls(state) | -']);
  assert.deepEqual(reasons(unversioned), ['schema version: schema changed without a new schema version: existing stores keep the old schema']);
  assert.deepEqual(unversioned.missingDocs, []);
});

test('WebSocket rules depend on message direction and the protocol version', () => {
  const before = [
    'socket control | path /v1/accounts/{a}/calls/control, scope calls, protocol 2 | Call control',
    'client CallCommand[call.dial] | message | Place a call',
    'client CallCommand[call.dial].target | object, required | Who to call',
    'client CallCommand[call.dial].note | string | Optional note',
    'client CallCommand[call.dial].mode | string, values audio|video | Call mode',
    'server CallEvent[call.state] | message | Call state changed',
    'server CallEvent[call.state].state | string, required, values ringing|ended | Current state',
    'server CallEvent[call.state].reason | string, required | Why it changed',
    'server CallEvent[call.error] | message | Command failed',
  ];
  const result = compare('ws', before, [
    'socket control | path /v1/accounts/{a}/calls/control, scope calls, protocol 2 | Call control',
    'client CallCommand[call.dial] | message | Place a call',
    'client CallCommand[call.dial].target | object, required | Who to call',
    'client CallCommand[call.dial].note | string, required | Optional note',
    'client CallCommand[call.dial].mode | string, values audio | Call mode',
    'client CallCommand[call.dial].priority | integer, required | Priority',
    'server CallEvent[call.state] | message | Call state changed',
    'server CallEvent[call.state].state | string, required, values ringing|ended|held | Current state',
    'server CallEvent[call.state].reason | string | Why it changed',
    'server CallEvent[call.state].since | integer, required | When',
    'server CallEvent[call.held] | message | Call held',
  ]);
  assert.deepEqual(reasons(result).sort(), [
    'client CallCommand[call.dial].mode: allowed values removed: video',
    'client CallCommand[call.dial].note: now required',
    'client CallCommand[call.dial].priority: new required field',
    'protocol version: breaking change without a call protocol version bump',
    'server CallEvent[call.error]: removed from server messages: clients that read it break',
    'server CallEvent[call.state].reason: may now be absent',
  ]);
  assert.deepEqual(result.notes.map(n => `${n.item}: ${n.reason}`).sort(), [
    'server CallEvent[call.held]: new server message type: clients must ignore types they do not know',
    'server CallEvent[call.state].state: new values held: clients must handle them',
  ]);
  const bumped = compare('ws', before, before.filter(row => !row.startsWith('server CallEvent[call.error]')).map(row => row.replace('protocol 2', 'protocol 3')));
  assert.deepEqual(reasons(bumped), ['server CallEvent[call.error]: removed from server messages: clients that read it break']);
  assert.deepEqual(bumped.notes.map(n => n.reason), ['protocol version changed from 2 to 3']);
});

test('removed UI routes are breaking because links and notifications use them', () => {
  const result = compare('routes', ['route /{server}/{account}/chats | params server, account | Chat list', 'route /{server}/{account}/chats/{thread} | params server, account, thread | A conversation'],
    ['route /{server}/{account}/chats | params server, account | Chat list', 'route /{server}/{account}/c/{thread} | params server, account, thread | A conversation']);
  assert.deepEqual(reasons(result), ['route /{server}/{account}/chats/{thread}: route removed: saved links and notifications that use it stop working']);
});
