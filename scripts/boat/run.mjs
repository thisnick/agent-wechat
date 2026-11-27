#!/usr/bin/env node
/**
 * Run builds, checks, and tests remotely on Boat sandboxes with zero Boat-side
 * environment configuration (`--no-env --no-monitoring`).
 *
 * See scripts/boat/boat.md for usage and design details.
 */
import { createHash, randomBytes } from 'node:crypto';
import { spawn, spawnSync } from 'node:child_process';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { pathToFileURL } from 'node:url';

export const REPO_ROOT = path.resolve(import.meta.dirname, '../..');
export const DEFAULT_BASE_NAME = 'aw-build-base';
export const DEFAULT_REMOTE_DIR = 'agent-wechat';
export const DEFAULT_EXCLUDES = ['work'];
export const DEFAULT_MAX_SINGLE_UNTRACKED_BYTES = 10 * 1024 * 1024; // 10 MiB
export const DEFAULT_MAX_TOTAL_UNTRACKED_BYTES = 50 * 1024 * 1024; // 50 MiB

const RUNNING_STATES = new Set(['idle', 'running', 'ready']);
const BUSY_STATES = new Set(['busy']);
const TRANSITION_STATES = new Set(['stopping', 'pending', 'starting', 'resuming']);
const FORKABLE_STATES = new Set(['stopped', 'idle', 'running', 'busy', 'ready']);
export const DEFAULT_REMOTE_LOCK_PATH = '/tmp/aw-boat-run.lock';

export function shellQuote(value) {
  return `'${String(value).replace(/'/g, `'\\''`)}'`;
}

export function parseRustToolchainChannel(tomlText) {
  const match = tomlText.match(/^\s*channel\s*=\s*"([^"]+)"/m);
  if (!match) {
    throw new Error('Could not find channel = "..." in rust-toolchain.toml');
  }
  return match[1];
}

export function resolvePreset(presetName, rustToolchain) {
  const manifest = 'packages/native-client-rust/Cargo.toml';
  const presets = {
    'rust-fmt': `cargo +${rustToolchain} fmt --all --check --manifest-path ${manifest}`,
    'rust-clippy': `cargo +${rustToolchain} clippy --workspace --all-targets --locked --manifest-path ${manifest} -- -D warnings`,
    'rust-lint':
      `cargo +${rustToolchain} fmt --all --check --manifest-path ${manifest} && ` +
      `cargo +${rustToolchain} clippy --workspace --all-targets --locked --manifest-path ${manifest} -- -D warnings`,
    'rust-build': 'pnpm native:build',
    'rust-test': 'pnpm native:test',
    ts: 'pnpm build && pnpm typecheck && pnpm --filter @agent-wechat/agent-wechat test',
    verify:
      `cargo +${rustToolchain} fmt --all --check --manifest-path ${manifest} && ` +
      `cargo +${rustToolchain} clippy --workspace --all-targets --locked --manifest-path ${manifest} -- -D warnings && ` +
      'pnpm native:test && bash scripts/generate-types.sh --check && pnpm interfaces --check',
  };
  if (!(presetName in presets)) {
    throw new Error(
      `Unknown preset "${presetName}". Available presets: ${Object.keys(presets).join(', ')}`,
    );
  }
  return presets[presetName];
}

export function sanitizeSlug(raw, maxLen = 40) {
  const slug = String(raw || '')
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, '-')
    .replace(/^-+|-+$/g, '');
  if (!slug) return 'worktree';
  return slug.slice(0, maxLen).replace(/-+$/g, '') || 'worktree';
}

export function deriveSandboxName({ branch, worktreePath }) {
  const hash = createHash('sha256')
    .update(path.resolve(worktreePath || REPO_ROOT))
    .digest('hex')
    .slice(0, 6);
  if (!branch || branch === 'HEAD') {
    return `aw-build-detached-${hash}`;
  }
  // Always include the checkout path hash so separate clones on the same branch never collide
  // and a branch named `base` cannot collide with the reserved seed name `aw-build-base`.
  const slug = sanitizeSlug(branch, 36);
  return `aw-build-${slug}-${hash}`;
}

export function toPublicHttpsUrl(remoteUrl) {
  if (!remoteUrl) return null;
  const trimmed = remoteUrl.trim();
  const httpsMatch = trimmed.match(/^https:\/\/([^/\s@]+)\/([^\s]+?)(?:\.git)?$/);
  if (httpsMatch) {
    return `https://${httpsMatch[1]}/${httpsMatch[2]}.git`;
  }
  const scpMatch = trimmed.match(/^git@([^:\s]+):([^\s]+?)(?:\.git)?$/);
  if (scpMatch) {
    return `https://${scpMatch[1]}/${scpMatch[2]}.git`;
  }
  const sshMatch = trimmed.match(/^ssh:\/\/git@([^/\s]+)\/([^\s]+?)(?:\.git)?$/);
  if (sshMatch) {
    return `https://${sshMatch[1]}/${sshMatch[2]}.git`;
  }
  return null;
}

export function filterUntrackedPaths(paths, { includeWork = false, excludes = [] } = {}) {
  const effectiveExcludes = [
    ...(includeWork ? [] : DEFAULT_EXCLUDES),
    ...excludes,
  ]
    .map((entry) => entry.replace(/^\/+|\/+$/g, ''))
    .filter(Boolean);

  const included = [];
  const skipped = [];

  for (const relPath of paths) {
    const normalized = relPath.replace(/^\/+/, '');
    const isExcluded = effectiveExcludes.some(
      (prefix) => normalized === prefix || normalized.startsWith(`${prefix}/`),
    );
    if (isExcluded) {
      skipped.push(normalized);
    } else {
      included.push(normalized);
    }
  }
  return { included, skipped };
}

export function parseJsonOutput(stdout) {
  const text = String(stdout || '').trim();
  if (!text) return null;
  try {
    return JSON.parse(text);
  } catch {
    const lines = text
      .split(/\r?\n/)
      .map((line) => line.trim())
      .filter(Boolean);
    const parsed = [];
    for (const line of lines) {
      try {
        parsed.push(JSON.parse(line));
      } catch {
        // Ignore non-JSON preamble/progress lines
      }
    }
    if (parsed.length === 0) {
      throw new Error(`Could not parse JSON output from boat:\n${text.slice(0, 400)}`);
    }
    return parsed[parsed.length - 1];
  }
}

export function parseBoatExecOutput(raw) {
  const parsed = parseJsonOutput(raw);
  if (!parsed || typeof parsed !== 'object' || typeof parsed.exitCode !== 'number') {
    throw new Error(
      `Unexpected non-exec JSON output from boat exec:\n${String(raw || '').slice(0, 400)}`,
    );
  }
  const stdout = typeof parsed.stdout === 'string' ? parsed.stdout : '';
  const stderr = typeof parsed.stderr === 'string' ? parsed.stderr : '';
  const exitCode = parsed.exitCode;
  if (parsed.timedOut) {
    throw new Error(`Remote command timed out.${stderr ? `\n${stderr.trim()}` : ''}`);
  }
  if (parsed.oomKilled) {
    throw new Error(`Remote command was OOM-killed.${stderr ? `\n${stderr.trim()}` : ''}`);
  }
  if (exitCode !== 0 || parsed.success === false) {
    const detail = [stderr, stdout].filter(Boolean).join('\n').trim();
    throw new Error(
      `Remote command failed with exit code ${exitCode}${detail ? `:\n${detail}` : ''}`,
    );
  }
  return { stdout, stderr, exitCode };
}

