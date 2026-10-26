#!/usr/bin/env node
// Same-run CLI handoff to desktop; never downloads or publishes an executable.
import { execFileSync } from 'node:child_process';
import { chmod, mkdir, readFile, rm, writeFile } from 'node:fs/promises';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { executableName, inspectBinary, manifestName, targetFor, validateManifest, verifyBytes } from '../packages/cli/scripts/native-runtime.mjs';

const repository = resolve(import.meta.dirname, '..');
const metadataName = 'native-ci-build.json';

async function expectedBuild({ root = repository, compiler, sourceCommit, rustflags = process.env.RUSTFLAGS ?? '', target = targetFor() } = {}) {
  const { version } = JSON.parse(await readFile(join(root, 'packages/cli/package.json'), 'utf8'));
  const toolchain = (await readFile(join(root, 'rust-toolchain.toml'), 'utf8')).match(/^channel\s*=\s*"([^"]+)"/m)?.[1];
  compiler ??= execFileSync('rustc', ['--version'], { encoding: 'utf8' }).trim();
  if (!toolchain || !compiler.startsWith(`rustc ${toolchain} `)) throw new Error('CLI artifact compiler must match the pinned Rust toolchain');
  sourceCommit ??= execFileSync('git', ['rev-parse', 'HEAD'], { cwd: root, encoding: 'utf8' }).trim();
  return { version, sourceCommit, compiler, rustflags, target: target.rustTarget };
}

async function artifact(directory, expected, target) {
  const manifest = JSON.parse(await readFile(join(directory, manifestName(expected.version)), 'utf8'));
  const asset = validateManifest(manifest, expected.version, { target });
  if (manifest.sourceCommit !== expected.sourceCommit || manifest.sourceTreeDirty !== false) {
    throw new Error('CLI artifact must come from the same clean source commit');
  }
  const bytes = await readFile(join(directory, asset.name));
  verifyBytes(bytes, asset);
  return { asset, bytes };
}

export async function recordArtifact(directory, options = {}) {
  const expected = await expectedBuild(options);
  const { asset } = await artifact(directory, expected, options.target ?? targetFor());
  await writeFile(join(directory, metadataName), `${JSON.stringify({ schemaVersion: 1, ...expected, sha256: asset.sha256 }, null, 2)}\n`, { flag: 'wx' });
}

export async function restoreArtifact(directory, destination, { inspect = inspectBinary, ...options } = {}) {
  const target = options.target ?? targetFor();
  const expected = await expectedBuild({ ...options, target });
  const metadata = JSON.parse(await readFile(join(directory, metadataName), 'utf8'));
  const { asset, bytes } = await artifact(directory, expected, target);
  if (metadata.schemaVersion !== 1 || metadata.sha256 !== asset.sha256 ||
      Object.entries(expected).some(([key, value]) => metadata[key] !== value)) {
    throw new Error('CLI artifact source, compiler, target or build flags differ');
  }
  destination = resolve(destination);
  await mkdir(dirname(destination), { recursive: true });
  await writeFile(destination, bytes, { flag: 'wx', mode: 0o755 });
  try {
    await chmod(destination, 0o755);
    await inspect(destination, expected.version);
  } catch (error) {
    await rm(destination, { force: true });
    throw error;
  }
  return destination;
}

if (process.argv[1] && fileURLToPath(import.meta.url) === resolve(process.argv[1])) {
  const [operation, directory, ...args] = process.argv.slice(2);
  if (!directory) throw new Error('Usage: native-ci-artifact.mjs record|restore DIRECTORY [--compiler-file FILE]');
  if (operation === 'record') {
    const fileIndex = args.indexOf('--compiler-file');
    const compiler = fileIndex === -1 ? undefined : (await readFile(args[fileIndex + 1], 'utf8')).trim();
    await recordArtifact(resolve(directory), { compiler });
  } else if (operation === 'restore') {
    const target = targetFor();
    const destination = join(repository, 'packages/native-client-rust/target', target.rustTarget, 'release', executableName(target));
    console.log(await restoreArtifact(resolve(directory), destination));
  } else throw new Error(`Unknown CLI artifact operation: ${operation}`);
}
