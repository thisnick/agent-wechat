import { constants } from 'node:fs';
import { access, lstat, mkdir, readFile, readlink, realpath, stat, symlink } from 'node:fs/promises';
import { execFile } from 'node:child_process';
import { promisify } from 'node:util';
import { join, posix, win32 } from 'node:path';
import { packageRoot } from './native-runtime.mjs';

const exec = promisify(execFile);

function pathModule(platform) {
  return platform === 'win32' ? win32 : posix;
}

function pathDelimiter(platform) {
  return platform === 'win32' ? ';' : ':';
}

function envPathValue(env, platform) {
  if (platform !== 'win32') return env.PATH ?? '';
  for (const [key, value] of Object.entries(env)) {
    if (key.toLowerCase() === 'path' && typeof value === 'string') return value;
  }
  return '';
}

function splitPathEntries(env, platform) {
  const raw = envPathValue(env, platform);
  if (!raw) return [];
  return raw
    .split(pathDelimiter(platform))
    .map(entry => {
      const trimmed = entry.trim();
      return platform === 'win32' ? trimmed.replace(/^"+|"+$/g, '').trim() : trimmed;
    })
    .filter(Boolean);
}

function stripTrailingSeparators(value, pathMod) {
  const root = pathMod.parse(value).root;
  let current = value;
  while (current.length > root.length && (current.endsWith('/') || current.endsWith('\\'))) {
    current = current.slice(0, -1);
  }
  return current;
}

async function canonicalKey(targetPath, platform, { useRealpath = (platform === 'win32') === (process.platform === 'win32') } = {}) {
  const pathMod = pathModule(platform);
  let resolved = pathMod.resolve(targetPath);
  if (useRealpath) {
    try { resolved = await realpath(resolved); } catch {}
  }
  const normalized = stripTrailingSeparators(resolved, pathMod);
  return platform === 'win32' ? normalized.toLowerCase() : normalized;
}

async function existsLstat(targetPath) {
  try {
    return await lstat(targetPath);
  } catch {
    return null;
  }
}

function isNpxCachePath(dirPath, platform) {
  const parts = pathModule(platform).resolve(dirPath).split(/[/\\]+/);
  return parts.some(part => (platform === 'win32' ? part.toLowerCase() : part) === '_npx');
}

function isLocalNodeModulesBin(dirPath, platform) {
  const parts = stripTrailingSeparators(pathModule(platform).resolve(dirPath), pathModule(platform)).split(/[/\\]+/);
  if (parts.length < 2) return false;
  const parent = platform === 'win32' ? parts.at(-2)?.toLowerCase() : parts.at(-2);
  const leaf = platform === 'win32' ? parts.at(-1)?.toLowerCase() : parts.at(-1);
  return parent === 'node_modules' && leaf === '.bin';
}

function deriveNpmBinFromRoot(root, platform) {
  const pathMod = pathModule(platform);
  const resolved = stripTrailingSeparators(pathMod.resolve(root), pathMod);
  if (isNpxCachePath(resolved, platform)) return null;
  const parts = resolved.split(/[/\\]+/);
  if (parts.some(part => (platform === 'win32' ? part.toLowerCase() : part) === '.pnpm')) return null;
  if (platform === 'win32') {
    if (
      parts.length >= 4 &&
      parts.at(-3)?.toLowerCase() === 'node_modules' &&
      parts.at(-2)?.toLowerCase() === '@agent-wechat' &&
      parts.at(-1)?.toLowerCase() === 'cli'
    ) {
      return stripTrailingSeparators(pathMod.dirname(pathMod.dirname(pathMod.dirname(resolved))), pathMod);
    }
    return null;
  }
  if (parts.length >= 5 && parts.at(-4) === 'lib' && parts.at(-3) === 'node_modules' && parts.at(-2) === '@agent-wechat' && parts.at(-1) === 'cli') {
    const prefix = pathMod.dirname(pathMod.dirname(pathMod.dirname(pathMod.dirname(resolved))));
    return pathMod.join(prefix, 'bin');
  }
  return null;
}

function binNameFor(platform) {
  return platform === 'win32' ? 'wx.cmd' : 'wx';
}

function prefixToBinDir(prefix, platform) {
  const pathMod = pathModule(platform);
  const resolved = stripTrailingSeparators(pathMod.resolve(prefix), pathMod);
  return platform === 'win32' ? resolved : pathMod.join(resolved, 'bin');
}

