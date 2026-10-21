// The npm package ships a small Node launcher; the CLI itself is compiled Rust.
import { readdir } from 'node:fs/promises';
import { execFileSync } from 'node:child_process';
for (const file of await readdir(new URL('./scripts/', import.meta.url))) {
  if (file.endsWith('.mjs')) execFileSync(process.execPath, ['--check', new URL(`./scripts/${file}`, import.meta.url).pathname], { stdio: 'inherit' });
}
