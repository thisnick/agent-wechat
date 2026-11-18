import test from 'node:test';
import assert from 'node:assert/strict';
import { affects, select, steps } from './index.mjs';

const selected = files => steps.filter(step => affects(step, files)).map(step => step.name);

test('only the listings whose sources changed are selected', () => {
  assert.deepEqual(selected(['packages/client-ui/src/main.tsx']), ['ui', 'scenes']);
  assert.deepEqual(selected(['packages/native-client-rust/crates/wechat-api/src/lib.rs']), ['websocket', 'http']);
  assert.deepEqual(selected(['packages/native-client-rust/crates/wx/src/main.rs']), ['cli']);
  assert.deepEqual(selected(['packages/native-client-rust/crates/wechat-calls/src/mixer.rs']), ['crates']);
  assert.deepEqual(selected(['packages/openclaw-extension/src/native-login.ts']), ['openclaw']);
  assert.deepEqual(selected(['docs/src/content/docs/use/media.mdx', 'README.md']), []);
});

test('shared inputs and hand-edited listings select their listings', () => {
  assert.deepEqual(selected(['packages/native-client-rust/Cargo.lock']), ['cli', 'storage', 'websocket', 'http', 'crates']);
  assert.deepEqual(selected(['interfaces/http/openapi.json']), ['http']);
  assert.deepEqual(selected(['docs/src/content/docs/openclaw/tools.mdx']), ['openclaw']);
  assert.deepEqual(selected(['scripts/interfaces/review.mjs']), steps.map(step => step.name));
});

test('a change to the CI job selects every listing', () => {
  assert.deepEqual(selected(['.github/workflows/interfaces.yml']), steps.map(step => step.name));
});

test('select runs named listings, every listing without a merge base, or the affected ones', () => {
  const names = (argv, files) => select(argv, files).map(step => step.name);
  assert.deepEqual(names(['--check', 'http', 'ui'], ['README.md']), ['http', 'ui']);
  assert.deepEqual(names(['--check'], null), steps.map(step => step.name));
  assert.deepEqual(names(['--affected'], ['packages/native-client-rust/Cargo.lock']), ['cli', 'storage', 'websocket', 'http', 'crates']);
  assert.deepEqual(names(['--affected'], ['docs/src/content/docs/use/media.mdx']), []);
});