export async function resolveGlobalBinDir({
  root = packageRoot,
  env = process.env,
  platform = process.platform,
  argv1 = process.argv[1],
  postinstall = false,
  exists = existsLstat,
} = {}) {
  const pathMod = pathModule(platform);
  const npmPrefix = env.npm_config_prefix?.trim() || env.npm_config_global_prefix?.trim();
  const prefixBin = npmPrefix ? prefixToBinDir(npmPrefix, platform) : null;
  const expectedBin = binNameFor(platform);

  // During npm postinstall, npm_config_prefix / npm_config_global_prefix identifies this install.
  if (postinstall && prefixBin) return prefixBin;

  // When invoked via a global bin symlink/shim (e.g. ~/.npm-global/bin/wx-native-install --link),
  // process.argv[1] preserves the bin folder path.
  if (argv1) {
    const resolvedArgv1 = pathMod.resolve(argv1);
    const base = pathMod.basename(resolvedArgv1).toLowerCase();
    const scriptsDir = stripTrailingSeparators(pathMod.join(pathMod.resolve(root), 'scripts'), pathMod);
    const argvDir = stripTrailingSeparators(pathMod.dirname(resolvedArgv1), pathMod);
    const matchesSelf = platform === 'win32'
      ? argvDir.toLowerCase() === scriptsDir.toLowerCase()
      : argvDir === scriptsDir;
    if (
      (base === 'wx-native-install' || base.startsWith('wx-native-install.')) &&
      !matchesSelf &&
      !isNpxCachePath(argvDir, platform) &&
      !isLocalNodeModulesBin(argvDir, platform)
    ) {
      return argvDir;
    }
  }

  // Standard npm global install layout (<prefix>/lib/node_modules/@agent-wechat/cli on POSIX,
  // <prefix>\node_modules\@agent-wechat\cli on Windows). On Windows, require wx.cmd in the
  // candidate directory so a local project's node_modules is not mistaken for a global prefix.
  const derivedNpmBin = deriveNpmBinFromRoot(root, platform);
  if (derivedNpmBin && (platform !== 'win32' || (await exists(pathMod.join(derivedNpmBin, expectedBin))))) {
    return derivedNpmBin;
  }

  // pnpm global install layout: check PNPM_HOME or an ancestor directory above `global/`.
  const pnpmHome = env.PNPM_HOME?.trim();
  if (pnpmHome) {
    const resolvedPnpmHome = stripTrailingSeparators(pathMod.resolve(pnpmHome), pathMod);
    if (await exists(pathMod.join(resolvedPnpmHome, expectedBin))) return resolvedPnpmHome;
  }

  const resolvedRoot = stripTrailingSeparators(pathMod.resolve(root), pathMod);
  let cursor = resolvedRoot;
  while (true) {
    const parent = pathMod.dirname(cursor);
    if (parent === cursor) break;
    if (pathMod.basename(cursor).toLowerCase() === 'global') {
      if (await exists(pathMod.join(parent, expectedBin))) return stripTrailingSeparators(parent, pathMod);
    }
    cursor = parent;
  }

  // Non-postinstall fallback (for example `npx -p @agent-wechat/cli wx-native-install --link`
  // or unit tests): only accept the npm prefix when it actually contains a `wx` launcher.
  if (prefixBin && (await exists(pathMod.join(prefixBin, expectedBin)))) return prefixBin;
  return null;
}

export async function isDirOnPath(dir, { env = process.env, platform = process.platform } = {}) {
  const targetKey = await canonicalKey(dir, platform);
  for (const entry of splitPathEntries(env, platform)) {
    if ((await canonicalKey(entry, platform)) === targetKey) return true;
  }
  return false;
}

export async function findWxOnPath({ env = process.env, platform = process.platform } = {}) {
  const pathMod = pathModule(platform);
  const entries = splitPathEntries(env, platform);
  if (platform === 'win32') {
    const pathextRaw = Object.entries(env).find(([key]) => key.toLowerCase() === 'pathext')?.[1] ?? '.COM;.EXE;.BAT;.CMD';
    const exts = pathextRaw
      .split(';')
      .map(ext => ext.trim())
      .filter(Boolean);
    for (const entry of entries) {
      for (const ext of exts) {
        for (const variant of new Set([ext.toLowerCase(), ext])) {
          const candidate = pathMod.join(entry, `wx${variant}`);
          try {
            const info = await stat(candidate);
            if (info.isFile()) return candidate;
          } catch {}
        }
      }
    }
    return null;
  }

  for (const entry of entries) {
    const candidate = pathMod.join(entry, 'wx');
    try {
      const info = await stat(candidate);
      if (!info.isFile()) continue;
      await access(candidate, constants.X_OK);
      return candidate;
    } catch {}
  }
  return null;
}

