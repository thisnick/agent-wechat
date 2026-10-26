import { createHash, createPublicKey, verify } from "node:crypto";
import { readFile, writeFile } from "node:fs/promises";
import { join } from "node:path";
export const updaterFiles = {
  "macos-arm64": ["darwin-aarch64-app", "macos-arm64.app.tar.gz"],
  "windows-x64": ["windows-x86_64-nsis", "windows-x64-setup.exe"],
  "windows-arm64": ["windows-aarch64-nsis", "windows-arm64-setup.exe"],
  "linux-x64": ["linux-x86_64-deb", "linux-x64.deb"],
  "linux-arm64": ["linux-aarch64-deb", "linux-arm64.deb"],
};
// Match the updater's Minisign format, including its signed version comment.
// The runtime independently verifies this signature before installing anything.
export function verifyUpdate(bytes, signature, publicKey, version) {
  const keyLines = Buffer.from(publicKey, "base64")
    .toString("utf8")
    .trim()
    .split(/\r?\n/);
  const key = Buffer.from(keyLines[1] || "", "base64");
  const lines = Buffer.from(signature, "base64")
    .toString("utf8")
    .trim()
    .split(/\r?\n/);
  const raw = Buffer.from(lines[1] || "", "base64"),
    global = Buffer.from(lines[3] || "", "base64");
  const comment = lines[2]?.replace(/^trusted comment: /, "");
  if (
    key.length !== 42 ||
    raw.length !== 74 ||
    global.length !== 64 ||
    raw.subarray(0, 2).toString() !== "ED" ||
    !key.subarray(2, 10).equals(raw.subarray(2, 10)) ||
    comment?.split("\t").find((field) => field.startsWith("version:")) !==
      `version:${version}`
  )
    throw new Error("Invalid updater signature or signed version");
  const edKey = createPublicKey({
    key: Buffer.concat([
      Buffer.from("302a300506032b6570032100", "hex"),
      key.subarray(10),
    ]),
    format: "der",
    type: "spki",
  });
  if (
    !verify(
      null,
      createHash("blake2b512").update(bytes).digest(),
      edKey,
      raw.subarray(10)
    ) ||
    !verify(
      null,
      Buffer.concat([raw.subarray(10), Buffer.from(comment)]),
      edKey,
      global
    )
  )
    throw new Error("Updater signature verification failed");
}
export async function updaterManifest(directory, version, publicKey) {
  const platforms = {};
  for (const [target, suffix] of Object.values(updaterFiles)) {
    const name = `AgentWechat-v${version}-${suffix}`;
    const signature = (
      await readFile(join(directory, name + ".sig"), "utf8")
    ).trim();
    verifyUpdate(
      await readFile(join(directory, name)),
      signature,
      publicKey,
      version
    );
    platforms[target] = {
      url: `https://github.com/thisnick/agent-wechat/releases/download/v${version}/${name}`,
      signature,
    };
  }
  const manifest = { version, platforms };
  await writeFile(
    join(directory, "desktop-updater.json"),
    JSON.stringify(manifest, null, 2) + "\n"
  );
  return manifest;
}