export function pickSandboxAction({ sandboxes, targetName, baseName, noFork = false }) {
  const byNewest = (a, b) =>
    String(b.createdAt || '').localeCompare(String(a.createdAt || ''));

  const matches = sandboxes
    .filter((box) => box && box.name === targetName && box.state !== 'error')
    .sort(byNewest);

  const busy = matches.find((box) => BUSY_STATES.has(box.state));
  if (busy) {
    return { action: 'busy', sandbox: busy };
  }

  const running = matches.find((box) => RUNNING_STATES.has(box.state));
  if (running) {
    return { action: 'reuse-running', sandbox: running };
  }

  const stopped = matches.find((box) => box.state === 'stopped');
  if (stopped) {
    return { action: 'resume', sandbox: stopped };
  }

  const transitioning = matches.find((box) => TRANSITION_STATES.has(box.state));
  if (transitioning) {
    return { action: 'wait-transition', sandbox: transitioning };
  }

  if (!noFork && baseName && baseName !== targetName) {
    const baseMatches = sandboxes
      .filter((box) => box && box.name === baseName && box.state !== 'error')
      .sort(byNewest);
    const forkable = baseMatches.find(
      (box) =>
        FORKABLE_STATES.has(box.state) &&
        (box.state === 'stopped' || box.snapshotAvailable !== false),
    );
    if (forkable) {
      return { action: 'fork-base', baseSandbox: forkable };
    }
    const baseTransitioning = baseMatches.find((box) => TRANSITION_STATES.has(box.state));
    if (baseTransitioning) {
      return { action: 'wait-transition', sandbox: baseTransitioning };
    }
  }

  return { action: 'create-fresh' };
}

export function parsePullSpec(spec) {
  const idx = spec.indexOf(':');
  if (idx === -1) {
    return {
      remotePath: spec,
      localPath: path.basename(spec),
    };
  }
  return {
    remotePath: spec.slice(0, idx),
    localPath: spec.slice(idx + 1) || path.basename(spec.slice(0, idx)),
  };
}

function parseStrictNonNegativeInt(raw, flagName, { min = 0 } = {}) {
  if (!/^\d+$/.test(String(raw))) {
    throw new Error(`${flagName} must be an integer >= ${min} (got "${raw}")`);
  }
  const value = Number.parseInt(raw, 10);
  if (!Number.isSafeInteger(value) || value < min) {
    throw new Error(`${flagName} must be an integer >= ${min} (got "${raw}")`);
  }
  return value;
}

export function parseArgs(argv) {
  const args = [...argv];
  let subcommand = 'run';
  if (
    args.length > 0 &&
    !args[0].startsWith('-') &&
    ['run', 'seed', 'status', 'stop', 'delete', 'help'].includes(args[0])
  ) {
    subcommand = args.shift();
  }

  const opts = {
    subcommand,
    name: null,
    baseName: DEFAULT_BASE_NAME,
    type: 'default',
    typeExplicit: false,
    ttl: 3600,
    keep: false,
    committed: false,
    ref: null,
    excludes: [],
    includeWork: false,
    allowLargeUntracked: false,
    noFork: false,
    preset: null,
    pulls: [],
    timeout: 0,
    withEnv: false,
    withDocsFonts: false,
    skipSetup: false,
    skipWarmBuild: false,
    forceUnlock: false,
    dryRun: false,
    command: [],
  };

  let i = 0;
  while (i < args.length) {
    const token = args[i];
    if (token === '--') {
      opts.command = args.slice(i + 1);
      break;
    }
    const nextValue = () => {
      if (i + 1 >= args.length) {
        throw new Error(`Option ${token} requires a value`);
      }
      i += 1;
      return args[i];
    };

    switch (token) {
      case '-h':
      case '--help':
        opts.subcommand = 'help';
        return opts;
      case '--name':
        opts.name = nextValue();
        break;
      case '--base-name':
        opts.baseName = nextValue();
        break;
      case '--type':
        opts.type = nextValue();
        opts.typeExplicit = true;
        break;
      case '--ttl':
        opts.ttl = parseStrictNonNegativeInt(nextValue(), '--ttl', { min: 1 });
        break;
      case '--keep':
        opts.keep = true;
        break;
      case '--committed':
        opts.committed = true;
        break;
      case '--ref':
        opts.ref = nextValue();
        opts.committed = true;
        break;
      case '--exclude':
        opts.excludes.push(nextValue());
        break;
      case '--include-work':
        opts.includeWork = true;
        break;
      case '--allow-large-untracked':
        opts.allowLargeUntracked = true;
        break;
      case '--no-fork':
        opts.noFork = true;
        break;
      case '--preset':
        opts.preset = nextValue();
        break;
      case '--pull':
        opts.pulls.push(parsePullSpec(nextValue()));
        break;
      case '--timeout':
        opts.timeout = parseStrictNonNegativeInt(nextValue(), '--timeout', { min: 0 });
        break;
      case '--with-env':
        opts.withEnv = true;
        break;
      case '--with-docs-fonts':
        opts.withDocsFonts = true;
        break;
      case '--skip-setup':
        opts.skipSetup = true;
        break;
      case '--skip-warm-build':
        opts.skipWarmBuild = true;
        break;
      case '--force-unlock':
        opts.forceUnlock = true;
        break;
      case '--dry-run':
        opts.dryRun = true;
        break;
      default:
        throw new Error(`Unknown option: ${token} (put remote commands after --)`);
    }
    i += 1;
  }

  return opts;
}

function usageText() {
  return `Usage:
  node scripts/boat/run.mjs [options] -- <command...>
  node scripts/boat/run.mjs --preset <name> [options]
  node scripts/boat/run.mjs seed [--type default|large] [--skip-warm-build]
  node scripts/boat/run.mjs status [--name <sandbox>]
  node scripts/boat/run.mjs stop [--name <sandbox>]
  node scripts/boat/run.mjs delete [--name <sandbox>]

Presets:
  rust-fmt      cargo fmt --all --check
  rust-clippy   cargo clippy --workspace --all-targets --locked -- -D warnings
  rust-lint     rust-fmt + rust-clippy
  rust-build    pnpm native:build
  rust-test     pnpm native:test
  ts            pnpm build && pnpm typecheck && pnpm --filter @agent-wechat/agent-wechat test
  verify        rust-lint + rust-test + generate-types --check + interfaces --check

Options:
  --name <name>             Sandbox name (default: aw-build-<branch>-<path-hash>)
  --base-name <name>        Warm seed sandbox to fork from (default: aw-build-base)
  --type <size>             Boat machine size: small, default, large, xlarge (default: default)
  --ttl <secs>              Auto-stop TTL in seconds (default: 3600)
  --keep                    Leave sandbox running after command finishes (default: stop it)
  --committed               Sync committed HEAD only (ignore uncommitted working-tree changes)
  --ref <git-ref>           Sync a specific committed git ref (implies --committed)
  --exclude <path>          Exclude an untracked path prefix (repeatable; work/ excluded by default)
  --include-work            Include untracked files under work/ when syncing uncommitted files
  --allow-large-untracked   Allow untracked files over 10 MiB / 50 MiB total
  --no-fork                 Do not fork from aw-build-base when creating a new sandbox
  --pull <remote[:local]>   Copy file from sandbox via boat scp after build (repeatable)
  --timeout <secs>          Remote command timeout in seconds (0 = no limit, default: 0)
  --with-env                Allow Boat account env/secrets (default uses --no-env --no-monitoring)
  --with-docs-fonts         Install fonts-noto-cjk during setup
  --skip-setup              Skip running scripts/boat/setup.sh
  --skip-warm-build         (seed only) Skip initial Cargo warm compile
  --force-unlock            Clear a stale remote run lock before starting
  --dry-run                 Print the planned action without mutating Boat sandboxes
`;
}

