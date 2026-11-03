// Stage the installed public runtime dependency tree into an isolated plugin.
// No package manager, network install, workspace/dev tree or optional peer
// package is needed by these fixtures.
import { cp, mkdir, readFile, realpath } from 'node:fs/promises';
import { createRequire } from 'node:module';
import { dirname, join, relative, sep } from 'node:path';

export async function stagePluginRuntimeDependencies(root, plugin) {
  const manifestPath = join(root, 'packages/openclaw-extension/package.json');
  const manifest = JSON.parse(await readFile(manifestPath, 'utf8'));
  await stage(manifest.dependencies ?? {}, manifestPath, join(plugin, 'node_modules'), new Set());
}

async function stage(dependencies, fromManifest, modules, ancestors) {
  for (const [name, spec] of Object.entries(dependencies)) {
    if (!/^(?:@[a-z0-9._-]+\/)?[a-z0-9][a-z0-9._-]*$/i.test(name)
        || typeof spec !== 'string' || /^(?:workspace:|file:|link:|git|https?:|npm:)/.test(spec)) {
      throw Error('Smoke plugin requires installed registry runtime dependencies');
    }
    const require = createRequire(fromManifest);
    let directory = dirname(require.resolve(name));
    let dependency;
    // Resolve from the owning package rather than the repository's dev tree.
    while (directory !== dirname(directory)) {
      try {
        const candidate = JSON.parse(await readFile(join(directory, 'package.json'), 'utf8'));
        if (candidate.name === name) { dependency = candidate; break; }
      } catch (error) {
        if (error.code !== 'ENOENT') throw error;
      }
      directory = dirname(directory);
    }
    if (!dependency) throw Error(`Installed smoke runtime dependency not found: ${name}`);
    const source = await realpath(directory);
    if (ancestors.has(source)) throw Error('Unsupported cyclic smoke runtime dependency');
    if (/^\d+\.\d+\.\d+(?:[-+].+)?$/.test(spec) && dependency.version !== spec) {
      throw Error(`Installed smoke runtime version mismatch: ${name}`);
    }
    const destination = join(modules, name);
    await mkdir(dirname(destination), { recursive: true });
    await cp(source, destination, {
      recursive: true, force: false, errorOnExist: true,
      filter: path => !relative(source, path).split(sep).some(part => ['node_modules', '.git', '.npmrc'].includes(part)),
    });
    await stage(dependency.dependencies ?? {}, join(source, 'package.json'),
      join(destination, 'node_modules'), new Set([...ancestors, source]));
  }
}
