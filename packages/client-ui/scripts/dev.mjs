// Local development only. Owns synthetic wx-dev processes; never discovers a wx daemon.
import { spawn } from "node:child_process";
import { readFile, writeFile, mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { resolve, dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { randomBytes, timingSafeEqual } from "node:crypto";
import { createInterface } from "node:readline";
import { createServer } from "vite";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const repository = resolve(root, "../..");
const supplied = process.env.WX_DEV_BINARY;
if (!supplied)
  await new Promise((done, reject) => {
    const child = spawn(
      "cargo",
      [
        "build",
        "--profile",
        "test",
        "--locked",
        "--manifest-path",
        join(repository, "packages/native-client-rust/Cargo.toml"),
        "-p",
        "wechat-simulator",
      ],
      { stdio: "inherit" },
    );
    child.once("error", reject);
    child.once("exit", (code) =>
      code === 0 ? done() : reject(new Error("Simulator build failed")),
    );
  });
const binary =
  supplied ??
  join(
    repository,
    "packages/native-client-rust/target/debug",
    process.platform === "win32" ? "wx-dev.exe" : "wx-dev",
  );
const dataArg = process.argv.indexOf("--data-dir");
const data =
  dataArg >= 0
    ? resolve(process.argv[dataArg + 1])
    : await mkdtemp(join(tmpdir(), "agentwechat-ui-"));
const temporary = dataArg < 0;
const children = [];
let vite, hub, upstream, hubToken, controlToken, stopping, clockTimer;
let clockWork = Promise.resolve(),
  advancing = false;
let secret = randomBytes(32).toString("hex");
const equal = (value) =>
  typeof value === "string" &&
  /^[0-9a-f]{64}$/.test(value) &&
  timingSafeEqual(Buffer.from(value), Buffer.from(secret));
async function start(command, args = []) {
  const child = spawn(binary, [command, "--data-dir", data, ...args], {
    stdio: ["ignore", "pipe", "pipe"],
  });
  children.push(child);
  child.stderr.pipe(process.stderr);
  return await new Promise((done, reject) => {
    const lines = createInterface({ input: child.stdout });
    const timer = setTimeout(
      () => reject(new Error("Simulator startup timed out")),
      30000,
    );
    child.once("error", reject);
    child.once("exit", () => {
      clearTimeout(timer);
      reject(new Error("Simulator stopped"));
      if (!stopping) void stop(1);
    });
    lines.once("line", (line) => {
      clearTimeout(timer);
      try {
        const info = JSON.parse(line);
        if (info.state !== "ready") throw new Error();
        done(info);
      } catch {
        reject(new Error("Invalid simulator startup response"));
      }
    });
  });
}
async function post(url, token, body) {
  return fetch(url, {
    method: "POST",
    redirect: "error",
    signal: AbortSignal.timeout(5000),
    headers: {
      Authorization: `Bearer ${token}`,
      "Content-Type": "application/json",
    },
    body: JSON.stringify(body),
  });
}
async function stop(code = 0) {
  if (stopping) return stopping;
  stopping = (async () => {
    clearInterval(clockTimer);
    // Close Vite before yielding so its signal callback cannot exit ahead of cleanup.
    const closing = vite?.close();
    await clockWork;
    await closing;
    if (hub && hubToken) {
      try {
        const status = await (
          await fetch(`${hub.endpoint}/v1/status`, {
            headers: { Authorization: `Bearer ${hubToken}` },
            signal: AbortSignal.timeout(3000),
          })
        ).json();
        await post(`${hub.endpoint}/v1/daemon/shutdown`, hubToken, status);
      } catch {
        /* Owned child fallback below. */
      }
    }
    if (upstream && controlToken)
      await post(
        `http://${upstream.upstream}/shutdown`,
        controlToken,
        {},
      ).catch(() => {});
    for (const child of children) {
      if (child.exitCode === null && child.signalCode === null)
        await new Promise((done) => {
          const timer = setTimeout(() => {
            child.kill();
            done();
          }, 5000);
          child.once("exit", () => {
            clearTimeout(timer);
            done();
          });
        });
    }
    if (temporary) await rm(data, { recursive: true, force: true });
    process.exitCode = code;
  })();
  return stopping;
}
process.once("SIGINT", () => void stop());
process.once("SIGTERM", () => void stop());
try {
  upstream = await start("upstream");
  controlToken = (await readFile(upstream.controlTokenFile, "utf8")).trim();
  hub = await start("hub", ["--upstream", upstream.upstream]);
  hubToken = (await readFile(hub.tokenFile, "utf8")).trim();
  const status = await (
    await fetch(`${hub.endpoint}/v1/status`, {
      headers: { Authorization: `Bearer ${hubToken}` },
    })
  ).json();
  if (status.version?.backend !== "simulator")
    throw new Error("Development UI requires a simulator hub");
  const previewSecretFile = join(data, "ui-preview-secret");
  try {
    secret = (await readFile(previewSecretFile, "utf8")).trim();
  } catch (error) {
    if (error.code !== "ENOENT") throw error;
    await writeFile(previewSecretFile, secret, { mode: 0o600, flag: "wx" });
  }
  if (!/^[0-9a-f]{64}$/.test(secret))
    throw new Error("Invalid preview bootstrap secret");
  clockTimer = setInterval(() => {
    if (advancing) return;
    advancing = true;
    clockWork = post(`http://${upstream.upstream}/control`, controlToken, {
      op: "advance",
      seconds: 1,
    })
      .catch(() => {})
      .finally(() => {
        advancing = false;
      });
  }, 1000);
  clockTimer.unref();
  let allowedOrigin = process.env.CLIENT_UI_ORIGIN || undefined;
  const external = allowedOrigin ? new URL(allowedOrigin) : null;
  if (
    external &&
    (external.protocol !== "https:" ||
      external.username ||
      external.password ||
      external.pathname !== "/" ||
      external.search ||
      external.hash)
  )
    throw new Error("CLIENT_UI_ORIGIN must be an HTTPS origin");
  if (external) allowedOrigin = external.origin;
  vite = await createServer({
    root,
    cacheDir: join(data, "vite-cache"),
    configFile: false,
    server: {
      host: "127.0.0.1",
      port: Number(process.env.CLIENT_UI_PORT ?? 5173),
      strictPort: true,
      allowedHosts: external ? [external.hostname] : [],
      hmr: external
        ? {
            host: external.hostname,
            protocol: "wss",
            clientPort: Number(external.port || 443),
          }
        : undefined,
      proxy: { "/v1": { target: hub.endpoint, changeOrigin: false } },
    },
    plugins: [
      {
        name: "isolated-simulator",
        configureServer(server) {
          server.middlewares.use(async (req, res, next) => {
            if (!req.url?.startsWith("/__dev/")) return next();
            res.setHeader("Cache-Control", "no-store");
            res.setHeader("Content-Type", "application/json");
            res.setHeader("X-Content-Type-Options", "nosniff");
            const cookie = req.headers.cookie
              ?.split(";")
              .map((x) => x.trim())
              .find((x) => x.startsWith("agentwechat-dev="))
              ?.slice("agentwechat-dev=".length);
            const authorization = req.headers.authorization?.replace(
              /^Bearer /,
              "",
            );
            if (
              req.headers.host !== new URL(allowedOrigin).host ||
              (req.headers.origin && req.headers.origin !== allowedOrigin) ||
              (!equal(authorization) && !equal(cookie))
            ) {
              res.statusCode = 403;
              return res.end("{}");
            }
            if (req.url === "/__dev/session" && req.method === "GET") {
              res.setHeader(
                "Set-Cookie",
                `agentwechat-dev=${secret}; HttpOnly; SameSite=Strict; Path=/__dev/${external ? "; Secure" : ""}`,
              );
              return res.end(
                JSON.stringify({
                  endpoint: allowedOrigin,
                  token: hubToken,
                  simulator: true,
                }),
              );
            }
            if (req.url === "/__dev/state" && req.method === "GET") {
              try {
                const response = await fetch(
                  `http://${upstream.upstream}/state`,
                  {
                    headers: { Authorization: `Bearer ${controlToken}` },
                    signal: AbortSignal.timeout(5000),
                    redirect: "error",
                  },
                );
                if (!response.ok) throw new Error();
                const state = await response.json();
                return res.end(JSON.stringify({ clock: state.clock }));
              } catch {
                res.statusCode = 503;
                return res.end("{}");
              }
            }
            if (req.url === "/__dev/control" && req.method === "POST") {
              try {
                let body = "";
                for await (const chunk of req) {
                  body += chunk;
                  if (body.length > 16384) throw new Error();
                }
                const response = await post(
                  `http://${upstream.upstream}/control`,
                  controlToken,
                  JSON.parse(body),
                );
                res.statusCode = response.status;
                return res.end(await response.text());
              } catch {
                res.statusCode = 400;
                return res.end("{}");
              }
            }
            res.statusCode = 404;
            res.end("{}");
          });
        },
      },
    ],
  });
  await vite.listen();
  allowedOrigin ??= `http://127.0.0.1:${vite.httpServer.address().port}`;
  console.log(
    `\nAgentWechat with synthetic WeChat upstream\n${allowedOrigin}/#dev=${secret}\n\nSimulator data: ${data}\nPress Ctrl+C to stop the owned hub and simulator.\n`,
  );
} catch (error) {
  console.error(error.message);
  await stop(1);
}
