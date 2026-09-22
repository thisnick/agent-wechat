const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');
const {test} = require('node:test');

// Exercise the actual helper's scheduling contract without invoking native code.
function helper({sourceId = 1} = {}) {
  let callbacks, timer, nativeCalls = 0, removals = 0, clears = 0;
  const context = {
    buildId: 'ce28c3471d532eeb1f136482eeb4d0bdfd59c06e',
    Process: {
      arch: 'x64', id: 10, getCurrentThreadId: () => 11,
      getModuleByName: () => ({base: {add: offset => offset}}),
      enumerateModules: () => [{name: 'libglib-2.0.so', getExportByName: name => name}],
    },
    NativeFunction: function (address) {
      if (address === 'g_idle_add_full') return (_priority, idle, _data, destroyed) => {
        callbacks = {idle, destroyed}; return sourceId;
      };
      if (address === 'g_source_remove') return id => {
        assert.equal(id, sourceId); removals++; callbacks.destroyed(); return 1;
      };
      return () => {nativeCalls++; throw Error('Native invocation not allowed');};
    },
    NativeCallback: function (fn) {return fn;},
    ptr: n => n, rpc: {},
    setTimeout: fn => {timer = fn; return 123;},
    clearTimeout: id => {assert.equal(id, 123); clears++;},
  };
  vm.runInNewContext(fs.readFileSync(require.resolve('../docker/tools/media-download.js'), 'utf8'), context);
  return {
    enqueue: data => context.rpc.exports.enqueue(data),
    timeout: () => timer(),
    dispatch: () => {assert.equal(callbacks.idle(), 0); callbacks.destroyed();},
    stats: () => ({nativeCalls, removals, clears}),
  };
}

test('an undispatched source is removed before its promise resolves', async () => {
  const h = helper();
  const request = h.enqueue({});
  assert.throws(() => h.enqueue({}), /busy/);
  h.timeout();
  assert.equal((await request).status, 'error');
  assert.deepEqual(h.stats(), {nativeCalls: 0, removals: 1, clears: 1});
  const next = h.enqueue({}); h.timeout();
  assert.equal((await next).status, 'error');
});

test('a callback on the wrong thread never touches native message objects', async () => {
  const h = helper();
  const request = h.enqueue({});
  h.dispatch();
  assert.equal((await request).status, 'error');
  h.timeout(); // A stale timer cannot remove a recycled GLib source ID.
  assert.deepEqual(h.stats(), {nativeCalls: 0, removals: 0, clears: 1});
});

test('failed source registration releases the pending slot', async () => {
  const h = helper({sourceId: 0});
  assert.equal((await h.enqueue({})).status, 'error');
  assert.equal((await h.enqueue({})).status, 'error');
  assert.deepEqual(h.stats(), {nativeCalls: 0, removals: 0, clears: 0});
});
