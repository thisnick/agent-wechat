/**
 * Tests for the settings-page connection-status endpoint
 * (`GET /plugins/wechat-channel/status`) that `bindWechatHost` registers on the
 * host web server. The route must be read-only, secret-free and fail-soft.
 */
import assert from 'node:assert/strict';
import { test } from 'node:test';
import { bindStatusRoute, wechatStatusReport } from './host-binding.ts';

interface CapturedResponse {
  status?: number;
  headers?: Record<string, string>;
  body?: string | undefined;
  ended: boolean;
}

function captureRes(): { res: unknown; captured: CapturedResponse } {
  const captured: CapturedResponse = { ended: false };
  const res = {
    writeHead(status: number, headers: Record<string, string>) {
      captured.status = status;
      captured.headers = headers;
    },
    end(body?: string) {
      captured.ended = true;
      captured.body = body;
    },
  };
  return { res, captured };
}

test('wechatStatusReport exposes state and endpoint, never secrets', () => {
  const full = wechatStatusReport(
    { running: true, attempts: 2, serverId: 'server-1' },
    { native: { endpoint: 'http://127.0.0.1:6174', accountId: 'account-1', tokenCredentialRef: 'WECHAT_DAEMON_TOKEN' } },
  );
  assert.deepEqual(full, {
    service: 'wechat-channel',
    running: true,
    attempts: 2,
    serverId: 'server-1',
    endpoint: 'http://127.0.0.1:6174',
    accountId: 'account-1',
    checkedAt: full.checkedAt,
  });
  assert.ok(typeof full.checkedAt === 'string' && full.checkedAt.length > 0);
  assert.equal('error' in full, false);
  assert.equal('tokenCredentialRef' in full, false);

  const bare = wechatStatusReport({ running: false, attempts: 0 }, {});
  assert.deepEqual(bare, { service: 'wechat-channel', running: false, attempts: 0, checkedAt: bare.checkedAt });

  const parked = wechatStatusReport({ running: false, attempts: 3, error: 'fetch failed' }, {});
  assert.equal(parked.error, 'fetch failed');
  assert.equal(parked.running, false);
});

test('bindStatusRoute registers an exact GET route at /plugins/wechat-channel/status', () => {
  const routes: Array<{ kind: string; path: string }> = [];
  let readCount = 0;
  const dispose = bindStatusRoute(
    {
      register(route) {
        routes.push({ kind: route.kind, path: route.path });
        return () => {};
      },
    },
    () => {
      readCount += 1;
      return { running: true, attempts: 1, serverId: 'sid' };
    },
    { native: { endpoint: 'http://127.0.0.1:6174', accountId: 'account-1' } },
  );
  assert.equal(typeof dispose, 'function');
  assert.deepEqual(routes, [{ kind: 'exact', path: '/plugins/wechat-channel/status' }]);

  // The handler is stored on the registered route; read state lazily.
  readCount = 0;
  const { res, captured } = captureRes();
  // Re-register to grab the handler (register captured above discarded it).
  let handler: ((req: unknown, res: unknown) => void) | undefined;
  bindStatusRoute(
    {
      register(route) {
        handler = route.handler as never;
        return () => {};
      },
    },
    () => ({ running: true, attempts: 1, serverId: 'sid' }),
    { native: { endpoint: 'http://127.0.0.1:6174' } },
  );
  assert.equal(typeof handler, 'function');
  handler!({ method: 'GET' }, res);
  assert.equal(captured.status, 200);
  assert.equal(captured.headers?.['Content-Type'], 'application/json; charset=utf-8');
  assert.equal(captured.headers?.['Cache-Control'], 'no-store');
  const payload = JSON.parse(captured.body!) as Record<string, unknown>;
  assert.equal(payload.service, 'wechat-channel');
  assert.equal(payload.running, true);
  assert.equal(payload.serverId, 'sid');
  assert.equal(payload.endpoint, 'http://127.0.0.1:6174');
  assert.equal(typeof payload.checkedAt, 'string');
  assert.equal(readCount, 0); // the first binding's reader was never invoked
});

test('bindStatusRoute: HEAD omits the body; non-GET methods get 405', () => {
  let handler: ((req: unknown, res: unknown) => void) | undefined;
  bindStatusRoute(
    {
      register(route) {
        handler = route.handler as never;
        return () => {};
      },
    },
    () => ({ running: false, attempts: 2, error: 'fetch failed' }),
    {},
  );

  const head = captureRes();
  handler!({ method: 'HEAD' }, head.res);
  assert.equal(head.captured.status, 200);
  assert.equal(head.captured.body, undefined);
  assert.equal(head.captured.ended, true);

  const post = captureRes();
  handler!({ method: 'POST' }, post.res);
  assert.equal(post.captured.status, 405);
  assert.equal(post.captured.headers?.Allow, 'GET, HEAD');
  assert.equal(post.captured.body, undefined);
});

test('bindStatusRoute: payload reflects state read at request time', () => {
  let handler: ((req: unknown, res: unknown) => void) | undefined;
  let running = false;
  bindStatusRoute(
    {
      register(route) {
        handler = route.handler as never;
        return () => {};
      },
    },
    () => ({ running, attempts: 1 }),
    { native: { endpoint: 'http://127.0.0.1:6174' } },
  );

  const before = captureRes();
  handler!({ method: 'GET' }, before.res);
  assert.equal((JSON.parse(before.captured.body!) as { running: boolean }).running, false);

  running = true;
  const after = captureRes();
  handler!({ method: 'GET' }, after.res);
  assert.equal((JSON.parse(after.captured.body!) as { running: boolean }).running, true);
});

test('bindStatusRoute: a rejecting webServer degrades to undefined', () => {
  const dispose = bindStatusRoute(
    {
      register() {
        throw new Error('route already registered');
      },
    },
    () => ({ running: false, attempts: 0 }),
    {},
  );
  assert.equal(dispose, undefined);
});

test('bindStatusRoute: disposal forwards to the registered route disposer', () => {
  let disposed = 0;
  const dispose = bindStatusRoute(
    {
      register() {
        return () => {
          disposed += 1;
        };
      },
    },
    () => ({ running: false, attempts: 0 }),
    {},
  );
  dispose!();
  assert.equal(disposed, 1);
});
