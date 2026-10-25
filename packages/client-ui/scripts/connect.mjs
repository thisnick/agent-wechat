// Explicit browser connection to an existing hub. Never starts/stops a daemon.
import { readFile, writeFile, mkdir, lstat } from "node:fs/promises";
import { resolve, dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { randomBytes, timingSafeEqual } from "node:crypto";
import { createServer } from "vite";
import { NativeWeChatClient } from "@agent-wechat/shared";

const argument = (name) => {
  const index = process.argv.indexOf(name);
  if (
    index < 0 ||
    !process.argv[index + 1] ||
    process.argv[index + 1].startsWith("--")
  )
    throw new Error(`Required: ${name}`);
  return process.argv[index + 1];
};
let vite;
async function stop() {
  await vite?.close();
}
process.once("SIGINT", () => void stop());
process.once("SIGTERM", () => void stop());
try {
  const endpoint = argument("--server");
  const account = process.argv.includes("--account")
    ? argument("--account")
    : null;
  const tokenFile = resolve(argument("--token-file"));
  const state = resolve(argument("--state-dir"));
  if (account !== null && !/^[a-zA-Z0-9_.-]{1,128}$/.test(account))
    throw new Error("Invalid account ID");
  const tokenInfo = await lstat(tokenFile);
  if (!tokenInfo.isFile() || tokenInfo.mode & 0o077)
    throw new Error("Use an owner-only token file");
  const token = (await readFile(tokenFile, "utf8")).trim();
  const client = new NativeWeChatClient({ baseUrl: endpoint, token });
  if (client.baseUrl.pathname !== "/")
    throw new Error("Use the hub origin as --server");
  const status = await client.status();
  if (status.version.backend !== "native")
    throw new Error(
      "Expected an existing native WeChat hub; use dev for the simulator",
    );
  const accounts = await client.accounts();
  if (account && !accounts.items.some((item) => item.id === account))
    throw new Error("Account not available with this token");
  await mkdir(state, { recursive: true, mode: 0o700 });
  const secretFile = join(state, "ui-connect-secret");
  try {
    await writeFile(secretFile, randomBytes(32).toString("hex"), {
      mode: 0o600,
      flag: "wx",
    });
  } catch (error) {
    if (error.code !== "EEXIST") throw error;
  }
  const info = await lstat(secretFile);
  if (!info.isFile() || info.mode & 0o077)
    throw new Error("Use an owner-only preview secret file");
  const secret = (await readFile(secretFile, "utf8")).trim();
  if (!/^[0-9a-f]{64}$/.test(secret)) throw new Error("Invalid preview secret");
  const equal = (value) =>
    typeof value === "string" &&
    /^[0-9a-f]{64}$/.test(value) &&
    timingSafeEqual(Buffer.from(value), Buffer.from(secret));
  let origin = process.env.CLIENT_UI_ORIGIN || undefined;
  const external = origin ? new URL(origin) : null;
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
  if (external) origin = external.origin;
  vite = await createServer({
    root: resolve(dirname(fileURLToPath(import.meta.url)), ".."),
    cacheDir: join(state, "vite-cache"),
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
      proxy: { "/v1": { target: client.baseUrl.origin, changeOrigin: false } },
    },
    plugins: [
      {
        name: "explicit-native-hub",
        configureServer(server) {
          server.middlewares.use((req, res, next) => {
            const path = req.url?.split("?")[0] ?? "";
            if (path.startsWith("/__dev/")) {
              res.setHeader("Cache-Control", "no-store");
              res.setHeader("Content-Type", "application/json");
              res.setHeader("X-Content-Type-Options", "nosniff");
              const cookie = req.headers.cookie
                ?.split(";")
                .map((part) => part.trim())
                .find((part) => part.startsWith("agentwechat-connect="))
                ?.slice("agentwechat-connect=".length);
              const authorization = req.headers.authorization?.replace(
                /^Bearer /,
                "",
              );
              if (
                req.headers.host !== new URL(origin).host ||
                (req.headers.origin && req.headers.origin !== origin) ||
                (!equal(authorization) && !equal(cookie))
              ) {
                res.statusCode = 403;
                return res.end("{}");
              }
              if (path !== "/__dev/session" || req.method !== "GET") {
                res.statusCode = 404;
                return res.end("{}");
              }
              res.setHeader(
                "Set-Cookie",
                `agentwechat-connect=${secret}; HttpOnly; SameSite=Strict; Path=/__dev/; Max-Age=2592000${
                  external ? "; Secure" : ""
                }`,
              );
              return res.end(
                JSON.stringify({
                  endpoint: origin,
                  token: secret,
                  simulator: false,
                }),
              );
            }
            if (path.startsWith("/v1")) {
              // The browser uses the preview key; the daemon key stays server-side.
              if (
                req.headers.host !== new URL(origin).host ||
                (req.headers.origin && req.headers.origin !== origin) ||
                !equal(req.headers.authorization?.replace(/^Bearer /, ""))
              ) {
                res.statusCode = 401;
                return res.end();
              }
              let parts;
              try {
                parts = path.split("/").map(decodeURIComponent);
              } catch {
                res.statusCode = 400;
                return res.end();
              }
              const safe = !parts.some(
                (part) =>
                  part === "." || part === ".." || /[\\/\u0000]/.test(part),
              );
              const read = req.method === "GET" || req.method === "HEAD";
              const allowed =
                (read && (path === "/v1/status" || path === "/v1/accounts")) ||
                (path === "/v1/accounts" && req.method === "POST") ||
                (parts[1] === "v1" && parts[2] === "accounts" && !!parts[3]);
              if (!safe || !allowed) {
                res.statusCode = 403;
                return res.end();
              }
              req.headers.authorization = `Bearer ${token}`;
            }
            next();
          });
        },
      },
    ],
  });
  await vite.listen();
  origin ??= `http://127.0.0.1:${vite.httpServer.address().port}`;
  console.log(
    `\nAgentWechat connected to an existing native hub\n${origin}/#dev=${secret}\n\nProfile management uses the existing connection key. Closing this launcher leaves the daemon running.\n`,
  );
} catch (error) {
  console.error(error.message);
  await stop();
  process.exitCode = 1;
}
