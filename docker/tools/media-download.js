'use strict';
// Full BuildID is checked by the host before this script is loaded.
const profiles = {
  ce28c3471d532eeb1f136482eeb4d0bdfd59c06e: {
    arch: 'x64', alloc: 0xa66dda0, request: 0x5746080, release: 0x4b86ae0,
    submit: 0x7068190, context: 0x603c4e0, lookup: 0x61d1b60,
    manager: 0xa91b150, service: 0xa728ef0, model: 0xa8981a0,
    modelControl: 0xa744f18, requestControl: 0xa75abe8,
    raw: 0x5cd6fa0, destroy: 0x5cd7380, type: 0x6055d00,
    parse: 0x60561b0, factory: 0x6059a60, string: 0x450c6a0,
  },
  e9f1cd045de714536a9e739aafa6d8362f317cd0: {
    arch: 'arm64', alloc: 0x9f9e3a8, request: 0x5656864, release: 0x467ab84,
    submit: 0x6d85acc, context: 0x5ee9a50, lookup: 0x6060db0,
    manager: 0xa25a770, service: 0xa0685f0, model: 0xa1d77c0,
    modelControl: 0xa084618, requestControl: 0xa09a2e8,
    raw: 0x5bb4a84, destroy: 0x5bb4e20, type: 0x5f02500,
    parse: 0x5f02928, factory: 0x5f060cc, string: 0x449a84c,
  },
};
const p = profiles[buildId];
if (!p || Process.arch !== p.arch) throw Error('Unsupported build');
const base = Process.getModuleByName('wechat').base;
const nf = (off, ret, args) => new NativeFunction(base.add(off), ret, args, { traps: 'none' });
const allocate = nf(p.alloc, 'pointer', ['ulong']);
const release = nf(p.release, 'void', ['pointer']);
const constructRaw = nf(p.raw, 'void', ['pointer']);
const destroyRaw = nf(p.destroy, 'void', ['pointer']);
const setType = nf(p.type, 'void', ['pointer', 'int']);
const parseRaw = nf(p.parse, 'void', ['pointer', 'int']);
const stringCtor = nf(p.string, 'void', ['pointer', 'pointer', 'ulong']);
const constructRequest = nf(p.request, 'void', ['pointer', 'pointer', 'pointer', 'pointer']);
const submit = nf(p.submit, 'void', ['pointer', 'pointer', 'int', 'int']);
const appContext = nf(p.context, 'pointer', []);
// AAPCS64 returns these objects through x8. Keep the bridge alive for the session.
let bridge = null, sret = null;
if (p.arch === 'arm64') {
  bridge = Memory.alloc(Process.pageSize);
  Memory.patchCode(bridge, 32, code => {
    const w = new Arm64Writer(code, { pc: bridge });
    w.putMovRegReg('x8', 'x0'); w.putMovRegReg('x0', 'x1');
    w.putMovRegReg('x1', 'x2'); w.putBrReg('x3'); w.flush();
  });
  sret = new NativeFunction(bridge, 'void', ['pointer', 'pointer', 'pointer', 'pointer'], { traps: 'none' });
}
function indirect(address, out, a, b) {
  if (sret) sret(out, a, b || ptr(0), address);
  else if (b) new NativeFunction(address, 'void', ['pointer', 'pointer', 'pointer'], { traps: 'none' })(out, a, b);
  else new NativeFunction(address, 'void', ['pointer', 'pointer'], { traps: 'none' })(out, a);
}
function emptyBox() { const b = Memory.alloc(16); b.writeByteArray(new Uint8Array(16)); return b; }
function control(size, vtable) {
  const c = allocate(size);
  if (c.isNull()) throw Error('Allocation failed');
  c.writeByteArray(new Uint8Array(size)); c.writePointer(base.add(vtable)); return c;
}
function box(c) { const b = emptyBox(); b.writePointer(c.add(24)); b.add(8).writePointer(c); return b; }
function putString(dst, text) {
  const value = Memory.allocUtf8String(text);
  stringCtor(dst, value, unescape(encodeURIComponent(text)).length);
}
function stringAt(s) {
  const tag = s.readU8();
  const len = (tag & 1) ? s.add(8).readU64().toNumber() : tag >>> 1;
  if (len > 128) throw Error('String bound');
  return ((tag & 1) ? s.add(16).readPointer() : s.add(1)).readUtf8String(len);
}
function manager() {
  const first = emptyBox(), second = emptyBox(), result = emptyBox();
  const key = Memory.alloc(8); key.writePointer(base.add(p.service));
  function virtual(obj, off, out) {
    if (obj.isNull()) throw Error('Missing context');
    indirect(obj.readPointer().add(off).readPointer(), out, obj);
  }
  try {
    virtual(appContext(), 0x68, first);
    virtual(first.readPointer(), 0x30, second);
    indirect(base.add(p.lookup), result, second.readPointer(), key);
    if (result.readPointer().isNull() || !result.readPointer().readPointer().equals(base.add(p.manager))) throw Error('Service mismatch');
    return result;
  } catch (e) { release(result); throw e; }
  finally { release(second); release(first); }
}
function queue(metadata) {
  let model = null, request = null, service = null;
  const raw = Memory.alloc(0x278); constructRaw(raw);
  try {
    putString(raw.add(0x18), metadata.chatId);
    putString(raw.add(0x30), metadata.accountId);
    setType(raw, Number(metadata.local_type) & 0xffffffff);
    putString(raw.add(0x130), metadata.content);
    raw.add(0xf4).writeU32(metadata.local_id);
    raw.add(0xf8).writeU64(uint64(metadata.server_id));
    raw.add(0x100).writeU64(uint64(metadata.sort_seq));
    raw.add(0x114).writeU32(metadata.create_time);
    parseRaw(raw, 1);
    if (raw.add(0x220).readPointer().isNull()) throw Error('Missing payload');
    const c = control(0x368, p.modelControl);
    indirect(base.add(p.factory), c.add(24), raw); model = box(c);
    const m = model.readPointer(), r = m.add(0x238).readPointer();
    if (!m.readPointer().equals(base.add(p.model)) || r.isNull() ||
        r.add(0xf4).readU32() !== metadata.local_id ||
        r.add(0xf8).readU64().toString() !== metadata.server_id ||
        stringAt(r.add(0x18)) !== metadata.chatId ||
        r.add(12).readU32() !== (metadata.local_type === 3 ? 3 : 49) ||
        (metadata.local_type !== 3 && m.add(8).readU64().toString() !== '25769803825')) throw Error('Message mismatch');
    service = manager();
    const q = control(0x68, p.requestControl); request = box(q);
    const resource = Memory.alloc(4); resource.writeU32(metadata.local_type === 3 ? 3 : 103);
    const tag = Memory.alloc(1); tag.writeU8(0);
    constructRequest(tag, q.add(24), model, resource);
    submit(service.readPointer(), request, metadata.local_type === 3 ? 100 : 0, 0);
  } finally {
    if (request) release(request);
    if (service) release(service);
    if (model) release(model);
    destroyRaw(raw);
  }
}
const glib = Process.enumerateModules().find(m => m.name.startsWith('libglib-2.0.so'));
if (!glib) throw Error('GLib unavailable');
const addIdle = new NativeFunction(glib.getExportByName('g_idle_add_full'), 'uint', ['int', 'pointer', 'pointer', 'pointer']);
const removeSource = new NativeFunction(glib.getExportByName('g_source_remove'), 'int', ['uint']);
let active = null;
// Session-owned callbacks outlive every one-shot GLib source.
const idle = new NativeCallback(() => {
  try {
    if (Process.getCurrentThreadId() !== Process.id) throw Error('Wrong UI thread');
    queue(active.metadata); active.result = { status: 'queued' };
  } catch (_) { active.result = { status: 'error' }; }
  return 0;
}, 'int', ['pointer']);
const destroyed = new NativeCallback(() => {
  const done = active; active = null;
  if (done.timer) clearTimeout(done.timer);
  done.resolve(done.result || { status: 'error' });
}, 'void', ['pointer']);
rpc.exports = {
  enqueue(metadata) {
    if (active) throw Error('Submission busy');
    return new Promise(resolve => {
      const task = { metadata, resolve, result: null, timer: null };
      active = task;
      const source = addIdle(200, idle, ptr(0), destroyed);
      if (!source) {
        active = null; resolve({ status: 'error' });
      } else if (active === task) {
        // Cancel an undispatched source before reporting a timeout. Never unload
        // callbacks while GLib still owns them. A running callback finishes first.
        task.timer = setTimeout(() => {
          if (active === task) removeSource(source);
        }, 5000);
      }
    });
  },
};
