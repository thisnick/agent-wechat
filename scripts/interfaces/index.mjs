#!/usr/bin/env node
// Regenerates or checks the interface listings under interfaces/.
//
//   pnpm interfaces                  regenerate the listings whose sources changed
//   pnpm interfaces cli http         regenerate only the named listings
//   pnpm interfaces --all            regenerate every listing
//   pnpm interfaces --check [...]    compare instead of writing (CI runs --check --all)
//   pnpm interfaces --list           show each listing and whether its sources changed
//
// "Changed" means changed since the merge base with origin/main (override with
// INTERFACES_BASE), including uncommitted and untracked files. Needs cargo with the
// pinned toolchain and `pnpm install`.
import { execFileSync } from 'node:child_process';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = join(dirname(fileURLToPath(import.meta.url)), '../..');
const rust = 'packages/native-client-rust/';
const rustShared = [`${rust}Cargo.toml`, `${rust}Cargo.lock`, 'rust-toolchain.toml'];
// The Rust listings are tests in these packages. They always run as one cargo invocation over the
// same package set: separate invocations resolve dependency features differently and rebuild them.
const rustPackages = ['wx-native', 'wechat-store', 'wechat-api'];

/** Each listing, the paths whose changes can affect it, and the commands that regenerate or check it. */
export const steps = [
  { name: 'cli', label: 'wx CLI', sources: [`${rust}crates/wx/`, 'interfaces/cli/', ...rustShared],
    test: 'cli_help::interface_tests' },
  { name: 'storage', label: 'Account and registry schema', sources: [`${rust}crates/wechat-store/`, 'interfaces/storage/', ...rustShared],
    test: 'schema_interface_tests' },
  { name: 'websocket', label: 'Calls WebSocket', sources: [`${rust}crates/wechat-api/`, `${rust}crates/wechat-core/`,
    'interfaces/http/calls-websocket.txt', 'interfaces/http/asyncapi.json', ...rustShared],
    test: 'calls_interface' },
  { name: 'http', label: 'HTTP API', sources: [`${rust}crates/wechat-api/`, `${rust}crates/wechat-core/`, `${rust}crates/wechat-store/`,
    'interfaces/http/routes.txt', 'interfaces/http/openapi.json', ...rustShared],
    test: 'http_interface' },
  { name: 'ui', label: 'UI routes', sources: ['packages/client-ui/', 'interfaces/ui/'],
    commands: check => [['node', ['packages/client-ui/scripts/interface-listing.mjs', ...(check ? ['--check'] : [])]]] },
  { name: 'crates', label: 'Rust crates', sources: ['wechat-platform', 'wechat-media', 'wechat-protocol', 'wechat-profile', 'wechat-calls']
    .map(crate => `${rust}crates/${crate}/`).concat('interfaces/crates/', 'scripts/native-crates.mjs', ...rustShared),
    commands: check => [['node', ['scripts/interfaces/rust-crates.mjs', ...(check ? ['--check'] : [])]]] },
  { name: 'openclaw', label: 'OpenClaw plugin', sources: ['packages/openclaw-extension/', 'packages/shared/', 'interfaces/openclaw/',
    'docs/src/content/docs/openclaw/tools.mdx', 'pnpm-lock.yaml'],
    commands: check => [['pnpm', ['turbo', 'run', 'build', '--filter=@agent-wechat/agent-wechat', '--output-logs=errors-only']],
      ['node', ['packages/openclaw-extension/scripts/interface-listing.mjs', ...(check ? ['--check'] : [])]]] },
];
// A change to the runner or generators themselves can affect every listing.
const everything = ['scripts/interfaces/', 'package.json'];

const git = args => execFileSync('git', args, { cwd: root, encoding: 'utf8', stdio: ['ignore', 'pipe', 'ignore'] }).split('\n').filter(Boolean);

/** Files changed since the merge base, plus uncommitted and untracked files; `null` if the base is unknown. */
export function changedFiles(base = process.env.INTERFACES_BASE ?? 'origin/main') {
  try {
    const mergeBase = git(['merge-base', base, 'HEAD'])[0];
    return [...new Set([...git(['diff', '--name-only', mergeBase]), ...git(['ls-files', '--others', '--exclude-standard'])])];
  } catch {
    return null;
  }
}

