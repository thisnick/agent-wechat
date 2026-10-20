#!/usr/bin/env node
import { access } from 'node:fs/promises';
import { join } from 'node:path';
import { installNative, packageRoot, resolveNative } from './native-runtime.mjs';
try {
  if (process.env.AGENT_WECHAT_SKIP_NATIVE_INSTALL === '1') {
    console.error('Native installation skipped explicitly; provision wx before use.');
  } else {
    let workspace = false;
    try { await access(join(packageRoot, 'src/cli.ts')); workspace = true; } catch {}
    if (workspace && process.argv.includes('--postinstall')) {
      console.error('Source workspace: native release installation deferred. Build Rust wx explicitly.');
    } else if (process.env.AGENT_WECHAT_NATIVE_BINARY) {
      console.error(`Verified separately provisioned native runtime: ${await resolveNative()}`);
    } else {
      console.error(`Installed native runtime: ${await installNative({ artifactDir: process.env.AGENT_WECHAT_NATIVE_ARTIFACT_DIR })}`);
    }
  }
} catch (error) { console.error(error.message); process.exitCode = 1; }
