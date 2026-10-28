import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtemp, mkdir, writeFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { changelogEntry, mergeReleaseNotes, renderReleaseNotes } from './native-release-notes.mjs';

async function fixture(t, cliNotes = '### Major Changes\n\n- Native daemon.', pluginNotes = '### Major Changes\n\n- OpenClaw SecretRefs.', desktopNotes = '') {
  const root = await mkdtemp(join(tmpdir(), 'native-release-notes-'));
  t.after(() => rm(root, { recursive: true, force: true }));
  for (const [directory, name, notes] of [['cli', '@agent-wechat/cli', cliNotes], ['openclaw-extension', '@agent-wechat/agent-wechat', pluginNotes], ['desktop', '@agent-wechat/desktop', desktopNotes]]) {
    const path = join(root, 'packages', directory); await mkdir(path, { recursive: true });
    await writeFile(join(path, 'package.json'), JSON.stringify({ name, version: '1.0.0', ...(directory === 'desktop' ? { private: true } : {}) }));
    await writeFile(join(path, 'CHANGELOG.md'), `# ${name}\n\n## 1.0.0\n\n${notes}\n\n## 0.16.0\n\n- Old release.\n`);
  }
  return root;
}
test('release notes include exact versioned Changesets entries from both public packages', async t => {
  const root = await fixture(t);
  const notes = await renderReleaseNotes({ root, version: '1.0.0' });
  assert.match(notes, /## @agent-wechat\/cli 1\.0\.0\n\n### Major Changes\n\n- Native daemon\./);
  assert.match(notes, /## @agent-wechat\/agent-wechat 1\.0\.0\n\n### Major Changes\n\n- OpenClaw SecretRefs\./);
  assert.doesNotMatch(notes, /Old release/);
  assert.equal(await renderReleaseNotes({ root, existing: notes }), notes);
});
test('fixed-group empty CLI entry does not hide plugin changes or include the previous release', async t => {
  const root = await fixture(t, '');
  const notes = await renderReleaseNotes({ root });
  assert.match(notes, /OpenClaw SecretRefs/); assert.doesNotMatch(notes, /Old release|## @agent-wechat\/cli/);
});
test('desktop-only changes produce release notes when both npm packages have empty fixed-group entries', async t => {
  const root = await fixture(t, '', '', '### Minor Changes\n\n- Per-account notifications.');
  const notes = await renderReleaseNotes({ root });
  assert.match(notes, /## @agent-wechat\/desktop 1\.0\.0\n\n### Minor Changes\n\n- Per-account notifications\./);
  assert.doesNotMatch(notes, /Old release|## @agent-wechat\/cli|## @agent-wechat\/agent-wechat/);
  assert.equal(await renderReleaseNotes({ root, existing: notes }), notes);
});
test('desktop release notes remain alongside CLI and plugin changes', async t => {
  const root = await fixture(t, undefined, undefined, '- Desktop settings.');
  const notes = await renderReleaseNotes({ root });
  for (const content of ['Native daemon.', 'OpenClaw SecretRefs.', 'Desktop settings.']) assert.ok(notes.includes(content));
});
test('desktop metadata and exact changelog version are checked before publication', async t => {
  const root = await fixture(t, '', '', '- Desktop settings.');
  const path = join(root, 'packages/desktop');
  for (const pkg of [
    { name: '@agent-wechat/desktop', version: '1.0.1', private: true },
    { name: '@agent-wechat/other', version: '1.0.0', private: true },
    { name: '@agent-wechat/desktop', version: '1.0.0' },
  ]) {
    await writeFile(join(path, 'package.json'), JSON.stringify(pkg));
    await assert.rejects(renderReleaseNotes({ root }), /package\/version mismatch/);
  }
  await writeFile(join(path, 'package.json'), JSON.stringify({ name: '@agent-wechat/desktop', version: '1.0.0', private: true }));
  await writeFile(join(path, 'CHANGELOG.md'), '## 0.16.0\n\n- Old release.');
  await assert.rejects(renderReleaseNotes({ root }), /one exact changelog entry/);
});
test('missing, empty, duplicate and mismatched versions fail before publication', async t => {
  const root = await fixture(t, '', '');
  await assert.rejects(renderReleaseNotes({ root }), /no release notes/);
  await assert.rejects(renderReleaseNotes({ root, version: '1.0.1' }), /mismatch/);
  await assert.rejects(renderReleaseNotes({ root, version: 'invalid' }), /Invalid release notes version/);
  assert.throws(() => changelogEntry('## 0.16.0\n\n- Old.', '1.0.0'), /one exact/);
  assert.throws(() => changelogEntry('## 1.0.0\n- First\n## 1.0.0\n- Second', '1.0.0'), /one exact/);
});
test('Markdown links, nested lists, fenced headings and prerelease versions are preserved', () => {
  const entry = '### Minor Changes\n\n- [#1](https://example.invalid/pr/1) Details.\n\n  - Nested note.\n\n```md\n## 0.0.0\n```\n\n~~~md\n## 9.9.9\n~~~';
  assert.equal(changelogEntry(`## 1.0.0-beta.1\r\n\r\n${entry.replace(/\n/g, '\r\n')}\r\n\r\n## 0.16.0\r\nold`, '1.0.0-beta.1'), entry);
});
test('release retries update only the owned Changesets block and preserve manual notes', () => {
  const original = mergeReleaseNotes('Operator introduction.', 'Old generated notes.') + '\nOperator follow-up.\n';
  const updated = mergeReleaseNotes(original, 'New generated notes.');
  assert.ok(updated.startsWith('Operator introduction.\n\n'));
  assert.ok(updated.endsWith('\nOperator follow-up.\n'));
  assert.match(updated, /New generated notes/); assert.doesNotMatch(updated, /Old generated notes/);
  assert.equal(mergeReleaseNotes(updated, 'New generated notes.'), updated);
  for (const malformed of ['<!-- agent-wechat:changesets:start -->', '<!-- agent-wechat:changesets:end -->', original + original]) {
    assert.throws(() => mergeReleaseNotes(malformed, 'New'), /ambiguous/);
  }
});