export function parseBundlePrerequisites(bundlePath) {
  // Git bundle headers are UTF-8 lines ending at the first blank line before the binary packfile.
  const fd = fs.openSync(bundlePath, 'r');
  try {
    const chunks = [];
    const buf = Buffer.alloc(4096);
    let total = 0;
    while (total < 1024 * 1024) {
      const n = fs.readSync(fd, buf, 0, buf.length, total);
      if (n <= 0) break;
      chunks.push(Buffer.from(buf.subarray(0, n)));
      total += n;
      const joined = Buffer.concat(chunks);
      const sep = joined.indexOf('\n\n');
      if (sep !== -1) {
        const header = joined.subarray(0, sep).toString('utf8');
        const prereqs = [];
        for (const line of header.split('\n')) {
          const m = line.match(/^-([0-9a-f]{40})(?:\s|$)/);
          if (m) prereqs.push(m[1]);
        }
        return prereqs;
      }
    }
    return [];
  } finally {
    fs.closeSync(fd);
  }
}

function runGit(args, { cwd = REPO_ROOT, env = process.env, input, allowFail = false } = {}) {
  const res = spawnSync('git', args, {
    cwd,
    env,
    input,
    encoding: 'utf8',
    maxBuffer: 64 * 1024 * 1024,
  });
  if (res.status !== 0 && !allowFail) {
    throw new Error(
      `git ${args.join(' ')} failed (exit ${res.status}):\n${res.stderr || res.stdout}`,
    );
  }
  return res;
}

let activeStreamChild = null;

function runBoat(args, { capture = true, stream = false } = {}) {
  const fullArgs = ['--no-update', ...args];
  if (stream) {
    return new Promise((resolve, reject) => {
      const child = spawn('boat', fullArgs, { stdio: 'inherit' });
      activeStreamChild = child;
      child.on('error', (err) => {
        activeStreamChild = null;
        reject(err);
      });
      child.on('close', (code, signal) => {
        activeStreamChild = null;
        if (signal) {
          resolve(signal === 'SIGINT' ? 130 : signal === 'SIGTERM' ? 143 : 128);
        } else {
          resolve(code ?? 1);
        }
      });
    });
  }
  const res = spawnSync('boat', fullArgs, {
    encoding: 'utf8',
    stdio: capture ? ['ignore', 'pipe', 'pipe'] : 'inherit',
    maxBuffer: 64 * 1024 * 1024,
  });
  if (res.error) {
    throw new Error(
      `Failed to run "boat": ${res.error.message}. Make sure Boat CLI is installed and signed in (\`boat status\`).`,
    );
  }
  if (res.status !== 0) {
    const detail = [res.stderr, res.stdout].filter(Boolean).join('\n').trim();
    throw new Error(`boat ${fullArgs.join(' ')} failed (exit ${res.status})${detail ? `:\n${detail}` : ''}`);
  }
  return res.stdout || '';
}

function runBoatExecCapture(sandboxId, script, { cwd = null, timeout = 120 } = {}) {
  const args = ['exec', sandboxId, '--json', '--timeout', String(timeout)];
  if (cwd) {
    args.push('--cwd', cwd);
  }
  args.push('--', 'bash', '-lc', script);
  const raw = runBoat(args);
  return parseBoatExecOutput(raw).stdout;
}

function listSandboxes() {
  const raw = runBoat(['list', '--all', '--json']);
  const parsed = parseJsonOutput(raw);
  return Array.isArray(parsed?.sandboxes) ? parsed.sandboxes : [];
}

function getLocalGitInfo(repoRoot = REPO_ROOT) {
  const branch = runGit(['rev-parse', '--abbrev-ref', 'HEAD'], { cwd: repoRoot }).stdout.trim();
  const gitDir = path.resolve(
    repoRoot,
    runGit(['rev-parse', '--git-dir'], { cwd: repoRoot }).stdout.trim(),
  );
  const gitCommonDir = path.resolve(
    repoRoot,
    runGit(['rev-parse', '--git-common-dir'], { cwd: repoRoot }).stdout.trim(),
  );
  const originRes = runGit(['remote', 'get-url', 'origin'], { cwd: repoRoot, allowFail: true });
  const originUrl = originRes.status === 0 ? originRes.stdout.trim() : null;
  return {
    branch,
    gitDir,
    gitCommonDir,
    isSecondaryWorktree: gitDir !== gitCommonDir,
    originUrl,
    publicHttpsUrl: toPublicHttpsUrl(originUrl),
  };
}

function pathExistsOnDisk(fullPath) {
  try {
    fs.lstatSync(fullPath);
    return true;
  } catch {
    return false;
  }
}

/**
 * Prepare local git target (realHead + optional ephemeral worktree commit in a temp gitdir).
 * Never writes to the user's real `.git` directory.
 */
