#!/usr/bin/env node
// npm postinstall (or explicit offline provisioning) installs a verified runtime.
// The wx launcher only runs an already installed executable; it never downloads.
import { access } from 'node:fs/promises';
import { join } from 'node:path';
import { ensureWxOnPath } from './native-path.mjs';
import { installNative, packageRoot, resolveNative } from './native-runtime.mjs';

const usage = `Usage: wx-native-install [options]

Install the verified native wx executable for this platform, or link a global
install onto PATH when npm or pnpm's global bin folder is not on PATH.

Options:
  --link         Put wx on PATH (links ~/.local/bin/wx if on PATH, or prints the shell PATH line)
  --postinstall  Run npm postinstall flow (defers in source workspace; links on global installs)
  -h, --help     Show this help message

Environment:
  AGENT_WECHAT_NATIVE_ARTIFACT_DIR  Absolute directory with offline release assets and checksums
  AGENT_WECHAT_NATIVE_BINARY        Absolute path to a separately provisioned wx executable
  AGENT_WECHAT_SKIP_NATIVE_INSTALL  Set to 1 to skip native runtime installation`;

try {
  const args = process.argv.slice(2);
  const knownFlags = new Set(['--link', '--postinstall', '--help', '-h']);
  const unknown = args.filter(arg => !knownFlags.has(arg));
  if (unknown.length > 0) {
    console.error(`Unknown option: ${unknown.join(', ')}\n\n${usage}`);
    process.exitCode = 1;
  } else if (args.includes('--help') || args.includes('-h')) {
    console.log(usage);
  } else if (args.includes('--link') && !args.includes('--postinstall')) {
    const result = await ensureWxOnPath({ postinstall: false });
    if (result.action === 'no-bin-dir' || result.action === 'link-failed') process.exitCode = 1;
  } else {
    const isPostinstall = args.includes('--postinstall');
    let runtimeReady = false;
    if (process.env.AGENT_WECHAT_SKIP_NATIVE_INSTALL === '1') {
      console.error('Native installation skipped explicitly; provision wx before use.');
    } else {
      let workspace = false;
      try { await access(join(packageRoot, '../../pnpm-workspace.yaml')); workspace = true; } catch {}
      if (workspace && isPostinstall) {
        console.error('Source workspace: native release installation deferred. Build Rust wx explicitly.');
      } else if (process.env.AGENT_WECHAT_NATIVE_BINARY) {
        console.error(`Verified separately provisioned native runtime: ${await resolveNative()}`);
        runtimeReady = true;
      } else {
        console.error(`Installed native runtime: ${await installNative({ artifactDir: process.env.AGENT_WECHAT_NATIVE_ARTIFACT_DIR })}`);
        runtimeReady = true;
      }
    }
    if (runtimeReady && (args.includes('--link') || (isPostinstall && process.env.npm_config_global === 'true'))) {
      if (isPostinstall && !args.includes('--link')) {
        try {
          await ensureWxOnPath({ postinstall: true });
        } catch (error) {
          console.error(`Warning: could not update PATH for wx: ${error.message}`);
        }
      } else {
        const result = await ensureWxOnPath({ postinstall: false });
        if (result.action === 'no-bin-dir' || result.action === 'link-failed') process.exitCode = 1;
      }
    }
  }
} catch (error) { console.error(error.message); process.exitCode = 1; }
