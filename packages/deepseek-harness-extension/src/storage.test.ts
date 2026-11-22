/**
 * Pure-part tests for the WeChat storage declaration. The real host Domain is
 * async and needs a runtime backend, so we test:
 *  (a) every zod record schema accepts a well-formed record and rejects
 *      malformed ones (including the passthrough evolution guarantee),
 *  (b) `wechatDomainSpec()` identity/version/tables and field shapes,
 *  (c) `openWechatTables` wiring against a hand-written in-memory fake Domain.
 */
import test from 'node:test';
import assert from 'node:assert/strict';
import {
  WECHAT_DOMAIN_NAME,
  WECHAT_STORAGE_UNIT_NAME,
  WECHAT_DOMAIN_VERSION,
  AccountRecordSchema,
  ThreadRecordSchema,
  JournalRecordSchema,
  CustodyRecordSchema,
  OutboxRecordSchema,
  RecallRecordSchema,
  ConfigGenerationRecordSchema,
  wechatDomainSpec,
  openWechatTables,
  type TableView,
} from './storage.ts';

const TS = '2026-11-23T02:00:00.000Z';

const validRecords = {
  account: { serverId: 'srv-1', status: 'online', lastSeenAt: TS, generation: 0 },
  thread: {
    routeKey: 'acct:chat@jid',
    sessionId: 'wht-acct-abc12345',
    accountId: 'acct',
    chatJid: 'chat@jid',
    policyFingerprint: 'fp-1',
    revision: 3,
    lastActivityAt: TS,
  },
  journal: { accountId: 'acct', cursor: 'c-42', eventKind: 'message', createdAt: TS },
  custody: {
    runId: 'run-1',
    accountId: 'acct',
    chatJid: 'chat@jid',
    messageIds: ['m1', 'm2'],
    status: 'prepared',
    createdAt: TS,
  },
  outbox: {
    key: 'idem-1',
    accountId: 'acct',
    chatJid: 'chat@jid',
    kind: 'text',
    payload: 'hello',
    status: 'pending',
    createdAt: TS,
    updatedAt: TS,
  },
  recall: { serverMessageId: 'sm-9', accountId: 'acct', chatJid: 'chat@jid', recalledAt: TS },
  configGeneration: { generation: 7 },
} as const;

// ---------------------------------------------------------------------------
// (a) Schemas
// ---------------------------------------------------------------------------

test('AccountRecordSchema validates a well-formed record and rejects malformed ones', () => {
  assert.deepEqual(AccountRecordSchema.parse(validRecords.account), validRecords.account);
  assert.equal(AccountRecordSchema.safeParse({ ...validRecords.account, generation: 1 }).success, true);
  assert.equal(AccountRecordSchema.safeParse({ ...validRecords.account, generation: 1.5 }).success, false);
  assert.equal(AccountRecordSchema.safeParse({ ...validRecords.account, generation: -1 }).success, false);
  const { serverId: _serverId, ...missing } = validRecords.account;
  assert.equal(AccountRecordSchema.safeParse(missing).success, false);
  assert.equal(AccountRecordSchema.safeParse({ ...validRecords.account, serverId: 42 }).success, false);
});

test('ThreadRecordSchema validates a well-formed record and rejects malformed ones', () => {
  assert.deepEqual(ThreadRecordSchema.parse(validRecords.thread), validRecords.thread);
  const { chatJid: _chatJid, ...missing } = validRecords.thread;
  assert.equal(ThreadRecordSchema.safeParse(missing).success, false);
  assert.equal(ThreadRecordSchema.safeParse({ ...validRecords.thread, revision: 2.2 }).success, false);
  assert.equal(ThreadRecordSchema.safeParse({ ...validRecords.thread, routeKey: 7 }).success, false);
});

test('JournalRecordSchema validates a well-formed record and rejects malformed ones', () => {
  assert.deepEqual(JournalRecordSchema.parse(validRecords.journal), validRecords.journal);
  const { cursor: _cursor, ...missing } = validRecords.journal;
  assert.equal(JournalRecordSchema.safeParse(missing).success, false);
  assert.equal(JournalRecordSchema.safeParse({ ...validRecords.journal, eventKind: null }).success, false);
});

test('CustodyRecordSchema validates a well-formed record and rejects malformed ones', () => {
  assert.deepEqual(CustodyRecordSchema.parse(validRecords.custody), validRecords.custody);
  assert.equal(
    CustodyRecordSchema.safeParse({ ...validRecords.custody, messageIds: ['m1', 2] }).success,
    false,
  );
  const { messageIds: _messageIds, ...missing } = validRecords.custody;
  assert.equal(CustodyRecordSchema.safeParse(missing).success, false);
  assert.equal(CustodyRecordSchema.safeParse({ ...validRecords.custody, status: 1 }).success, false);
});