export function prepareLocalSync({
  repoRoot,
  gitCommonDir,
  committed,
  ref,
  excludes,
  includeWork,
  allowLargeUntracked,
}) {
  const realHead = runGit(['rev-parse', '--verify', `${ref || 'HEAD'}^{commit}`], {
    cwd: repoRoot,
  }).stdout.trim();
  const headTree = runGit(['rev-parse', `${realHead}^{tree}`], { cwd: repoRoot }).stdout.trim();

  const tempDir = fs.mkdtempSync(path.join(os.tmpdir(), 'aw-boat-'));
  try {
    const stageGitDir = path.join(tempDir, 'gitdir');
    const stageIndex = path.join(tempDir, 'index');
    fs.mkdirSync(path.join(stageGitDir, 'objects', 'info'), { recursive: true });
    fs.mkdirSync(path.join(stageGitDir, 'refs', 'heads'), { recursive: true });
    fs.writeFileSync(path.join(stageGitDir, 'HEAD'), 'ref: refs/heads/boat-target\n');
    fs.writeFileSync(
      path.join(stageGitDir, 'objects', 'info', 'alternates'),
      `${path.join(gitCommonDir, 'objects')}\n`,
    );

    const stageEnv = {
      ...process.env,
      GIT_DIR: stageGitDir,
      GIT_WORK_TREE: repoRoot,
      GIT_INDEX_FILE: stageIndex,
      GIT_AUTHOR_NAME: 'boat-build',
      GIT_AUTHOR_EMAIL: 'boat-build@local',
      GIT_COMMITTER_NAME: 'boat-build',
      GIT_COMMITTER_EMAIL: 'boat-build@local',
    };

    let targetSha = realHead;
    let isDirty = false;
    const summary = {
      realHead,
      modified: 0,
      deleted: 0,
      added: 0,
      skippedUntrackedCount: 0,
      skippedUntrackedRoots: [],
    };

    if (!committed) {
      // 1. Enumerate untracked non-ignored files and apply exclusions + size guard.
      const untrackedRaw = runGit(['ls-files', '--others', '--exclude-standard', '-z'], {
        cwd: repoRoot,
      }).stdout;
      const allUntracked = untrackedRaw ? untrackedRaw.split('\0').filter(Boolean) : [];
      const { included: includedUntracked, skipped: skippedUntracked } = filterUntrackedPaths(
        allUntracked,
        { includeWork, excludes },
      );

      if (skippedUntracked.length > 0) {
        summary.skippedUntrackedCount = skippedUntracked.length;
        const roots = new Set(skippedUntracked.map((p) => p.split('/')[0]));
        summary.skippedUntrackedRoots = [...roots].sort();
      }

      if (!allowLargeUntracked && includedUntracked.length > 0) {
        let totalBytes = 0;
        for (const rel of includedUntracked) {
          const full = path.join(repoRoot, rel);
          let stat;
          try {
            stat = fs.lstatSync(full);
          } catch {
            continue;
          }
          if (!stat.isFile()) continue;
          if (stat.size > DEFAULT_MAX_SINGLE_UNTRACKED_BYTES) {
            throw new Error(
              `Refusing to sync large untracked file "${rel}" (${(stat.size / 1024 / 1024).toFixed(1)} MiB). ` +
                'Exclude it with --exclude <path>, build committed files with --committed, or pass --allow-large-untracked.',
            );
          }
          totalBytes += stat.size;
        }
        if (totalBytes > DEFAULT_MAX_TOTAL_UNTRACKED_BYTES) {
          throw new Error(
            `Refusing to sync ${(totalBytes / 1024 / 1024).toFixed(1)} MiB of untracked files (${includedUntracked.length} files). ` +
              'Exclude scratch folders with --exclude <path>, use --committed, or pass --allow-large-untracked.',
          );
        }
      }

      // 2. Build clean temp index from realHead (immune to racy-git stat cache issues).
      runGit(['read-tree', realHead], { cwd: repoRoot, env: stageEnv });
      runGit(['add', '-u'], { cwd: repoRoot, env: stageEnv });

      // 3. Include newly staged files (using --no-renames so copies/renames report as A) +
      //    included untracked files. Use lstatSync so dangling symlinks are preserved, and
      //    `git add -f` so force-staged ignored files succeed (untracked paths were already
      //    filtered via --exclude-standard).
      const stagedNewRaw = runGit(
        ['diff', '--cached', '--no-renames', '--name-only', '--diff-filter=A', '-z'],
        { cwd: repoRoot },
      ).stdout;
      const stagedNew = stagedNewRaw
        ? stagedNewRaw.split('\0').filter((p) => p && pathExistsOnDisk(path.join(repoRoot, p)))
        : [];
      const pathsToAdd = [...new Set([...stagedNew, ...includedUntracked])];
      if (pathsToAdd.length > 0) {
        runGit(['add', '-f', '--pathspec-from-file=-', '--pathspec-file-nul'], {
          cwd: repoRoot,
          env: stageEnv,
          input: `${pathsToAdd.join('\0')}\0`,
        });
      }

      const treeSha = runGit(['write-tree'], { cwd: repoRoot, env: stageEnv }).stdout.trim();
      if (treeSha !== headTree) {
        isDirty = true;
        targetSha = runGit(
          ['commit-tree', treeSha, '-p', realHead, '-m', 'boat-build-worktree'],
          { cwd: repoRoot, env: stageEnv },
        ).stdout.trim();

        const diffStats = runGit(
          ['diff-tree', '--no-commit-id', '--name-status', '-r', realHead, targetSha],
          { cwd: repoRoot, env: stageEnv },
        ).stdout;
        for (const line of diffStats.split(/\r?\n/)) {
          if (!line) continue;
          const code = line[0];
          if (code === 'A') summary.added += 1;
          else if (code === 'D') summary.deleted += 1;
          else summary.modified += 1;
        }
      }
    }

    runGit(['update-ref', 'refs/heads/boat-head', realHead], { cwd: repoRoot, env: stageEnv });
    runGit(['update-ref', 'refs/heads/boat-target', targetSha], { cwd: repoRoot, env: stageEnv });

    return {
      tempDir,
      stageGitDir,
      stageEnv,
      realHead,
      targetSha,
      isDirty,
      summary,
      cleanup() {
        fs.rmSync(tempDir, { recursive: true, force: true });
      },
    };
  } catch (err) {
    fs.rmSync(tempDir, { recursive: true, force: true });
    throw err;
  }
}

export function buildSeedUnpackSnippet(bundlePath, realHead) {
  return `
git bundle unbundle ${shellQuote(bundlePath)} >/dev/null
rm -f ${shellQuote(bundlePath)}
if git cat-file -p ${shellQuote(realHead)} | grep -q '^parent '; then
  _aw_missing_parent=0
  for _aw_p in $(git cat-file -p ${shellQuote(realHead)} | awk '/^parent / {print $2}'); do
    if ! git cat-file -e "$_aw_p^{commit}" 2>/dev/null; then
      _aw_missing_parent=1
      break
    fi
  done
  if [ "$_aw_missing_parent" -eq 1 ]; then
    touch .git/shallow
    grep -qxF ${shellQuote(realHead)} .git/shallow || printf '%s\\n' ${shellQuote(realHead)} >> .git/shallow
  fi
fi
git update-ref refs/boat/head ${shellQuote(realHead)}
`;
}

export function buildApplyScript({
  remoteDir = DEFAULT_REMOTE_DIR,
  needSeedReset,
  headAlreadyOnRemote,
  realHead,
  isDirty,
  uploadedBundle,
  seedBundlePath = '/tmp/aw-seed.bundle',
  syncBundlePath = '/tmp/aw-sync.bundle',
}) {
  const fetchHeadLine = needSeedReset
    ? buildSeedUnpackSnippet(seedBundlePath, realHead)
    : headAlreadyOnRemote
      ? `git update-ref refs/boat/head ${shellQuote(realHead)}`
      : `git fetch -q ${shellQuote(syncBundlePath)} refs/heads/boat-head:refs/boat/head --force`;

  const fetchTargetLine = isDirty
    ? `git fetch -q ${shellQuote(syncBundlePath)} refs/heads/boat-target:refs/boat/target --force`
    : 'git update-ref refs/boat/target refs/boat/head';

  return `
set -euo pipefail
cd ${shellQuote(remoteDir)}
${fetchHeadLine}
${fetchTargetLine}
${uploadedBundle ? `rm -f ${shellQuote(syncBundlePath)}` : ''}

TARGET_SHA="$(git rev-parse refs/boat/target)"
LAST_SHA="$(git rev-parse --verify -q refs/boat/last 2>/dev/null || true)"

if [ -n "$LAST_SHA" ] && git cat-file -e "$LAST_SHA^{tree}" 2>/dev/null; then
  if [ "$LAST_SHA" != "$TARGET_SHA" ]; then
    git symbolic-ref HEAD refs/heads/boat-build
    git update-ref refs/heads/boat-build "$LAST_SHA"
    git read-tree "$LAST_SHA"
    git update-index -q --refresh || true
    git read-tree -m -u "$LAST_SHA" "$TARGET_SHA" || git read-tree --reset -u "$TARGET_SHA"
  else
    git read-tree "$TARGET_SHA"
  fi
else
  git checkout -q -f -B boat-build "$TARGET_SHA"
fi

# Restore only tracked files modified or deleted remotely (preserving unchanged file mtimes).
git update-index -q --refresh || true
if ! git diff-files --quiet; then
  git diff-files --name-only -z | git checkout-index -f -z --stdin
fi

# Index is currently at TARGET_SHA (including synced untracked files), so git clean
# removes only stray non-ignored files while preserving .gitignore'd target/ & node_modules/.
git clean -fd -q

git update-ref refs/boat/last "$TARGET_SHA"
git symbolic-ref HEAD refs/heads/boat-build
git update-ref refs/heads/boat-build refs/boat/head
git read-tree refs/boat/head
`;
}

function wrapWithMutex(lockPath, body) {
  return `
set -eu
LOCK_STATE=${shellQuote(lockPath)}
LOCK_FLOCK="${'${LOCK_STATE}'}.flock"
LOCK_MUTEX="${'${LOCK_STATE}'}.mutex"
_aw_run_locked() {
${body}
}
if command -v flock >/dev/null 2>&1; then
  exec 9>"$LOCK_FLOCK"
  flock -x 9
  _aw_run_locked
else
  _aw_tries=0
  while ! mkdir "$LOCK_MUTEX" 2>/dev/null; do
    _aw_tries=$((_aw_tries + 1))
    if [ "$_aw_tries" -gt 200 ]; then
      echo "Timed out waiting for lock mutex" >&2
      exit 1
    fi
    sleep 0.05
  done
  trap 'rmdir "$LOCK_MUTEX" 2>/dev/null || true' EXIT
  _aw_run_locked
fi
`;
}

