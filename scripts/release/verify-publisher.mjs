import { spawnSync } from 'node:child_process';
import { mkdtemp, writeFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { isAbsolute, join } from 'node:path';
import { pathToFileURL } from 'node:url';

const npmVersion = '11.12.1';

export async function verifyCiPublisher({ env = process.env, spawn = spawnSync } = {}) {
  const npmPath = env.npm_config_npm_path;
  if (!npmPath || !isAbsolute(npmPath)) {
    throw new Error('The CI publisher must use an absolute npm_config_npm_path');
  }
  const version = spawn(npmPath, ['--version'], { env, encoding: 'utf8', timeout: 10_000 });
  if (version.status !== 0 || version.stdout?.trim() !== npmVersion) {
    throw new Error(`The pinned publisher must be npm ${npmVersion}`);
  }

  const dir = await mkdtemp(join(tmpdir(), 'agent-wechat-publisher-check-'));
  try {
    await writeFile(join(dir, 'package.json'), JSON.stringify({
      name: 'agent-wechat-ci-publisher-check', version: '0.0.0', files: ['package.json'],
    }));
    // Check pnpm's real subprocess, not merely the shell's npm. This is always
    // a dry run, with no lifecycle scripts or OIDC exchange for the fixture.
    const probeEnv = { ...env, npm_config_loglevel: 'verbose', npm_config_provenance: 'false',
      npm_config_fetch_retries: '0', npm_config_fetch_timeout: '5000' };
    delete probeEnv.ACTIONS_ID_TOKEN_REQUEST_URL;
    delete probeEnv.ACTIONS_ID_TOKEN_REQUEST_TOKEN;
    const probe = spawn('pnpm', ['publish', '--dry-run', '--no-git-checks', '--ignore-scripts', '--json'], {
      cwd: dir, env: probeEnv, encoding: 'utf8', timeout: 30_000,
    });
    const output = `${probe.stdout ?? ''}\n${probe.stderr ?? ''}`;
    const versions = [...output.matchAll(/npm info using npm@([^\s]+)/g)].map(match => match[1]);
    if (probe.status !== 0 || versions.length !== 1 || versions[0] !== npmVersion) {
      throw new Error(`pnpm's dry-run publisher did not verify as npm ${npmVersion} `
        + `(exit ${probe.status}, observed npm: ${versions.join(', ') || 'none'})`);
    }
    return npmVersion;
  } finally {
    await rm(dir, { recursive: true, force: true });
  }
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  await verifyCiPublisher();
  console.log(`Verified pnpm's publishing subprocess uses npm ${npmVersion}`);
}
