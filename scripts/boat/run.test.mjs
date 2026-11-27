import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import test from 'node:test';

import {
  DEFAULT_MAX_SINGLE_UNTRACKED_BYTES,
  buildApplyScript,
  createStandaloneSeedBundle,
  deriveSandboxName,
  filterUntrackedPaths,
  findCommonBase,
  parseArgs,
  parseBoatExecOutput,
  parseBundlePrerequisites,
  parseJsonOutput,
  parsePullSpec,
  parseRustToolchainChannel,
  pickSandboxAction,
  prepareLocalSync,
  resolvePreset,
  shellQuote,
  toPublicHttpsUrl,
} from './run.mjs';

function runGit(cwd, args, env = process.env) {
  const res = spawnSync('git', args, { cwd, env, encoding: 'utf8' });
  assert.equal(res.status, 0, `git ${args.join(' ')} failed: ${res.stderr}`);
  return res.stdout.trim();
}

function runBash(script) {
  const res = spawnSync('bash', ['-c', script], { encoding: 'utf8' });
  assert.equal(res.status, 0, `bash failed (exit ${res.status}):\n${res.stderr}\n${res.stdout}`);
  return res.stdout;
}

test('parseArgs parses presets, passthrough commands, strict numeric flags, and lifecycle subcommands', () => {
  const preset = parseArgs(['--preset', 'rust-lint', '--committed', '--type', 'large', '--keep']);
  assert.equal(preset.subcommand, 'run');
  assert.equal(preset.preset, 'rust-lint');
  assert.equal(preset.committed, true);
  assert.equal(preset.type, 'large');
  assert.equal(preset.typeExplicit, true);
  assert.equal(preset.ttl, 3600);
  assert.equal(preset.keep, true);
  assert.equal(preset.withEnv, false);

  const passthrough = parseArgs([
    '--name',
    'aw-build-custom',
    '--exclude',
    'tmp-data',
    '--pull',
    'packages/native-client-rust/target/release/wx:./wx',
    '--',
    'cargo',
    'test',
    '--locked',
  ]);
  assert.equal(passthrough.name, 'aw-build-custom');
  assert.equal(passthrough.typeExplicit, false);
  assert.deepEqual(passthrough.excludes, ['tmp-data']);
  assert.deepEqual(passthrough.pulls, [
    { remotePath: 'packages/native-client-rust/target/release/wx', localPath: './wx' },
  ]);
  assert.deepEqual(passthrough.command, ['cargo', 'test', '--locked']);

  const seed = parseArgs(['seed', '--skip-warm-build']);
  assert.equal(seed.subcommand, 'seed');
  assert.equal(seed.skipWarmBuild, true);

  const refMode = parseArgs(['--ref', 'origin/main', '--preset', 'ts']);
  assert.equal(refMode.ref, 'origin/main');
  assert.equal(refMode.committed, true);

  assert.throws(() => parseArgs(['--unknown']), /Unknown option/);
  assert.throws(() => parseArgs(['--ttl', '0']), /integer >= 1/);
  assert.throws(() => parseArgs(['--ttl', '10m']), /integer >= 1/);
  assert.throws(() => parseArgs(['--timeout', '5m']), /integer >= 0/);
});

test('deriveSandboxName sanitizes branch names and disambiguates worktrees/detached HEAD', () => {
  assert.equal(
    deriveSandboxName({
      branch: 'feat/Voice_Fix#123',
      worktreePath: '/repo/main',
      isSecondaryWorktree: false,
    }),
    'aw-build-feat-voice-fix-123',
  );

  const wt1 = deriveSandboxName({
    branch: 'main',
    worktreePath: '/repo/worktrees/agent-1',
    isSecondaryWorktree: true,
  });
  const wt2 = deriveSandboxName({
    branch: 'main',
    worktreePath: '/repo/worktrees/agent-2',
    isSecondaryWorktree: true,
  });
  assert.match(wt1, /^aw-build-main-[0-9a-f]{6}$/);
  assert.notEqual(wt1, wt2);

  const detached = deriveSandboxName({
    branch: 'HEAD',
    worktreePath: '/repo/main',
    isSecondaryWorktree: false,
  });
  assert.match(detached, /^aw-build-detached-[0-9a-f]{6}$/);
});