export function buildLockAcquireScript(
  lockPath,
  runId,
  { forceUnlock = false, allowStoppedLock = false } = {},
) {
  return wrapWithMutex(
    lockPath,
    `  CURRENT="$(cat "$LOCK_STATE" 2>/dev/null || true)"
  if [ -z "$CURRENT" ] || [ ${forceUnlock ? '1' : '0'} -eq 1 ]; then
    printf 'active:%s\\n' ${shellQuote(runId)} > "$LOCK_STATE"
    echo "LOCK_ACQUIRED=1"
    return 0
  fi
  if [ ${allowStoppedLock ? '1' : '0'} -eq 1 ]; then
    case "$CURRENT" in
      stopping:*)
        printf 'active:%s\\n' ${shellQuote(runId)} > "$LOCK_STATE"
        echo "LOCK_ACQUIRED=1"
        return 0
        ;;
    esac
  fi
  echo "LOCK_ACQUIRED=0"
  echo "LOCK_HOLDER=$CURRENT"`,
  );
}

export function buildLockMarkStoppingScript(lockPath, runId) {
  return wrapWithMutex(
    lockPath,
    `  CURRENT="$(cat "$LOCK_STATE" 2>/dev/null || true)"
  if [ "$CURRENT" = ${shellQuote(`active:${runId}`)} ]; then
    printf 'stopping:%s\\n' ${shellQuote(runId)} > "$LOCK_STATE"
  fi`,
  );
}

export function buildLockReleaseScript(lockPath, runId) {
  return wrapWithMutex(
    lockPath,
    `  CURRENT="$(cat "$LOCK_STATE" 2>/dev/null || true)"
  if [ "$CURRENT" = ${shellQuote(`active:${runId}`)} ]; then
    rm -f "$LOCK_STATE"
  fi`,
  );
}

function acquireRemoteLock(
  sandboxId,
  targetName,
  runId,
  { forceUnlock = false, allowStoppedLock = false } = {},
) {
  const out = runBoatExecCapture(
    sandboxId,
    buildLockAcquireScript(DEFAULT_REMOTE_LOCK_PATH, runId, { forceUnlock, allowStoppedLock }),
    { timeout: 60 },
  );
  let acquired = false;
  let holder = 'unknown';
  for (const rawLine of out.split(/\r?\n/)) {
    const line = rawLine.trim();
    if (line === 'LOCK_ACQUIRED=1') acquired = true;
    else if (line.startsWith('LOCK_HOLDER=')) holder = line.slice('LOCK_HOLDER='.length) || 'unknown';
  }
  if (!acquired) {
    throw new Error(
      `Sandbox ${targetName} (${sandboxId}) is currently locked by another run (${holder}). ` +
        'Wait for it to finish, pass --name <other> to use a separate sandbox, or pass --force-unlock if a previous run was killed.',
    );
  }
}

function markRemoteLockStopping(sandboxId, runId) {
  runBoatExecCapture(sandboxId, buildLockMarkStoppingScript(DEFAULT_REMOTE_LOCK_PATH, runId), {
    timeout: 30,
  });
}

function releaseRemoteLock(sandboxId, runId) {
  runBoatExecCapture(sandboxId, buildLockReleaseScript(DEFAULT_REMOTE_LOCK_PATH, runId), {
    timeout: 30,
  });
}

export function parseProbeOutput(out) {
  const info = {
    remoteHome: null,
    hasRepo: false,
    isShallow: false,
    remoteHead: null,
    remoteLast: null,
    remoteClean: false,
    recentCommits: [],
  };
  let inRecent = false;
  for (const rawLine of String(out || '').split(/\r?\n/)) {
    const line = rawLine.trim();
    if (line === 'RECENT_BEGIN') {
      inRecent = true;
      continue;
    }
    if (line === 'RECENT_END') {
      inRecent = false;
      continue;
    }
    if (inRecent) {
      if (/^[0-9a-f]{40}$/.test(line)) info.recentCommits.push(line);
      continue;
    }
    if (line.startsWith('REMOTE_HOME=')) info.remoteHome = line.slice('REMOTE_HOME='.length) || null;
    else if (line === 'HAS_REPO=1') info.hasRepo = true;
    else if (line === 'IS_SHALLOW=true') info.isShallow = true;
    else if (line.startsWith('REMOTE_HEAD=')) info.remoteHead = line.slice('REMOTE_HEAD='.length) || null;
    else if (line.startsWith('REMOTE_LAST=')) info.remoteLast = line.slice('REMOTE_LAST='.length) || null;
    else if (line === 'REMOTE_CLEAN=1') info.remoteClean = true;
  }
  return info;
}

function probeRemoteRepo(sandboxId) {
  const probeScript = `
set -eu
echo "REMOTE_HOME=$HOME"
if [ ! -d "$HOME/${DEFAULT_REMOTE_DIR}/.git" ]; then
  echo "HAS_REPO=0"
  exit 0
fi
cd "$HOME/${DEFAULT_REMOTE_DIR}"
echo "HAS_REPO=1"
echo "IS_SHALLOW=$(git rev-parse --is-shallow-repository 2>/dev/null || echo false)"
echo "REMOTE_HEAD=$(git rev-parse --verify HEAD 2>/dev/null || true)"
echo "REMOTE_LAST=$(git rev-parse --verify refs/boat/last 2>/dev/null || true)"
if [ -z "$(git status --porcelain 2>/dev/null || true)" ]; then
  echo "REMOTE_CLEAN=1"
else
  echo "REMOTE_CLEAN=0"
fi
echo "RECENT_BEGIN"
git rev-list -n 200 HEAD 2>/dev/null || true
echo "RECENT_END"
`;
  const out = runBoatExecCapture(sandboxId, probeScript, { timeout: 60 });
  return parseProbeOutput(out);
}

export function createStandaloneSeedBundle(repoRoot, realHead, tempDir) {
  const shallowDir = path.join(tempDir, 'shallow-seed');
  const bundlePath = path.join(tempDir, 'seed.bundle');
  fs.rmSync(shallowDir, { recursive: true, force: true });
  fs.mkdirSync(shallowDir, { recursive: true });
  // Init + depth-1 fetch handles normal branches, detached HEAD, and arbitrary --ref commits uniformly.
  runGit(['init', '-q'], { cwd: shallowDir });
  runGit(
    [
      'fetch',
      '--depth',
      '1',
      '--upload-pack=git -c uploadpack.allowAnySHA1InWant=true upload-pack',
      `file://${repoRoot}`,
      realHead,
    ],
    { cwd: shallowDir },
  );
  runGit(['update-ref', 'refs/heads/boat-head', 'FETCH_HEAD'], { cwd: shallowDir });
  runGit(['bundle', 'create', bundlePath, 'refs/heads/boat-head'], { cwd: shallowDir });
  return bundlePath;
}

function bootstrapRemoteRepo({
  sandboxId,
  remoteRepoPath,
  remoteSeedPath,
  publicHttpsUrl,
  repoRoot,
  realHead,
  tempDir,
}) {
  if (publicHttpsUrl) {
    console.log(`[boat] Cloning ${publicHttpsUrl} (depth 50) directly into sandbox ${sandboxId}...`);
    const cloneScript = `
set -euo pipefail
rm -rf ${shellQuote(remoteRepoPath)}
GIT_TERMINAL_PROMPT=0 git clone --depth 50 ${shellQuote(publicHttpsUrl)} ${shellQuote(remoteRepoPath)}
`;
    try {
      runBoatExecCapture(sandboxId, cloneScript, { timeout: 300 });
      return;
    } catch {
      console.log('[boat] Public clone unavailable (private repo or unreachable); falling back to local seed bundle...');
    }
  }

  console.log('[boat] Creating standalone 1-commit seed bundle...');
  const seedBundle = createStandaloneSeedBundle(repoRoot, realHead, tempDir);
  const sizeMiB = (fs.statSync(seedBundle).size / 1024 / 1024).toFixed(1);
  console.log(`[boat] Uploading seed bundle (${sizeMiB} MiB) to ${sandboxId}...`);
  runBoat(['scp', seedBundle, `${sandboxId}:${remoteSeedPath}`]);
  const initScript = `
set -euo pipefail
rm -rf ${shellQuote(remoteRepoPath)}
git init -q ${shellQuote(remoteRepoPath)}
cd ${shellQuote(remoteRepoPath)}
${buildSeedUnpackSnippet(remoteSeedPath, realHead)}
git checkout -q -f -B boat-build refs/boat/head
git update-ref refs/boat/last HEAD
`;
  runBoatExecCapture(sandboxId, initScript, { timeout: 300 });
}