test('OutboxRecordSchema validates well-formed records (with and without optional fields)', () => {
  assert.deepEqual(OutboxRecordSchema.parse(validRecords.outbox), validRecords.outbox);
  const full = {
    ...validRecords.outbox,
    serverId: 'srv-1',
    serverMessageId: 'sm-1',
    reason: 'needs_review: ambiguous readback',
  };
  assert.deepEqual(OutboxRecordSchema.parse(full), full);
  const { key: _key, ...missing } = validRecords.outbox;
  assert.equal(OutboxRecordSchema.safeParse(missing).success, false);
  assert.equal(OutboxRecordSchema.safeParse({ ...validRecords.outbox, status: null }).success, false);
  assert.equal(OutboxRecordSchema.safeParse({ ...validRecords.outbox, serverId: 5 }).success, false);
});

test('RecallRecordSchema validates a well-formed record and rejects malformed ones', () => {
  assert.deepEqual(RecallRecordSchema.parse(validRecords.recall), validRecords.recall);
  const { recalledAt: _recalledAt, ...missing } = validRecords.recall;
  assert.equal(RecallRecordSchema.safeParse(missing).success, false);
  assert.equal(RecallRecordSchema.safeParse({ ...validRecords.recall, accountId: 0 }).success, false);
});

test('ConfigGenerationRecordSchema validates a singleton and rejects malformed ones', () => {
  assert.deepEqual(ConfigGenerationRecordSchema.parse(validRecords.configGeneration), validRecords.configGeneration);
  assert.equal(ConfigGenerationRecordSchema.safeParse({ generation: 0 }).success, true);
  assert.equal(ConfigGenerationRecordSchema.safeParse({ generation: -1 }).success, false);
  assert.equal(ConfigGenerationRecordSchema.safeParse({ generation: 1.5 }).success, false);
  assert.equal(ConfigGenerationRecordSchema.safeParse({}).success, false);
});

test('all record schemas are passthrough: unknown future fields survive parsing', () => {
  const schemas = [
    AccountRecordSchema,
    ThreadRecordSchema,
    JournalRecordSchema,
    CustodyRecordSchema,
    OutboxRecordSchema,
    RecallRecordSchema,
    ConfigGenerationRecordSchema,
  ];
  const records = [
    validRecords.account,
    validRecords.thread,
    validRecords.journal,
    validRecords.custody,
    validRecords.outbox,
    validRecords.recall,
    validRecords.configGeneration,
  ];
  for (let i = 0; i < schemas.length; i++) {
    const schema = schemas[i]!;
    const record = records[i]!;
    const parsed = schema.parse({ ...record, brandNewHostField: { nested: true } });
    assert.deepEqual(
      (parsed as Record<string, unknown>).brandNewHostField,
      { nested: true },
      `schema #${i} must preserve unknown fields`,
    );
  }
});

// ---------------------------------------------------------------------------
// (b) Domain spec
// ---------------------------------------------------------------------------

test('wechatDomainSpec returns the expected identity and all 7 table declarations', () => {
  const spec = wechatDomainSpec();
  assert.equal(WECHAT_DOMAIN_NAME, 'agent-wechat');
  assert.equal(WECHAT_STORAGE_UNIT_NAME, 'agent_wechat');
  assert.equal(spec.name, WECHAT_STORAGE_UNIT_NAME);
  assert.equal(spec.name, 'agent_wechat');
  assert.equal(spec.version, WECHAT_DOMAIN_VERSION);
  assert.equal(spec.version, 1);
  assert.deepEqual(Object.keys(spec.tables).sort(), [
    'accounts',
    'config_generation',
    'custody',
    'journal',
    'outbox',
    'recalls',
    'threads',
  ]);
});

test('wechatDomainSpec tables use valueSchema (host field name) and global uses schema+initial', () => {
  const spec = wechatDomainSpec() as {
    tables: Record<string, { valueSchema?: unknown; schema?: unknown }>;
    global?: { schema: { safeParse(v: unknown): { success: boolean } }; initial: unknown };
  };
  for (const [name, declaration] of Object.entries(spec.tables)) {
    assert.ok(declaration.valueSchema, `${name} must declare valueSchema`);
    assert.equal(declaration.schema, undefined, `${name} must not declare schema`);
  }
  assert.ok(spec.global);
  assert.deepEqual(spec.global.initial, { generation: 0 });
  assert.equal(spec.global.schema.safeParse({ generation: 0 }).success, true);
  assert.equal(spec.global.schema.safeParse({ generation: -1 }).success, false);
});