test('toPublicHttpsUrl normalizes https, scp-style ssh, and ssh:// remotes', () => {
  assert.equal(
    toPublicHttpsUrl('https://github.com/thisnick/agent-wechat'),
    'https://github.com/thisnick/agent-wechat.git',
  );
  assert.equal(
    toPublicHttpsUrl('https://github.com/thisnick/agent-wechat.git'),
    'https://github.com/thisnick/agent-wechat.git',
  );
  assert.equal(
    toPublicHttpsUrl('git@github.com:thisnick/agent-wechat.git'),
    'https://github.com/thisnick/agent-wechat.git',
  );
  assert.equal(
    toPublicHttpsUrl('ssh://git@github.com/thisnick/agent-wechat.git'),
    'https://github.com/thisnick/agent-wechat.git',
  );
  assert.equal(toPublicHttpsUrl('https://token@github.com/thisnick/agent-wechat.git'), null);
  assert.equal(toPublicHttpsUrl('/local/bare/repo.git'), null);
  assert.equal(toPublicHttpsUrl(null), null);
});

test('filterUntrackedPaths skips work/ by default and honors --include-work and --exclude', () => {
  const input = [
    'packages/shared/src/new.ts',
    'work/triage/dump.bin',
    'work/notes.md',
    'scratch/temp.log',
  ];

  const def = filterUntrackedPaths(input);
  assert.deepEqual(def.included, ['packages/shared/src/new.ts', 'scratch/temp.log']);
  assert.deepEqual(def.skipped, ['work/triage/dump.bin', 'work/notes.md']);

  const withExclude = filterUntrackedPaths(input, { excludes: ['scratch/'] });
  assert.deepEqual(withExclude.included, ['packages/shared/src/new.ts']);
  assert.deepEqual(withExclude.skipped, [
    'work/triage/dump.bin',
    'work/notes.md',
    'scratch/temp.log',
  ]);

  const withWork = filterUntrackedPaths(input, { includeWork: true });
  assert.deepEqual(withWork.included, input);
  assert.deepEqual(withWork.skipped, []);
});

test('pickSandboxAction prioritizes reuse > resume > wait-transition > fork-base > create-fresh', () => {
  const baseStopped = {
    id: 'bx_base',
    name: 'aw-build-base',
    state: 'stopped',
    snapshotAvailable: true,
    createdAt: '2026-11-27T01:00:00Z',
  };
  const branchRunning = {
    id: 'bx_run',
    name: 'aw-build-feat',
    state: 'idle',
    createdAt: '2026-11-27T02:00:00Z',
  };
  const branchStopped = {
    id: 'bx_stop',
    name: 'aw-build-feat',
    state: 'stopped',
    createdAt: '2026-11-27T01:30:00Z',
  };

  assert.equal(
    pickSandboxAction({
      sandboxes: [baseStopped, branchStopped, branchRunning],
      targetName: 'aw-build-feat',
      baseName: 'aw-build-base',
    }).action,
    'reuse-running',
  );

  assert.equal(
    pickSandboxAction({
      sandboxes: [baseStopped, branchStopped],
      targetName: 'aw-build-feat',
      baseName: 'aw-build-base',
    }).action,
    'resume',
  );

  const forkPlan = pickSandboxAction({
    sandboxes: [baseStopped],
    targetName: 'aw-build-feat',
    baseName: 'aw-build-base',
  });
  assert.equal(forkPlan.action, 'fork-base');
  assert.equal(forkPlan.baseSandbox.id, 'bx_base');

  assert.equal(
    pickSandboxAction({
      sandboxes: [baseStopped],
      targetName: 'aw-build-feat',
      baseName: 'aw-build-base',
      noFork: true,
    }).action,
    'create-fresh',
  );

  assert.equal(
    pickSandboxAction({
      sandboxes: [{ id: 'bx_err', name: 'aw-build-base', state: 'error' }],
      targetName: 'aw-build-feat',
      baseName: 'aw-build-base',
    }).action,
    'create-fresh',
  );

  const stoppingPlan = pickSandboxAction({
    sandboxes: [{ id: 'bx_stopping', name: 'aw-build-feat', state: 'stopping' }],
    targetName: 'aw-build-feat',
    baseName: 'aw-build-base',
  });
  assert.equal(stoppingPlan.action, 'wait-transition');
  assert.equal(stoppingPlan.sandbox.id, 'bx_stopping');

  const baseStoppingPlan = pickSandboxAction({
    sandboxes: [{ id: 'bx_base_stopping', name: 'aw-build-base', state: 'stopping' }],
    targetName: 'aw-build-feat',
    baseName: 'aw-build-base',
  });
  assert.equal(baseStoppingPlan.action, 'wait-transition');
  assert.equal(baseStoppingPlan.sandbox.id, 'bx_base_stopping');
});

