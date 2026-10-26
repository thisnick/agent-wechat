// Tauri's bundle marker must be set before OS signing. Later bundling uses
// --no-binary-patching to preserve signatures. Matches tauri-bundler's patch_binary.
import { readFile, writeFile } from "node:fs/promises";
import { resolve } from "node:path";
export function bundleType(bytes, type) {
  const marker = Buffer.from("__TAURI_BUNDLE_TYPE_VAR_UNK");
  const replacement = {
    deb: "__TAURI_BUNDLE_TYPE_VAR_DEB",
    nsis: "__TAURI_BUNDLE_TYPE_VAR_NSS",
  }[type];
  if (!replacement) throw new Error("Unsupported desktop bundle type");
  const offset = bytes.indexOf(marker);
  if (offset < 0 || bytes.indexOf(marker, offset + 1) >= 0)
    throw new Error("Expected one unsigned Tauri bundle marker");
  const output = Buffer.from(bytes);
  output.write(replacement, offset, "ascii");
  return output;
}
if (process.argv[1] && resolve(process.argv[1]) === import.meta.filename)
  await writeFile(
    process.argv[2],
    bundleType(await readFile(process.argv[2]), process.argv[3])
  );
