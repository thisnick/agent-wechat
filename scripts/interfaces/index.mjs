#!/usr/bin/env node
// `pnpm interfaces` regenerates every interface listing; `pnpm interfaces --check` fails
// when any listing differs from the code. Needs cargo (pinned toolchain) and pnpm install.
import { execFileSync } from 'node:child_process';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = join(dirname(fileURLToPath(import.meta.url)), '../..');
const check = process.argv.includes('--check');
const flag = check ? ['--check'] : [];
const run = (command, args, env = {}) => {
  console.log(`> ${command} ${args.join(' ')}`);
  execFileSync(command, args, { cwd: root, stdio: 'inherit', env: { ...process.env, ...env } });
};

const failures = [];
const step = (name, action) => {
  try { action(); } catch { failures.push(name); }
};

step('wx CLI', () => run('cargo', ['test', '--locked', '--quiet', '--manifest-path', 'packages/native-client-rust/Cargo.toml',
  '-p', 'wx-native', '--bin', 'wx', 'cli_help::interface_tests'], check ? {} : { AGENT_WECHAT_UPDATE_INTERFACES: '1' }));
step('Account and registry schema', () => run('cargo', ['test', '--locked', '--quiet', '--manifest-path', 'packages/native-client-rust/Cargo.toml',
  '-p', 'wechat-store', '--lib', 'schema_interface_tests'], check ? {} : { AGENT_WECHAT_UPDATE_INTERFACES: '1' }));
step('Rust crates', () => run('node', ['scripts/interfaces/rust-crates.mjs', ...flag]));
step('OpenClaw plugin', () => {
  run('pnpm', ['turbo', 'run', 'build', '--filter=@agent-wechat/agent-wechat', '--output-logs=errors-only']);
  run('node', ['packages/openclaw-extension/scripts/interface-listing.mjs', ...flag]);
});

if (failures.length) {
  console.error(`\nInterface listings ${check ? 'out of date' : 'failed'}: ${failures.join(', ')}.${check ? ' Run `pnpm interfaces` and commit the result.' : ''}`);
  process.exitCode = 1;
} else {
  console.log(check ? '\nAll interface listings match the code.' : '\nInterface listings updated. Review the diff under interfaces/.');
}