test('parseJsonOutput and parseBoatExecOutput parse JSON/JSONL and reject malformed or failed exec output', () => {
  assert.deepEqual(parseJsonOutput('{"sandboxes":[]}'), { sandboxes: [] });
  const stream = [
    '{"event":"created","id":"bx_1"}',
    '{"event":"state","id":"bx_1","state":"ready"}',
    '{"event":"ready","id":"bx_1","state":"ready"}',
  ].join('\n');
  assert.deepEqual(parseJsonOutput(stream), { event: 'ready', id: 'bx_1', state: 'ready' });

  const ok = parseBoatExecOutput(
    `warning line\n${JSON.stringify({ stdout: 'HAS_REPO=1\n', stderr: '', exitCode: 0, success: true })}\n`,
  );
  assert.equal(ok.stdout, 'HAS_REPO=1\n');
  assert.equal(ok.exitCode, 0);

  assert.throws(() => parseBoatExecOutput('plain text without json'), /Could not parse JSON/);
  assert.throws(() => parseBoatExecOutput('{"hello":"world"}'), /Unexpected non-exec JSON/);
  assert.throws(
    () =>
      parseBoatExecOutput(
        JSON.stringify({ stdout: '', stderr: 'boom', exitCode: 2, success: false }),
      ),
    /exit code 2[\s\S]*boom/,
  );
  assert.throws(
    () =>
      parseBoatExecOutput(
        JSON.stringify({ stdout: '', stderr: '', exitCode: 137, oomKilled: true }),
      ),
    /OOM-killed/,
  );
  assert.throws(
    () =>
      parseBoatExecOutput(
        JSON.stringify({ stdout: '', stderr: '', exitCode: 124, timedOut: true }),
      ),
    /timed out/,
  );
});

test('parseRustToolchainChannel, resolvePreset, parsePullSpec, and shellQuote', () => {
  assert.equal(
    parseRustToolchainChannel('[toolchain]\nchannel = "1.93.1"\nprofile = "minimal"\n'),
    '1.93.1',
  );
  assert.match(resolvePreset('rust-lint', '1.93.1'), /cargo \+1\.93\.1 fmt --all --check/);
  assert.throws(() => resolvePreset('nope', '1.93.1'), /Unknown preset/);
  assert.deepEqual(parsePullSpec('target/release/wx'), {
    remotePath: 'target/release/wx',
    localPath: 'wx',
  });
  assert.equal(shellQuote("a'b c"), "'a'\\''b c'");
});