export const affects = (step, files) => files.some(file => [...step.sources, ...everything].some(path => file === path || file.startsWith(path)));

function main(argv) {
  const check = argv.includes('--check');
  const named = argv.filter(arg => !arg.startsWith('--'));
  const unknown = named.filter(name => !steps.some(step => step.name === name));
  if (unknown.length) {
    console.error(`Unknown listing: ${unknown.join(', ')}. Choose from: ${steps.map(step => step.name).join(', ')}.`);
    return 2;
  }
  const files = argv.includes('--all') || named.length ? null : changedFiles();
  if (argv.includes('--list')) {
    for (const step of steps) console.log(`${step.name.padEnd(10)} ${step.label.padEnd(30)} ${files === null ? '' : affects(step, files) ? 'changed' : 'unchanged'}`);
    return 0;
  }
  const selected = named.length ? steps.filter(step => named.includes(step.name))
    : files === null ? steps : steps.filter(step => affects(step, files));
  if (!argv.includes('--all') && !named.length && files === null) console.log('Could not find the merge base with origin/main; running every listing.');
  if (!selected.length) {
    console.log('No interface sources changed since the merge base with origin/main. Nothing to run (use --all to run everything).');
    return 0;
  }
  const skipped = steps.filter(step => !selected.includes(step));
  if (skipped.length && !named.length) console.log(`Skipping unchanged listings: ${skipped.map(step => step.name).join(', ')}.`);
  const started = process.hrtime.bigint();
  const results = [];
  const rustSteps = selected.filter(step => step.test);
  if (rustSteps.length) {
    const begin = process.hrtime.bigint();
    const args = ['test', '--locked', '--quiet', '--manifest-path', `${rust}Cargo.toml`, ...rustPackages.flatMap(name => ['-p', name]), '--', ...rustSteps.map(step => step.test)];
    console.log(`> cargo ${args.join(' ')}`);
    let ok = true;
    try {
      execFileSync('cargo', args, { cwd: root, stdio: 'inherit', env: { ...process.env, ...(check ? {} : { AGENT_WECHAT_UPDATE_INTERFACES: '1' }) } });
    } catch {
      ok = false;
    }
    const seconds = Number(process.hrtime.bigint() - begin) / 1e9;
    // One cargo run covers these listings; a failure names the failing test in cargo's output above.
    results.push({ step: { name: rustSteps.map(step => step.name).join('+'), label: rustSteps.map(step => step.label).join(', ') }, ok, seconds, names: rustSteps.map(step => step.name) });
  }
  for (const step of selected.filter(step => !step.test)) {
    const begin = process.hrtime.bigint();
    let ok = true;
    for (const [command, args, env = {}] of step.commands(check)) {
      console.log(`> ${command} ${args.join(' ')}`);
      try {
        execFileSync(command, args, { cwd: root, stdio: 'inherit', env: { ...process.env, ...env } });
      } catch {
        ok = false;
        break;
      }
    }
    results.push({ step, ok, seconds: Number(process.hrtime.bigint() - begin) / 1e9 });
  }
  const total = Number(process.hrtime.bigint() - started) / 1e9;
  console.log('');
  const width = Math.max(10, ...results.map(result => result.step.name.length));
  for (const { step, ok, seconds } of results) console.log(`${ok ? 'ok    ' : 'FAILED'} ${step.name.padEnd(width)} ${seconds.toFixed(1).padStart(6)}s  ${step.label}`);
  console.log(`       ${'total'.padEnd(width)} ${total.toFixed(1).padStart(6)}s`);
  const failed = results.filter(result => !result.ok).flatMap(result => result.names ?? [result.step.name]);
  if (failed.length) {
    console.error(`\nInterface listings ${check ? 'out of date' : 'failed'}: ${failed.join(', ')}.${check ? ` Run \`pnpm interfaces ${failed.join(' ')}\` and commit the result.` : ''}`);
    return 1;
  }
  console.log(check ? '\nThe checked listings match the code.' : '\nInterface listings updated. Review the diff under interfaces/.');
  return 0;
}

if (process.argv[1] && fileURLToPath(import.meta.url) === process.argv[1]) process.exitCode = main(process.argv.slice(2));
