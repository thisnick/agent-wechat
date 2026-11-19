#!/usr/bin/env node
// Records the UI journeys in journeys/ as videos against a fresh simulator hub, then
// encodes each recording for review:
//
//   .videos/out/<journey>.mp4         H.264 video for people
//   .videos/out/<journey>.gif         animated preview that GitHub shows inline
//   .videos/out/<journey>-frames.png  contact sheet of evenly spaced frames, for
//                                     checking a recording without playing it
//   .videos/out/manifest.json         what was recorded, for CI's PR comment
//
// Usage: node scripts/videos.mjs [journey ...] [playwright test options, as --option=value]
// With no names every journey is recorded. A bare word is a journey name, so give
// Playwright options their values after "=", for example --repeat-each=2. Needs ffmpeg and ffprobe on PATH as well
// as the scene prerequisites in scripts/harness.mjs. Nothing is uploaded from here.
import { spawnSync } from "node:child_process";
import { statSync } from "node:fs";
import { mkdir, readdir, readFile, rm, stat, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { elapsed, packageDir, playwright, prepare } from "./harness.mjs";

const started = Date.now();
const rawDir = join(packageDir, ".videos/raw");
const outDir = join(packageDir, ".videos/out");
/** GitHub proxies inline images and gives up on large ones; previews stay below this. */
const PREVIEW_LIMIT = 5 * 1024 * 1024;
/** Preview sizes to try, largest first: maximum width in pixels and frames per second. */
const PREVIEW_STEPS = [
  { width: 960, fps: 10 },
  { width: 800, fps: 8 },
  { width: 640, fps: 6 },
];
const FRAMES = { columns: 4, rows: 3, width: 480 };

const args = process.argv.slice(2);
if (args[0] === "--") args.shift();
const names = args.filter((arg) => !arg.startsWith("-") && /^[a-z0-9]+(-[a-z0-9]+)*$/.test(arg));
const passThrough = args.filter((arg) => !names.includes(arg));

function tool(command, toolArgs) {
  const result = spawnSync(command, toolArgs, { encoding: "utf8", maxBuffer: 16 * 1024 * 1024 });
  if (result.error) throw new Error(`${command}: ${result.error.message}`);
  if (result.status !== 0) throw new Error(`${command} failed:\n${result.stderr.trim()}`);
  return result.stdout;
}

for (const command of ["ffmpeg", "ffprobe"]) {
  if (spawnSync(command, ["-version"], { stdio: "ignore" }).error) {
    console.error(`${command} is not on PATH. Install FFmpeg first (Debian/Ubuntu: apt-get install ffmpeg; macOS: brew install ffmpeg).`);
    process.exit(1);
  }
}

const prepared = await prepare();
if (prepared !== 0) process.exit(prepared);

await rm(rawDir, { recursive: true, force: true });
await rm(outDir, { recursive: true, force: true });
const code = await playwright("journeys/playwright.config.mjs", passThrough, {
  ...process.env,
  JOURNEYS: names.join(","),
});

/** The animated preview: the largest step that fits PREVIEW_LIMIT. Reports the width it has. */
function encodePreview(input, output, sourceWidth) {
  for (const { width, fps } of PREVIEW_STEPS) {
    tool("ffmpeg", [
      "-v", "error", "-y", "-i", input,
      "-filter_complex",
      `fps=${fps},scale='min(${width},iw)':-2:flags=lanczos,split[a][b];` +
        "[a]palettegen=max_colors=192:stats_mode=diff[p];[b][p]paletteuse=dither=bayer:bayer_scale=4:diff_mode=rectangle",
      "-loop", "0", output,
    ]);
    if (statSync(output).size <= PREVIEW_LIMIT) return { width: Math.min(width, sourceWidth), fps };
  }
  const { width, fps } = PREVIEW_STEPS.at(-1);
  return { width: Math.min(width, sourceWidth), fps, oversized: true };
}

/** A grid of evenly spaced frames, each stamped with its time. */
function encodeFrames(input, output, seconds) {
  const count = FRAMES.columns * FRAMES.rows;
  const grid = `scale='min(${FRAMES.width},iw)':-2,`;
  const tile = `tile=${FRAMES.columns}x${FRAMES.rows}:padding=6:margin=6:color=white`;
  const select = `fps=${count}/${Math.max(seconds, 0.1).toFixed(3)},`;
  const stamp = "drawtext=text='%{pts\\:hms}':x=8:y=8:fontsize=16:fontcolor=white:box=1:boxcolor=black@0.65:boxborderw=4,";
  try {
    tool("ffmpeg", ["-v", "error", "-y", "-i", input, "-vf", select + grid + stamp + tile, "-frames:v", "1", output]);
  } catch {
    // drawtext needs a font from fontconfig; without one, leave the frames unstamped.
    tool("ffmpeg", ["-v", "error", "-y", "-i", input, "-vf", select + grid + tile, "-frames:v", "1", output]);
  }
}

const manifest = [];
await mkdir(outDir, { recursive: true });
const recorded = (await readdir(rawDir).catch(() => [])).filter((file) => file.endsWith(".json")).sort();
for (const file of recorded) {
  const metadata = JSON.parse(await readFile(join(rawDir, file), "utf8"));
  const input = join(rawDir, `${metadata.name}.webm`);
  const seconds = Number(tool("ffprobe", ["-v", "error", "-show_entries", "format=duration", "-of", "csv=p=0", input]).trim()) || metadata.durationMs / 1000;
  const mp4 = `${metadata.name}.mp4`;
  const preview = `${metadata.name}.gif`;
  const frames = `${metadata.name}-frames.png`;
  tool("ffmpeg", [
    "-v", "error", "-y", "-i", input,
    "-vf", "scale=trunc(iw/2)*2:trunc(ih/2)*2",
    "-c:v", "libx264", "-preset", "medium", "-crf", "23", "-pix_fmt", "yuv420p", "-movflags", "+faststart", "-an",
    join(outDir, mp4),
  ]);
  const previewSize = encodePreview(input, join(outDir, preview), metadata.size.width);
  encodeFrames(input, join(outDir, frames), seconds);
  const bytes = async (name) => (await stat(join(outDir, name))).size;
  manifest.push({
    name: metadata.name,
    description: metadata.description,
    viewport: metadata.viewport,
    size: metadata.size,
    seconds: Math.round(seconds * 10) / 10,
    chapters: metadata.chapters,
    files: {
      mp4: { name: mp4, bytes: await bytes(mp4) },
      preview: { name: preview, bytes: await bytes(preview), ...previewSize },
      frames: { name: frames, bytes: await bytes(frames) },
    },
  });
}
await writeFile(join(outDir, "manifest.json"), `${JSON.stringify(manifest, null, 2)}\n`);

const mb = (value) => `${(value / 1024 / 1024).toFixed(1)} MB`;
for (const entry of manifest) {
  console.log(`\n${entry.name} (${entry.seconds}s): ${entry.description}`);
  console.log(`  video   ${join(outDir, entry.files.mp4.name)} (${mb(entry.files.mp4.bytes)})`);
  console.log(`  preview ${join(outDir, entry.files.preview.name)} (${mb(entry.files.preview.bytes)}, ${entry.files.preview.width}px at ${entry.files.preview.fps} fps${entry.files.preview.oversized ? ", over the inline limit" : ""})`);
  console.log(`  frames  ${join(outDir, entry.files.frames.name)}`);
}
if (!manifest.length) console.log("\nNo journey finished recording.");
console.log(`\nVideos ${code === 0 ? "finished" : "failed"} in ${elapsed(Date.now() - started)} (including builds).`);
process.exit(code || (manifest.length ? 0 : 1));