test('stop/delete --dry-run do not invoke boat', () => {
  const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'aw-boat-fakebin-'));
  try {
    const fakeBoat = path.join(tmp, 'boat');
    fs.writeFileSync(fakeBoat, '#!/usr/bin/env bash\necho "boat must not be called" >&2\nexit 99\n');
    fs.chmodSync(fakeBoat, 0o755);
    for (const sub of ['stop', 'delete', 'status']) {
      const res = spawnSync(
        process.execPath,
        [path.resolve(import.meta.dirname, 'run.mjs'), sub, '--name', 'aw-build-dry', '--dry-run'],
        {
          cwd: path.resolve(import.meta.dirname, '../..'),
          encoding: 'utf8',
          env: { ...process.env, PATH: `${tmp}:${process.env.PATH || ''}` },
        },
      );
      assert.equal(res.status, 0, `${sub} --dry-run failed: ${res.stderr}`);
      const parsed = JSON.parse(res.stdout);
      assert.equal(parsed.subcommand, sub);
      assert.equal(parsed.targetName, 'aw-build-dry');
    }
  } finally {
    fs.rmSync(tmp, { recursive: true, force: true });
  }
});

test('prepareLocalSync handles uncommitted edits, force-staged files, dangling symlinks, size guard, and worktrees', () => {
  const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'aw-boat-test-'));
  try {
    const repo = path.join(tmp, 'repo');
    fs.mkdirSync(repo);

    runGit(repo, ['init', '-q']);
    runGit(repo, ['config', 'user.email', 'test@example.com']);
    runGit(repo, ['config', 'user.name', 'Test']);
    runGit(repo, ['config', 'diff.renames', 'copies']);
    fs.writeFileSync(path.join(repo, '.gitignore'), '*.ignored\n');
    fs.writeFileSync(path.join(repo, 'keep.txt'), 'v1\n');
    fs.writeFileSync(path.join(repo, 'edit.txt'), 'old\n');
    fs.writeFileSync(path.join(repo, 'remove.txt'), 'bye\n');
    runGit(repo, ['add', '-A']);
    runGit(repo, ['commit', '-q', '-m', 'init']);

    const headBefore = runGit(repo, ['rev-parse', 'HEAD']);

    // Create uncommitted edit, deletion, copied staged file, force-staged ignored file,
    // dangling symlink, new untracked file, and untracked work/ scratch file.
    fs.writeFileSync(path.join(repo, 'edit.txt'), 'new\n');
    fs.unlinkSync(path.join(repo, 'remove.txt'));
    fs.writeFileSync(path.join(repo, 'keep-copy.txt'), 'v1\n');
    runGit(repo, ['add', 'keep-copy.txt']);
    fs.writeFileSync(path.join(repo, 'secret.ignored'), 'force-staged\n');
    runGit(repo, ['add', '-f', 'secret.ignored']);
    fs.symlinkSync('missing-target', path.join(repo, 'dangling-link'));
    runGit(repo, ['add', 'dangling-link']);
    fs.writeFileSync(path.join(repo, 'added.txt'), 'hello\n');
    fs.mkdirSync(path.join(repo, 'work'), { recursive: true });
    fs.writeFileSync(path.join(repo, 'work', 'scratch.bin'), 'ignored-by-default\n');

    const gitObjectsBefore = fs.readdirSync(path.join(repo, '.git', 'objects')).sort();

    const sync = prepareLocalSync({
      repoRoot: repo,
      gitCommonDir: path.join(repo, '.git'),
      committed: false,
      ref: null,
      excludes: [],
      includeWork: false,
      allowLargeUntracked: false,
    });
    try {
      assert.equal(sync.realHead, headBefore);
      assert.equal(sync.isDirty, true);
      assert.equal(sync.summary.skippedUntrackedCount, 1);
      assert.deepEqual(sync.summary.skippedUntrackedRoots, ['work']);

      const files = runGit(repo, ['ls-tree', '-r', '--name-only', sync.targetSha], sync.stageEnv)
        .split('\n')
        .sort();
      assert.deepEqual(files, [
        '.gitignore',
        'added.txt',
        'dangling-link',
        'edit.txt',
        'keep-copy.txt',
        'keep.txt',
        'secret.ignored',
      ]);

      // Verify local .git/objects was not mutated.
      const gitObjectsAfter = fs.readdirSync(path.join(repo, '.git', 'objects')).sort();
      assert.deepEqual(gitObjectsAfter, gitObjectsBefore);
    } finally {
      sync.cleanup();
    }

    // Large untracked file guard rejects and cleans up tempDir.
    const bigFile = path.join(repo, 'huge.bin');
    fs.writeFileSync(bigFile, Buffer.alloc(DEFAULT_MAX_SINGLE_UNTRACKED_BYTES + 1));
    assert.throws(
      () =>
        prepareLocalSync({
          repoRoot: repo,
          gitCommonDir: path.join(repo, '.git'),
          committed: false,
          ref: null,
          excludes: [],
          includeWork: false,
          allowLargeUntracked: false,
        }),
      /Refusing to sync large untracked file/,
    );
    fs.unlinkSync(bigFile);

    // Secondary worktree sync works using shared gitCommonDir.
    const wtDir = path.join(tmp, 'wt');
    runGit(repo, ['worktree', 'add', '-q', '-b', 'wt-branch', wtDir, headBefore]);
    fs.writeFileSync(path.join(wtDir, 'wt-only.txt'), 'from-worktree\n');
    const wtCommonDir = path.resolve(wtDir, runGit(wtDir, ['rev-parse', '--git-common-dir']));
    const wtSync = prepareLocalSync({
      repoRoot: wtDir,
      gitCommonDir: wtCommonDir,
      committed: false,
      ref: null,
      excludes: [],
      includeWork: false,
      allowLargeUntracked: false,
    });
    try {
      const wtFiles = runGit(wtDir, ['ls-tree', '-r', '--name-only', wtSync.targetSha], wtSync.stageEnv)
        .split('\n')
        .sort();
      assert.ok(wtFiles.includes('wt-only.txt'));
    } finally {
      wtSync.cleanup();
    }
  } finally {
    fs.rmSync(tmp, { recursive: true, force: true });
  }
});

