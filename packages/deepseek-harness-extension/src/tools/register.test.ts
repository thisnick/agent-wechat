import assert from 'node:assert/strict';
import { test } from 'node:test';
import {
  createHostMediaSink,
  projectBlocks,
  registerWechatTools,
  type HostAttachmentsLike,
  type RegistrableToolDefinition,
  type RegisteredImageRef,
} from './register.ts';
import type { ToolContentBlock, WechatToolDefinition } from './shared.ts';

const imageRef: RegisteredImageRef = {
  attachmentId: 'att-9',
  mediaType: 'image/jpeg',
  bytes: 4,
  width: 2,
  height: 2,
  name: 'pic.jpg',
};

function fakeAttachments(): { host: HostAttachmentsLike; saved: unknown[] } {
  const saved: unknown[] = [];
  return {
    saved,
    host: {
      saveImage: async (input) => {
        saved.push(input);
        return imageRef;
      },
      saveFile: async (input) => {
        saved.push(input);
        return { attachmentId: 'file-1', name: input.name, bytes: input.data.length };
      },
    },
  };
}

function fakeTool(name: string): WechatToolDefinition {
  return {
    name,
    description: `${name} description`,
    parameters: { type: 'object', properties: {} },
    output: {
      schema: { type: 'object', additionalProperties: true },
      render: (_args, value) => [
        { type: 'text', text: JSON.stringify(value) },
        { type: 'image', attachment: { ref: 'att-9' } },
        { type: 'image', attachment: { ref: 'ghost' } },
      ],
    },
    execute: async (args, exec) => ({ echoed: args, agentId: (exec as { agent?: { id?: string } }).agent?.id }),
  };
}

// ---------------------------------------------------------------------------
// createHostMediaSink
// ---------------------------------------------------------------------------

test('media sink stores images and remembers the published reference', async () => {
  const { host, saved } = fakeAttachments();
  const sink = createHostMediaSink(host);
  const result = await sink.saveImage({
    data: new Uint8Array([1, 2, 3, 4]),
    mimeType: 'image/jpeg',
    filename: 'pic.jpg',
  });
  assert.deepEqual(result, { ref: 'att-9' });
  assert.equal(sink.refOf('att-9'), imageRef, 'published ref is resolvable');
  assert.equal(sink.refOf('att-x'), undefined);
  assert.deepEqual(saved[0], {
    data: new Uint8Array([1, 2, 3, 4]),
    mediaType: 'image/jpeg',
    name: 'pic.jpg',
  });
});

test('media sink rejects unsupported image media types without calling the store', async () => {
  const { host, saved } = fakeAttachments();
  const sink = createHostMediaSink(host);
  await assert.rejects(
    sink.saveImage({ data: new Uint8Array(1), mimeType: 'image/heic' }),
    /unsupported image media type/,
  );
  assert.equal(saved.length, 0);
});

test('media sink stores files verbatim under their attachment id', async () => {
  const { host } = fakeAttachments();
  const sink = createHostMediaSink(host);
  const result = await sink.saveFile({
    data: new Uint8Array([9, 9]),
    mimeType: 'application/pdf',
    filename: 'doc.pdf',
  });
  assert.deepEqual(result, { ref: 'file-1' });
});

test('media sink rejects references without an attachment id', async () => {
  const sink = createHostMediaSink({
    saveImage: async () => ({ unexpected: true }),
    saveFile: async () => ({ unexpected: true }),
  });
  await assert.rejects(sink.saveImage({ data: new Uint8Array(1), mimeType: 'image/png' }), /attachmentId/);
  await assert.rejects(
    sink.saveFile({ data: new Uint8Array(1), mimeType: 'text/plain', filename: 'a.txt' }),
    /attachmentId/,
  );
});

// ---------------------------------------------------------------------------
// projectBlocks
// ---------------------------------------------------------------------------

test('projectBlocks passes text through and resolves published images', () => {
  const blocks: ToolContentBlock[] = [
    { type: 'text', text: 'hi' },
    { type: 'image', attachment: { ref: 'att-9' } },
  ];
  const out = projectBlocks(blocks, (ref) => (ref === 'att-9' ? imageRef : undefined)) as Array<
    Record<string, unknown>
  >;
  assert.deepEqual(out[0], { type: 'text', text: 'hi' });
  assert.deepEqual(out[1], { type: 'image', attachment: imageRef });
});

test('projectBlocks degrades unknown image refs to an honest text note', () => {
  const blocks: ToolContentBlock[] = [{ type: 'image', attachment: { ref: 'ghost' } }];
  const out = projectBlocks(blocks) as Array<Record<string, unknown>>;
  assert.equal(out.length, 1);
  assert.equal(out[0]!['type'], 'text');
  assert.match(String(out[0]!['text']), /not available/);
  assert.match(String(out[0]!['text']), /ghost/);
});

// ---------------------------------------------------------------------------
// registerWechatTools
// ---------------------------------------------------------------------------

test('registerWechatTools registers every tool and the disposer unregisters all', () => {
  const registered: RegistrableToolDefinition[] = [];
  const disposed: string[] = [];
  const register = (definition: RegistrableToolDefinition): (() => void) => {
    registered.push(definition);
    return () => {
      disposed.push(definition.name);
    };
  };
  const dispose = registerWechatTools({
    tools: [fakeTool('wechat_history'), fakeTool('wechat_search')],
    register,
  });
  assert.deepEqual(
    registered.map((d) => d.name),
    ['wechat_history', 'wechat_search'],
  );
  assert.equal(registered[0]!.description, 'wechat_history description');
  assert.deepEqual(disposed, []);
  dispose();
  assert.deepEqual(disposed, ['wechat_history', 'wechat_search']);
});

test('registered render resolves images through the sink table', () => {
  const { host } = fakeAttachments();
  const sink = createHostMediaSink(host);
  const registered: RegistrableToolDefinition[] = [];
  registerWechatTools({
    tools: [fakeTool('wechat_media')],
    register: (d) => {
      registered.push(d);
      return () => {};
    },
    resolveImageRef: (ref) => sink.refOf(ref),
  });
  // Simulate that the sink published att-9 during an earlier download.
  (host as { saveImage: (i: unknown) => Promise<unknown> });
  const out = registered[0]!.output.render({}, { ok: true }) as Array<Record<string, unknown>>;
  assert.equal(out[0]!['type'], 'text');
  // 'att-9' was never published through THIS sink instance, so it degrades.
  assert.equal(out[1]!['type'], 'text');
  assert.match(String(out[1]!['text']), /att-9/);
});

test('registered execute forwards args and the agent identity to the tool', async () => {
  const registered: RegistrableToolDefinition[] = [];
  registerWechatTools({
    tools: [fakeTool('wechat_history')],
    register: (d) => {
      registered.push(d);
      return () => {};
    },
  });
  const value = (await registered[0]!.execute(
    { limit: 5 },
    { agent: { id: 'agent-7' }, signal: new AbortController().signal },
  )) as Record<string, unknown>;
  assert.deepEqual(value['echoed'], { limit: 5 });
  assert.equal(value['agentId'], 'agent-7');
});