test('wechatDomainSpec declaration schemas match the exported record schemas', () => {
  const spec = wechatDomainSpec() as {
    tables: Record<string, { valueSchema: { safeParse(v: unknown): { success: boolean } } }>;
  };
  const cases = [
    ['accounts', AccountRecordSchema, validRecords.account],
    ['threads', ThreadRecordSchema, validRecords.thread],
    ['journal', JournalRecordSchema, validRecords.journal],
    ['custody', CustodyRecordSchema, validRecords.custody],
    ['outbox', OutboxRecordSchema, validRecords.outbox],
    ['recalls', RecallRecordSchema, validRecords.recall],
    ['config_generation', ConfigGenerationRecordSchema, validRecords.configGeneration],
  ] as const;
  for (const [name, schema, record] of cases) {
    const declared = spec.tables[name]!.valueSchema;
    assert.equal(declared.safeParse(record).success, true, `${name}: spec validator accepts valid record`);
    assert.equal(declared.safeParse({}).success, schema.safeParse({}).success, `${name}: validator parity`);
  }
});

// ---------------------------------------------------------------------------
// (c) Typed accessor wrapper over a fake Domain
// ---------------------------------------------------------------------------

class FakeKvTable<V> implements TableView<string, V> {
  readonly seen: string[] = [];
  readonly name: string;
  private readonly data: Map<string, V>;
  constructor(name: string, data: Map<string, V> = new Map<string, V>()) {
    this.name = name;
    this.data = data;
  }

  get(key: string): V | undefined { return this.data.get(key); }
  entries(): IterableIterator<[string, V]> { return this.data.entries(); }
  keys(): IterableIterator<string> { return this.data.keys(); }
  get size(): number { return this.data.size; }
  async put(key: string, value: V): Promise<void> { this.seen.push(`put:${key}`); this.data.set(key, value); }
  async delete(key: string): Promise<boolean> { this.seen.push(`delete:${key}`); return this.data.delete(key); }
  async deleteMany(keys: readonly string[]): Promise<number> {
    this.seen.push(`deleteMany:${keys.join(',')}`);
    let n = 0;
    for (const key of keys) if (this.data.delete(key)) n++;
    return n;
  }
  async update(key: string, fn: (current: V) => V): Promise<V> {
    this.seen.push(`update:${key}`);
    const current = this.data.get(key);
    if (current === undefined) throw new Error('missing-key');
    const next = fn(current);
    this.data.set(key, next);
    return next;
  }
}

function fakeDomain() {
  const tables = new Map<string, FakeKvTable<unknown>>();
  const requested: string[] = [];
  const domain = {
    name: WECHAT_DOMAIN_NAME,
    table(name: string) {
      requested.push(name);
      let table = tables.get(name);
      if (!table) {
        table = new FakeKvTable(name);
        tables.set(name, table);
      }
      return table;
    },
  };
  return { domain, tables, requested };
}

test('openWechatTables returns all 7 accessors wired to the right table names', () => {
  const { domain, tables, requested } = fakeDomain();
  const t = openWechatTables(domain);

  assert.deepEqual(Object.keys(t).sort(), [
    'accounts',
    'configGeneration',
    'custody',
    'journal',
    'outbox',
    'recalls',
    'threads',
  ]);
  assert.equal(t.accounts, tables.get('accounts'));
  assert.equal(t.threads, tables.get('threads'));
  assert.equal(t.journal, tables.get('journal'));
  assert.equal(t.custody, tables.get('custody'));
  assert.equal(t.outbox, tables.get('outbox'));
  assert.equal(t.recalls, tables.get('recalls'));
  assert.equal(t.configGeneration, tables.get('config_generation'));
  assert.deepEqual(requested, [
    'accounts', 'threads', 'journal', 'custody', 'outbox', 'recalls', 'config_generation',
  ]);
});

test('typed accessors are the real fake handles (get/put/update work through them)', async () => {
  const { domain, tables } = fakeDomain();
  const t = openWechatTables(domain);

  await t.accounts.put('acct-1', validRecords.account);
  assert.equal(t.accounts.size, 1);
  assert.deepEqual(t.accounts.get('acct-1'), validRecords.account);
  assert.deepEqual([...t.accounts.keys()], ['acct-1']);

  const updated = await t.accounts.update('acct-1', (cur) => ({ ...cur, generation: cur.generation + 1 }));
  assert.equal(updated.generation, 1);
  assert.equal(t.accounts.get('acct-1')?.generation, 1);

  await t.outbox.put('idem-1', validRecords.outbox);
  await t.recalls.put('sm-9', validRecords.recall);
  assert.deepEqual(t.outbox.get('idem-1'), validRecords.outbox);
  assert.deepEqual(t.recalls.get('sm-9'), validRecords.recall);
  assert.deepEqual(tables.get('outbox')?.seen, ['put:idem-1']);
});