test('standalone seed bundle (with detached HEAD), shallow boundary checks, and mtime-preserving remote apply', () => {
  const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'aw-boat-sync-'));
  try {
    const localRepo = path.join(tmp, 'local');
    const remoteRepo = path.join(tmp, 'remote');
    fs.mkdirSync(localRepo);

    runGit(localRepo, ['init', '-q']);
    runGit(localRepo, ['config', 'user.email', 'test@example.com']);
    runGit(localRepo, ['config', 'user.name', 'Test']);

    // Commit 1 -> Commit 2 -> Commit 3
    fs.writeFileSync(path.join(localRepo, 'a.txt'), 'a1\n');
    fs.writeFileSync(path.join(localRepo, 'b.txt'), 'b1\n');
    fs.writeFileSync(path.join(localRepo, 'c.txt'), 'c1\n');
    runGit(localRepo, ['add', '-A']);
    runGit(localRepo, ['commit', '-q', '-m', 'c1']);
    const c1 = runGit(localRepo, ['rev-parse', 'HEAD']);

    fs.writeFileSync(path.join(localRepo, 'a.txt'), 'a2\n');
    runGit(localRepo, ['add', '-A']);
    runGit(localRepo, ['commit', '-q', '-m', 'c2']);
    const c2 = runGit(localRepo, ['rev-parse', 'HEAD']);

    // Detach HEAD at c2 to verify createStandaloneSeedBundle handles detached HEAD & parent shallowing.
    runGit(localRepo, ['checkout', '-q', '--detach', c2]);

    // 1. Seed unpack + dirty incremental bundle on top of a depth-1 seed repo.
    fs.writeFileSync(path.join(localRepo, 'a.txt'), 'a2-dirty\n');
    fs.writeFileSync(path.join(localRepo, 'new.txt'), 'new1\n');
    const localSync1 = prepareLocalSync({
      repoRoot: localRepo,
      gitCommonDir: path.join(localRepo, '.git'),
      committed: false,
      ref: null,
      excludes: [],
      includeWork: false,
      allowLargeUntracked: false,
    });
    try {
      const seedBundle = createStandaloneSeedBundle(localRepo, c2, localSync1.tempDir);
      const dirtyBundle = path.join(localSync1.tempDir, 'dirty.bundle');
      runGit(localRepo, ['bundle', 'create', dirtyBundle, `^${c2}`, 'refs/heads/boat-target'], localSync1.stageEnv);
      assert.deepEqual(parseBundlePrerequisites(dirtyBundle), [c2]);

      runGit(tmp, ['init', '-q', remoteRepo]);
      const apply1 = buildApplyScript({
        remoteDir: remoteRepo,
        needSeedReset: true,
        headAlreadyOnRemote: false,
        realHead: c2,
        isDirty: true,
        uploadedBundle: true,
        seedBundlePath: seedBundle,
        syncBundlePath: dirtyBundle,
      });
      runBash(apply1);

      assert.equal(runGit(remoteRepo, ['rev-parse', 'HEAD']), c2);
      assert.equal(runGit(remoteRepo, ['rev-parse', '--is-shallow-repository']), 'true');
      assert.equal(fs.readFileSync(path.join(remoteRepo, 'a.txt'), 'utf8'), 'a2-dirty\n');
      assert.equal(fs.readFileSync(path.join(remoteRepo, 'new.txt'), 'utf8'), 'new1\n');
    } finally {
      localSync1.cleanup();
    }

    // 2. Simulate remote command mutating tracked file `c.txt` while `b.txt` stays untouched.
    //    Ensure next sync reverts `c.txt` WITHOUT touching `b.txt`'s mtime!
    const oldTime = new Date('2021-01-01T00:00:00Z');
    fs.utimesSync(path.join(remoteRepo, 'b.txt'), oldTime, oldTime);
    fs.writeFileSync(path.join(remoteRepo, 'c.txt'), 'remote-mutated\n');

    fs.writeFileSync(path.join(localRepo, 'new.txt'), 'new2\n');
    const localSync2 = prepareLocalSync({
      repoRoot: localRepo,
      gitCommonDir: path.join(localRepo, '.git'),
      committed: false,
      ref: null,
      excludes: [],
      includeWork: false,
      allowLargeUntracked: false,
    });
    try {
      const dirtyBundle2 = path.join(localSync2.tempDir, 'dirty2.bundle');
      runGit(localRepo, ['bundle', 'create', dirtyBundle2, `^${c2}`, 'refs/heads/boat-target'], localSync2.stageEnv);
      const apply2 = buildApplyScript({
        remoteDir: remoteRepo,
        needSeedReset: false,
        headAlreadyOnRemote: true,
        realHead: c2,
        isDirty: true,
        uploadedBundle: true,
        syncBundlePath: dirtyBundle2,
      });
      runBash(apply2);

      // c.txt reverted to local state, new.txt updated, b.txt mtime preserved!
      assert.equal(fs.readFileSync(path.join(remoteRepo, 'c.txt'), 'utf8'), 'c1\n');
      assert.equal(fs.readFileSync(path.join(remoteRepo, 'new.txt'), 'utf8'), 'new2\n');
      assert.equal(
        Math.floor(fs.statSync(path.join(remoteRepo, 'b.txt')).mtimeMs / 1000),
        Math.floor(oldTime.getTime() / 1000),
      );
    } finally {
      localSync2.cleanup();
    }

    // 3. Shallow boundary check: amend c2 -> c2' (parent c1 is NOT on shallow remote).
    //    findCommonBase must return null when isShallow=true instead of returning absent c1!
    runGit(localRepo, ['checkout', '-q', '--', '.']);
    fs.unlinkSync(path.join(localRepo, 'new.txt'));
    fs.writeFileSync(path.join(localRepo, 'a.txt'), 'a2-amended\n');
    runGit(localRepo, ['add', 'a.txt']);
    runGit(localRepo, ['commit', '-q', '--amend', '-m', 'c2-amended']);
    const c2Amended = runGit(localRepo, ['rev-parse', 'HEAD']);

    assert.equal(findCommonBase(localRepo, [c2], c2Amended, { isShallow: false }), c1);
    assert.equal(findCommonBase(localRepo, [c2], c2Amended, { isShallow: true }), null);
  } finally {
    fs.rmSync(tmp, { recursive: true, force: true });
  }
});
