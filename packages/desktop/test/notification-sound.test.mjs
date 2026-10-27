import test from "node:test";
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
const root = new URL("../src-tauri/", import.meta.url);
test("desktop bundles Soft pair at the native platform resource paths", async () => {
  const config = JSON.parse(await readFile(new URL("tauri.conf.json", root)));
  const linux = JSON.parse(
    await readFile(new URL("tauri.linux.conf.json", root)),
  );
  assert.equal(
    config.bundle.resources["sounds/AgentWechatSoftPair.wav"],
    "AgentWechatSoftPair.wav",
  );
  assert.equal(
    linux.bundle.resources["sounds/AgentWechatSoftPair.wav"],
    "sounds/AgentWechatSoftPair.wav",
  );
  const audio = await readFile(new URL("sounds/AgentWechatSoftPair.wav", root));
  assert.equal(audio.toString("ascii", 0, 4), "RIFF");
  assert.equal(audio.toString("ascii", 8, 12), "WAVE");
  assert.equal(audio.readUInt16LE(20), 1);
  assert.equal(audio.readUInt16LE(22), 1);
  assert.equal(audio.readUInt32LE(24), 48000);
  assert.equal(audio.readUInt16LE(34), 16);
  assert.equal(audio.length - 44, 48000 * 0.36 * 2);
});