export function findCommonBase(repoRoot, remoteCommits, realHead, { isShallow = false } = {}) {
  const remoteSet = new Set(remoteCommits);
  for (const sha of remoteCommits) {
    const exists = runGit(['cat-file', '-e', `${sha}^{commit}`], {
      cwd: repoRoot,
      allowFail: true,
    });
    if (exists.status !== 0) continue;
    const mb = runGit(['merge-base', sha, realHead], { cwd: repoRoot, allowFail: true });
    const candidate = mb.status === 0 ? mb.stdout.trim() : '';
    if (!/^[0-9a-f]{40}$/.test(candidate)) continue;
    // On shallow sandboxes, a merge-base older than the shallow boundary is absent remotely.
    if (isShallow && !remoteSet.has(candidate)) continue;
    return candidate;
  }
  return null;
}

function syncRepoToSandbox({ sandboxId, runId, localSync, gitInfo }) {
  const remoteSeedPath = `/tmp/aw-seed-${runId}.bundle`;
  const remoteSyncPath = `/tmp/aw-sync-${runId}.bundle`;

  let remote = probeRemoteRepo(sandboxId);
  if (!remote.remoteHome) {
    throw new Error(`Could not determine $HOME on sandbox ${sandboxId}.`);
  }
  const remoteRepoPath = path.posix.join(remote.remoteHome, DEFAULT_REMOTE_DIR);

  if (!remote.hasRepo) {
    bootstrapRemoteRepo({
      sandboxId,
      remoteRepoPath,
      remoteSeedPath,
      publicHttpsUrl: gitInfo.publicHttpsUrl,
      repoRoot: REPO_ROOT,
      realHead: localSync.realHead,
      tempDir: localSync.tempDir,
    });
    remote = probeRemoteRepo(sandboxId);
  }

  const { realHead, isDirty, summary } = localSync;
  const shortHead = realHead.slice(0, 10);
  const dirtyNote = isDirty
    ? ` (+${summary.modified}M +${summary.added}A +${summary.deleted}D uncommitted)`
    : ' (clean)';
  const skipNote =
    summary.skippedUntrackedCount > 0
      ? `; skipped ${summary.skippedUntrackedCount} untracked in ${summary.skippedUntrackedRoots.join(', ')}`
      : '';

  if (
    !isDirty &&
    remote.remoteHead === realHead &&
    remote.remoteLast === realHead &&
    remote.remoteClean
  ) {
    console.log(`[boat] Sync: already at ${shortHead} (0 B transferred${skipNote})`);
    return { remoteRepoPath };
  }

  const remoteCommitSet = new Set(remote.recentCommits);
  let base = findCommonBase(REPO_ROOT, remote.recentCommits, realHead, {
    isShallow: remote.isShallow,
  });
  let needSeedReset = false;
  let uploadedBundle = false;
  let headAlreadyOnRemote = false;

  // Verify that an incremental bundle against `base` only requires commits present on the sandbox.
  let incrementalBundlePath = null;
  if (base && (base !== realHead || isDirty)) {
    const candidatePath = path.join(localSync.tempDir, 'sync.bundle');
    const bundleRefs = [];
    if (base !== realHead) {
      bundleRefs.push('refs/heads/boat-head');
    }
    if (isDirty) {
      bundleRefs.push('refs/heads/boat-target');
    }
    runGit(['bundle', 'create', candidatePath, `^${base}`, ...bundleRefs], {
      cwd: REPO_ROOT,
      env: localSync.stageEnv,
    });
    const prereqs = parseBundlePrerequisites(candidatePath);
    if (remote.isShallow && prereqs.some((sha) => !remoteCommitSet.has(sha))) {
      // A merge commit or shallow boundary pulled in a prerequisite missing on the sandbox.
      fs.rmSync(candidatePath, { force: true });
      base = null;
    } else {
      incrementalBundlePath = candidatePath;
    }
  }

  // If shallow history on remote doesn't overlap local HEAD, try a fast 1-commit
  // cloud-side fetch from public origin before falling back to a local seed bundle.
  if (!base && gitInfo.publicHttpsUrl) {
    try {
      const fetchDirectScript = `
set -euo pipefail
cd ${shellQuote(remoteRepoPath)}
GIT_TERMINAL_PROMPT=0 git fetch --depth 1 ${shellQuote(gitInfo.publicHttpsUrl)} ${shellQuote(realHead)}
git update-ref refs/boat/head FETCH_HEAD
`;
      runBoatExecCapture(sandboxId, fetchDirectScript, { timeout: 120 });
      base = realHead;
      if (isDirty) {
        const dirtyPath = path.join(localSync.tempDir, 'sync.bundle');
        runGit(['bundle', 'create', dirtyPath, `^${realHead}`, 'refs/heads/boat-target'], {
          cwd: REPO_ROOT,
          env: localSync.stageEnv,
        });
        incrementalBundlePath = dirtyPath;
      }
    } catch {
      // Commit not on public origin (unpushed or private repo); fall through to local seed bundle.
    }
  }

  if (base && base === realHead && !isDirty) {
    headAlreadyOnRemote = true;
    console.log(`[boat] Sync: ${shortHead}${dirtyNote} (already present on sandbox, 0 B transferred${skipNote})`);
  } else if (base && incrementalBundlePath) {
    if (base === realHead) {
      headAlreadyOnRemote = true;
    }
    const bytes = fs.statSync(incrementalBundlePath).size;
    const sizeLabel =
      bytes < 1024 * 1024
        ? `${Math.max(1, Math.round(bytes / 1024))} KiB`
        : `${(bytes / 1024 / 1024).toFixed(1)} MiB`;
    console.log(`[boat] Sync: ${shortHead}${dirtyNote} (incremental bundle ${sizeLabel}${skipNote})`);
    runBoat(['scp', incrementalBundlePath, `${sandboxId}:${remoteSyncPath}`]);
    uploadedBundle = true;
  } else {
    needSeedReset = true;
    console.log(`[boat] Sync: ${shortHead}${dirtyNote} (diverged shallow history; uploading fresh seed bundle${skipNote})`);
    const seedBundle = createStandaloneSeedBundle(REPO_ROOT, realHead, localSync.tempDir);
    runBoat(['scp', seedBundle, `${sandboxId}:${remoteSeedPath}`]);
    if (isDirty) {
      const dirtyBundle = path.join(localSync.tempDir, 'dirty.bundle');
      runGit(['bundle', 'create', dirtyBundle, `^${realHead}`, 'refs/heads/boat-target'], {
        cwd: REPO_ROOT,
        env: localSync.stageEnv,
      });
      runBoat(['scp', dirtyBundle, `${sandboxId}:${remoteSyncPath}`]);
      uploadedBundle = true;
    }
  }

  const applyScript = buildApplyScript({
    remoteDir: remoteRepoPath,
    needSeedReset,
    headAlreadyOnRemote,
    realHead,
    isDirty,
    uploadedBundle,
    seedBundlePath: remoteSeedPath,
    syncBundlePath: remoteSyncPath,
  });
  runBoatExecCapture(sandboxId, applyScript, { timeout: 300 });
  return { remoteRepoPath };
}