export async function probeWxVersion(commandPath, { platform = process.platform } = {}) {
  try {
    let stdout;
    if (platform === 'win32' && /\.(cmd|bat)$/i.test(commandPath)) {
      if (/["&|<>^%!\r\n]/.test(commandPath)) return null;
      const comspec = process.env.ComSpec || 'cmd.exe';
      ({ stdout } = await exec(comspec, ['/d', '/s', '/c', `""${commandPath}" --json version"`], {
        timeout: 3000,
        maxBuffer: 65536,
        windowsHide: true,
        windowsVerbatimArguments: true,
      }));
    } else {
      ({ stdout } = await exec(commandPath, ['--json', 'version'], {
        timeout: 3000,
        maxBuffer: 65536,
        windowsHide: true,
      }));
    }
    const parsed = JSON.parse(stdout);
    if (parsed?.product === 'agent-wechat' && typeof parsed.version === 'string' && parsed.version.length > 0) {
      return parsed.version;
    }
  } catch {}
  return null;
}

function shellArgPosix(value) {
  if (/^[A-Za-z0-9_./~-]+$/.test(value)) return value;
  return `'${value.replaceAll("'", "'\\''")}'`;
}

function escapeDoubleQuotedPosix(value) {
  return value.replaceAll('\\', '\\\\').replaceAll('"', '\\"').replaceAll('$', '\\$').replaceAll('`', '\\`');
}

export function shellProfileHint(binDir, { env = process.env, platform = process.platform } = {}) {
  if (platform === 'win32') {
    const escaped = binDir.replaceAll("'", "''");
    return {
      shell: 'powershell',
      file: null,
      line: `[Environment]::SetEnvironmentVariable('Path', ([Environment]::GetEnvironmentVariable('Path', 'User') + ';${escaped}').TrimStart(';'), 'User')`,
    };
  }
  const shellName = posix.basename(env.SHELL || '').toLowerCase();
  const escaped = escapeDoubleQuotedPosix(binDir);
  if (shellName === 'fish') {
    return {
      shell: 'fish',
      file: '~/.config/fish/config.fish',
      line: `fish_add_path ${shellArgPosix(binDir)}`,
    };
  }
  if (shellName === 'zsh' || (!shellName && platform === 'darwin')) {
    return {
      shell: 'zsh',
      file: '~/.zshrc',
      line: `export PATH="${escaped}:$PATH"`,
    };
  }
  if (shellName === 'bash') {
    return {
      shell: 'bash',
      file: platform === 'darwin' ? '~/.bash_profile' : '~/.bashrc',
      line: `export PATH="${escaped}:$PATH"`,
    };
  }
  return {
    shell: shellName || 'sh',
    file: '~/.profile',
    line: `export PATH="${escaped}:$PATH"`,
  };
}

function formatHintLines(binDir, hint, platform) {
  if (platform === 'win32') {
    return [
      `wx is installed in ${binDir}, which is not on PATH.`,
      'Add it to your user PATH in PowerShell and open a new terminal:',
      `  ${hint.line}`,
    ];
  }
  return [
    `wx is installed in ${binDir}, which is not on PATH.`,
    `Add this line to ${hint.file} and open a new terminal:`,
    `  ${hint.line}`,
  ];
}

async function resolveLauncherTarget(binDir, root, platform) {
  const pathMod = pathModule(platform);
  const binLauncher = pathMod.join(binDir, binNameFor(platform));
  if (await existsLstat(binLauncher)) return binLauncher;
  if (isNpxCachePath(root, platform)) return null;
  const packageLauncher = pathMod.join(pathMod.resolve(root), 'scripts', 'native-launcher.mjs');
  if (await existsLstat(packageLauncher)) return packageLauncher;
  return null;
}

async function isSameInstall(candidatePath, launcherTarget, root, platform) {
  const pathMod = pathModule(platform);
  const packageLauncher = pathMod.join(pathMod.resolve(root), 'scripts', 'native-launcher.mjs');
  const candidateKey = await canonicalKey(candidatePath, platform);
  if (launcherTarget && candidateKey === (await canonicalKey(launcherTarget, platform))) return true;
  if (candidateKey === (await canonicalKey(packageLauncher, platform))) return true;
  return false;
}

async function readPackageVersion(root) {
  const pkg = JSON.parse(await readFile(join(root, 'package.json'), 'utf8'));
  return pkg.version;
}

export async function ensureWxOnPath({
  root = packageRoot,
  env = process.env,
  platform = process.platform,
  argv1 = process.argv[1],
  postinstall = false,
  probeVersion = probeWxVersion,
  log = message => console.error(message),
} = {}) {
  const pathMod = pathModule(platform);
  const binDir = await resolveGlobalBinDir({ root, env, platform, argv1, postinstall });
  if (!binDir) {
    const message = 'Could not determine a global npm or pnpm bin folder for this @agent-wechat/cli installation.';
    if (!postinstall) log(message);
    return { action: 'no-bin-dir', binDir: null, messages: postinstall ? [] : [message] };
  }

  const messages = [];
  const emit = line => {
    messages.push(line);
    log(line);
  };

  // 1. If npm's bin folder is already on PATH, do nothing.
  if (await isDirOnPath(binDir, { env, platform })) {
    if (!postinstall) emit(`wx is already on PATH via ${binDir}.`);
    return { action: 'on-path', binDir, messages };
  }

  const launcherTarget = await resolveLauncherTarget(binDir, root, platform);

  // 2. If another wx is already on PATH, leave it alone; warn if its version differs.
  const existingOnPath = await findWxOnPath({ env, platform });
  if (existingOnPath) {
    if (await isSameInstall(existingOnPath, launcherTarget, root, platform)) {
      if (!postinstall) {
        emit(`wx is already linked at ${existingOnPath}${launcherTarget ? ` -> ${launcherTarget}` : ''}.`);
        if (platform !== 'win32') {
          emit(`To remove it later, run:\n  rm ${shellArgPosix(existingOnPath)}`);
        }
      }
      return { action: 'already-linked', binDir, linkPath: existingOnPath, target: launcherTarget, messages };
    }

    const packageVersion = await readPackageVersion(root);
    const existingVersion = await probeVersion(existingOnPath, { platform });
    const versionMismatch = existingVersion !== packageVersion;
    if (existingVersion && !versionMismatch) {
      emit(`Another wx (${existingVersion}) is already on PATH at ${existingOnPath}; leaving it unchanged.`);
    } else if (existingVersion) {
      emit(`Warning: another wx (${existingVersion}) is already on PATH at ${existingOnPath}, which differs from @agent-wechat/cli ${packageVersion}; leaving it unchanged.`);
      emit(`To use this install instead, put ${binDir} ahead of it on PATH, or run ${launcherTarget ?? pathMod.join(binDir, binNameFor(platform))}.`);
    } else {
      emit(`Warning: another wx is already on PATH at ${existingOnPath} (could not confirm it is agent-wechat ${packageVersion}); leaving it unchanged.`);
      emit(`To use this install instead, put ${binDir} ahead of it on PATH, or run ${launcherTarget ?? pathMod.join(binDir, binNameFor(platform))}.`);
    }
    return {
      action: 'other-on-path',
      binDir,
      existingPath: existingOnPath,
      existingVersion,
      packageVersion,
      versionMismatch,
      messages,
    };
  }

  const hint = shellProfileHint(binDir, { env, platform });

  // 3. On POSIX, if ~/.local/bin is on PATH and has no wx, link ~/.local/bin/wx to npm's wx launcher.
  // Follow the desktop install_cli rules: never replace an existing file or a symlink this installer did not create.
  if (platform !== 'win32' && env.HOME && launcherTarget) {
    const localBin = posix.join(env.HOME, '.local', 'bin');
    if (await isDirOnPath(localBin, { env, platform })) {
      const destination = posix.join(localBin, 'wx');
      const existing = await existsLstat(destination);
      if (existing) {
        if (existing.isSymbolicLink()) {
          const linkTarget = await readlink(destination);
          const resolvedLink = posix.resolve(localBin, linkTarget);
          if (resolvedLink === posix.resolve(launcherTarget)) {
            if (!postinstall) {
              emit(`wx is already linked at ${destination} -> ${launcherTarget}.`);
              emit(`To remove it later, run:\n  rm ${shellArgPosix(destination)}`);
            }
            return { action: 'already-linked', binDir, linkPath: destination, target: launcherTarget, messages };
          }
        }
        emit(`Another wx already exists at ${destination} and was not replaced.`);
        for (const line of formatHintLines(binDir, hint, platform)) emit(line);
        return { action: 'conflict', binDir, conflictPath: destination, hint, messages };
      }

      try {
        await mkdir(localBin, { recursive: true });
        await symlink(launcherTarget, destination);
        emit(`Linked wx into PATH: ${destination} -> ${launcherTarget}`);
        emit(`npm does not remove this link on uninstall. To remove it later, run:\n  rm ${shellArgPosix(destination)}`);
        return { action: 'linked', binDir, linkPath: destination, target: launcherTarget, messages };
      } catch (error) {
        if (error?.code === 'EEXIST') {
          emit(`Another wx already exists at ${destination} and was not replaced.`);
          for (const line of formatHintLines(binDir, hint, platform)) emit(line);
          return { action: 'conflict', binDir, conflictPath: destination, hint, messages };
        }
        emit(`Could not link ${destination}: ${error.message}`);
        for (const line of formatHintLines(binDir, hint, platform)) emit(line);
        return { action: 'link-failed', binDir, linkPath: destination, error: error.message, hint, messages };
      }
    }
  }

  // 4. Otherwise print the exact line to add to the shell profile.
  for (const line of formatHintLines(binDir, hint, platform)) emit(line);
  return { action: 'hint', binDir, hint, messages };
}