function sleepMs(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

async function waitForSandboxSettled(sandboxId, label, timeoutMs = 90_000) {
  const deadline = Date.now() + timeoutMs;
  console.log(`[boat] Waiting for ${label} (${sandboxId}) to finish transitioning...`);
  while (Date.now() < deadline) {
    await sleepMs(2000);
    const raw = runBoat(['info', sandboxId, '--json']);
    const parsed = parseJsonOutput(raw);
    const state = parsed?.sandbox?.state || parsed?.state;
    if (!state || !TRANSITION_STATES.has(state)) {
      return state;
    }
  }
  throw new Error(`Timed out waiting for sandbox ${label} (${sandboxId}) to finish transitioning.`);
}

async function ensureSandbox({ targetName, baseName, type, typeExplicit, ttl, noFork, withEnv, keep }) {
  let sandboxes = listSandboxes();
  let plan = pickSandboxAction({ sandboxes, targetName, baseName, noFork });

  for (let attempt = 0; attempt < 3 && plan.action === 'wait-transition'; attempt += 1) {
    await waitForSandboxSettled(plan.sandbox.id, plan.sandbox.name);
    sandboxes = listSandboxes();
    plan = pickSandboxAction({ sandboxes, targetName, baseName, noFork });
  }
  if (plan.action === 'wait-transition') {
    throw new Error(
      `Sandbox ${plan.sandbox.name} (${plan.sandbox.id}) remained in state "${plan.sandbox.state}".`,
    );
  }

  if (plan.action === 'busy') {
    throw new Error(
      `Sandbox ${plan.sandbox.name} (${plan.sandbox.id}) is currently busy. ` +
        'Wait for it to finish or pass --name <other> to run in a separate sandbox.',
    );
  }

  if (plan.action === 'reuse-running') {
    if (typeExplicit && plan.sandbox.type && plan.sandbox.type !== type) {
      console.warn(
        `[boat] Warning: reusing running sandbox ${plan.sandbox.name} of type "${plan.sandbox.type}" (--type "${type}" applies on resume/new).`,
      );
    }
    console.log(`[boat] Reusing running sandbox ${plan.sandbox.name} (${plan.sandbox.id})`);
    return {
      sandboxId: plan.sandbox.id,
      created: false,
      forked: false,
      reusedRunning: true,
      resumed: false,
    };
  }

  if (plan.action === 'resume') {
    console.log(
      `[boat] Resuming stopped sandbox ${plan.sandbox.name} (${plan.sandbox.id})${typeExplicit ? ` as ${type}` : ''}...`,
    );
    const resumeArgs = ['resume', plan.sandbox.id, '--ttl', String(ttl), '--no-wait'];
    if (typeExplicit) {
      resumeArgs.push('--type', type);
    }
    runBoat(resumeArgs);
    return {
      sandboxId: plan.sandbox.id,
      created: false,
      forked: false,
      reusedRunning: false,
      resumed: true,
    };
  }

  if (plan.action === 'fork-base') {
    console.log(
      `[boat] Forking warm base ${plan.baseSandbox.name} (${plan.baseSandbox.id}) -> ${targetName} (${type})...`,
    );
    const forkArgs = [
      'fork',
      plan.baseSandbox.id,
      '--type',
      type,
      '--ttl',
      String(ttl),
      '--wait',
      '--json',
    ];
    if (!withEnv) {
      forkArgs.push('--no-env', '--no-monitoring');
    }
    const raw = runBoat(forkArgs);
    const created = parseJsonOutput(raw);
    if (!created?.id) {
      throw new Error(`Unexpected boat fork output: ${raw}`);
    }
    try {
      runBoat(['rename', created.id, targetName]);
    } catch (renameErr) {
      if (!keep) {
        try {
          stopSandbox(created.id, created.id);
        } catch {
          // Best-effort stop on rename failure
        }
      }
      throw renameErr;
    }
    console.log(`[boat] Ready: ${targetName} (${created.id})`);
    return { sandboxId: created.id, created: true, forked: true };
  }

  console.log(`[boat] Creating fresh sandbox ${targetName} (${type}, no-env=${!withEnv})...`);
  const newArgs = ['new', '--name', targetName, '--type', type, '--ttl', String(ttl), '--json'];
  if (!withEnv) {
    newArgs.push('--no-env', '--no-monitoring');
  }
  const raw = runBoat(newArgs);
  const created = parseJsonOutput(raw);
  if (!created?.id) {
    throw new Error(`Unexpected boat new output: ${raw}`);
  }
  console.log(`[boat] Ready: ${targetName} (${created.id})`);
  return { sandboxId: created.id, created: true, forked: false };
}

async function runRemoteSetup(sandboxId, { withDocsFonts = false } = {}) {
  const flags = withDocsFonts ? ['--with-docs-fonts'] : [];
  const cmd = `bash scripts/boat/setup.sh ${flags.join(' ')}`.trim();
  const code = await runBoat(
    ['exec', sandboxId, '--cwd', DEFAULT_REMOTE_DIR, '--timeout', '0', '--stream', '--', 'bash', '-lc', cmd],
    { stream: true },
  );
  if (code !== 0) {
    throw new Error(`Remote setup failed with exit code ${code}`);
  }
}

async function runRemoteCommand(sandboxId, remoteCmd, { timeout = 0 } = {}) {
  // Source env.sh without forcing `set -euo pipefail` onto arbitrary user commands.
  const wrapped = `
if [ -f "$HOME/.local/share/agent-wechat-boat/env.sh" ]; then
  # shellcheck disable=SC1091
  . "$HOME/.local/share/agent-wechat-boat/env.sh"
fi
${remoteCmd}
`;
  return runBoat(
    [
      'exec',
      sandboxId,
      '--cwd',
      DEFAULT_REMOTE_DIR,
      '--timeout',
      String(timeout),
      '--stream',
      '--',
      'bash',
      '-lc',
      wrapped,
    ],
    { stream: true },
  );
}

function stopSandbox(sandboxId, label) {
  console.log(`[boat] Stopping ${label} (${sandboxId}) to preserve cache at $0/hr...`);
  runBoat(['stop', sandboxId, '-y']);
}

function pullArtifacts(sandboxId, remoteRepoPath, pulls) {
  for (const { remotePath, localPath } of pulls) {
    const fullRemote = remotePath.startsWith('/')
      ? remotePath
      : path.posix.join(remoteRepoPath, remotePath);
    const fullLocal = path.resolve(process.cwd(), localPath);
    fs.mkdirSync(path.dirname(fullLocal), { recursive: true });
    console.log(`[boat] Pulling ${fullRemote} -> ${path.relative(process.cwd(), fullLocal) || fullLocal}`);
    runBoat(['scp', `${sandboxId}:${fullRemote}`, fullLocal]);
  }
}

async function main(argv = process.argv.slice(2)) {
  const opts = parseArgs(argv);
  if (opts.subcommand === 'help') {
    process.stdout.write(usageText());
    return 0;
  }

  const gitInfo = getLocalGitInfo(REPO_ROOT);
  const rustToolchain = parseRustToolchainChannel(
    fs.readFileSync(path.join(REPO_ROOT, 'rust-toolchain.toml'), 'utf8'),
  );
  const targetName =
    opts.name ||
    (opts.subcommand === 'seed'
      ? opts.baseName
      : deriveSandboxName({
          branch: gitInfo.branch,
          worktreePath: REPO_ROOT,
        }));

  if (opts.subcommand === 'status') {
    if (opts.dryRun) {
      console.log(JSON.stringify({ subcommand: 'status', targetName, baseName: opts.baseName }, null, 2));
      return 0;
    }
    const sandboxes = listSandboxes();
    const mine = sandboxes.filter((b) => b.name === targetName);
    const base = sandboxes.filter((b) => b.name === opts.baseName);
    const allAw = sandboxes.filter((b) => String(b.name || '').startsWith('aw-build-'));
    console.log(`Target sandbox name: ${targetName}`);
    console.log(
      `  Target: ${mine.length ? mine.map((b) => `${b.id} (${b.state}, ${b.type})`).join(', ') : 'none'}`,
    );
    console.log(
      `  Seed (${opts.baseName}): ${base.length ? base.map((b) => `${b.id} (${b.state}, ${b.type})`).join(', ') : 'none'}`,
    );
    if (allAw.length > 0) {
      console.log('All aw-build-* sandboxes:');
      for (const b of allAw) {
        console.log(`  ${b.name}\t${b.id}\t${b.state}\t${b.type}`);
      }
    }
    return 0;
  }

  if (opts.subcommand === 'stop' || opts.subcommand === 'delete') {
    if (opts.dryRun) {
      console.log(
        JSON.stringify({ subcommand: opts.subcommand, targetName, dryRun: true }, null, 2),
      );
      return 0;
    }
    const sandboxes = listSandboxes().filter((b) => b.name === targetName);
    if (sandboxes.length === 0) {
      console.log(`[boat] No sandbox named "${targetName}" found.`);
      return 0;
    }
    for (const box of sandboxes) {
      if (opts.subcommand === 'stop') {
        if (box.state === 'stopped') {
          console.log(`[boat] ${box.name} (${box.id}) is already stopped.`);
        } else {
          stopSandbox(box.id, box.name);
        }
      } else {
        console.log(`[boat] Deleting ${box.name} (${box.id})...`);
        runBoat(['delete', box.id, '--yes']);
      }
    }
    return 0;
  }

  let remoteCommand;
  if (opts.subcommand === 'seed') {
    opts.committed = true;
    opts.noFork = true;
    remoteCommand = opts.skipWarmBuild
      ? 'true'
      : `cargo +${rustToolchain} build --workspace --exclude wechat-protocol --all-targets --profile test --locked --manifest-path packages/native-client-rust/Cargo.toml`;
  } else {
    if (targetName === opts.baseName) {
      throw new Error(
        `Sandbox name "${targetName}" is reserved for the warm seed sandbox. ` +
          'Use `node scripts/boat/run.mjs seed` to update it, or choose another --name.',
      );
    }
    if (opts.preset && opts.command.length > 0) {
      throw new Error('Specify either --preset <name> or -- <command...>, not both.');
    }
    if (opts.preset) {
      remoteCommand = resolvePreset(opts.preset, rustToolchain);
    } else if (opts.command.length > 0) {
      remoteCommand =
        opts.command.length === 1 ? opts.command[0] : opts.command.map(shellQuote).join(' ');
    } else {
      throw new Error('Missing command. Pass --preset <name> or -- <command...>. Run with --help for examples.');
    }
  }

  const localSync = prepareLocalSync({
    repoRoot: REPO_ROOT,
    gitCommonDir: gitInfo.gitCommonDir,
    committed: opts.committed,
    ref: opts.ref,
    excludes: opts.excludes,
    includeWork: opts.includeWork,
    allowLargeUntracked: opts.allowLargeUntracked,
  });

  if (opts.dryRun) {
    try {
      console.log(
        JSON.stringify(
          {
            subcommand: opts.subcommand,
            targetName,
            baseName: opts.baseName,
            type: opts.type,
            withEnv: opts.withEnv,
            keep: opts.keep,
            committed: opts.committed,
            realHead: localSync.realHead,
            isDirty: localSync.isDirty,
            summary: localSync.summary,
            remoteCommand,
          },
          null,
          2,
        ),
      );
      return 0;
    } finally {
      localSync.cleanup();
    }
  }

  const runId = `${process.pid}-${randomBytes(4).toString('hex')}`;
  let sandboxId = null;
  let lockAcquired = false;
  let interruptedSignal = null;
  let cleanedUp = false;

  const finalize = () => {
    if (cleanedUp) return;
    cleanedUp = true;
    localSync.cleanup();
    if (!sandboxId || !lockAcquired) {
      // Never touch or stop a sandbox whose lock we did not acquire.
      return;
    }
    if (opts.keep) {
      try {
        releaseRemoteLock(sandboxId, runId);
      } catch (unlockErr) {
        console.error(`[boat] Warning: failed to release lock on ${sandboxId}: ${unlockErr.message}`);
      }
      return;
    }
    // Keep the lock held (marked `stopping:<runId>`) across `boat stop` so a concurrent
    // `reuse-running` run cannot slip in before stop completes; a later resume from `stopped`
    // atomically reclaims `stopping:*` (or finds /tmp cleared after a cold stop).
    try {
      markRemoteLockStopping(sandboxId, runId);
    } catch (markErr) {
      console.error(`[boat] Warning: failed to mark lock stopping on ${sandboxId}: ${markErr.message}`);
    }
    try {
      stopSandbox(sandboxId, targetName);
    } catch (stopErr) {
      console.error(`[boat] Warning: failed to stop ${sandboxId}: ${stopErr.message}`);
    }
  };

  const onSignal = (signal) => {
    if (interruptedSignal) {
      // Second signal: exit immediately without waiting for cleanup.
      process.exit(signal === 'SIGINT' ? 130 : 143);
    }
    interruptedSignal = signal;
    console.error(`\n[boat] Received ${signal}; cleaning up...`);
    if (activeStreamChild && !activeStreamChild.killed) {
      activeStreamChild.kill(signal);
    } else {
      finalize();
      process.exit(signal === 'SIGINT' ? 130 : 143);
    }
  };

  process.on('SIGINT', onSignal);
  process.on('SIGTERM', onSignal);

  try {
    const ensured = await ensureSandbox({
      targetName,
      baseName: opts.baseName,
      type: opts.type,
      typeExplicit: opts.typeExplicit,
      ttl: opts.ttl,
      noFork: opts.noFork,
      withEnv: opts.withEnv,
      keep: opts.keep,
    });
    sandboxId = ensured.sandboxId;

    acquireRemoteLock(sandboxId, targetName, runId, {
      forceUnlock: opts.forceUnlock,
      allowStoppedLock: Boolean(ensured.resumed || ensured.forked),
    });
    lockAcquired = true;

    if (ensured.reusedRunning) {
      runBoat(['extend', sandboxId, '--ttl', String(opts.ttl)]);
    }

    const { remoteRepoPath } = syncRepoToSandbox({ sandboxId, runId, localSync, gitInfo });

    if (!opts.skipSetup) {
      await runRemoteSetup(sandboxId, { withDocsFonts: opts.withDocsFonts });
    }

    console.log(`[boat] Running: ${remoteCommand}`);
    const exitCode = await runRemoteCommand(sandboxId, remoteCommand, { timeout: opts.timeout });

    if (exitCode === 0 && !interruptedSignal && opts.pulls.length > 0) {
      pullArtifacts(sandboxId, remoteRepoPath, opts.pulls);
    }

    if (interruptedSignal) {
      return interruptedSignal === 'SIGINT' ? 130 : 143;
    }
    return exitCode;
  } finally {
    process.off('SIGINT', onSignal);
    process.off('SIGTERM', onSignal);
    finalize();
  }
}

const isDirectRun =
  process.argv[1] && pathToFileURL(path.resolve(process.argv[1])).href === import.meta.url;

if (isDirectRun) {
  main()
    .then((code) => {
      process.exitCode = code;
    })
    .catch((err) => {
      console.error(`[boat] Error: ${err.message}`);
      process.exitCode = 1;
    });
}
