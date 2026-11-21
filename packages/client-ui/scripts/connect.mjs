// Explicit browser connection to an existing hub. Never starts/stops a daemon.
import { readFile, writeFile, mkdir, lstat } from "node:fs/promises";
import { resolve, dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { randomBytes, timingSafeEqual } from "node:crypto";
import { createServer } from "vite";
import { NativeWeChatClient, validateNativeBaseUrl } from "@agent-wechat/shared";

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
  const multiple = process.argv.includes("--connections-file");
  const privateText = async (path) => {
    const info = await lstat(path);
    if (!info.isFile() || info.mode & 0o077 || info.size > 32768) throw new Error("Use an owner-only connection file");
    return readFile(path, "utf8");
  };
  const targets = multiple ? JSON.parse(await privateText(resolve(argument("--connections-file")))) : [{ id: "default", name: "Hub", server: argument("--server"), tokenFile: resolve(argument("--token-file")) }];
  if (!Array.isArray(targets) || !targets.length || targets.length > 8) throw new Error("Configure between one and eight preview hubs");
  const hubs = new Map();
  for (const target of targets) {
    if (!target || typeof target !== "object" || typeof target.id !== "string" || !/^[a-z0-9-]{1,48}$/.test(target.id) || hubs.has(target.id)
      || typeof target.name !== "string" || !target.name.trim() || target.name.length > 80
      || typeof target.server !== "string" || target.endpoint !== undefined && typeof target.endpoint !== "string"
      || typeof target.tokenFile !== "string") throw new Error("Invalid preview hub configuration");
    const token = (await privateText(resolve(target.tokenFile))).trim();
    const client = new NativeWeChatClient({ baseUrl: target.server, token });
    const browser = validateNativeBaseUrl(target.endpoint ?? target.server);
    if (client.baseUrl.pathname !== "/" || browser.pathname !== "/") throw new Error("Use a hub origin in preview connections");
    hubs.set(target.id, { ...target, client, token, endpoint: browser.origin });
  }
  const first = hubs.values().next().value;
  const account = process.argv.includes("--account")
    ? argument("--account")
    : null;
  const state = resolve(argument("--state-dir"));
  if (account !== null && !/^[a-zA-Z0-9_.-]{1,128}$/.test(account))
    throw new Error("Invalid account ID");
  if (!multiple) {
    const status = await first.client.status();
    if (status.version.backend !== "native") throw new Error("Expected an existing native WeChat hub; use dev for the simulator");
    const accounts = await first.client.accounts();
    if (account && !accounts.items.some((item) => item.id === account)) throw new Error("Account not available with this token");
  }
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
      proxy: {
        "/v1": { target: first.client.baseUrl.origin, changeOrigin: false, ws: true },
        ...Object.fromEntries([...hubs].map(([id, hub]) => [`/__hubs/${id}/v1`, {
          target: hub.client.baseUrl.origin, changeOrigin: false, ws: true,
          rewrite: (path) => path.slice(`/__hubs/${id}`.length),
        }])),
      },
    },
    plugins: [
      {
        name: "explicit-native-hub",
        configureServer(server) {
          const rejectUpgrade = (socket, status, text) => {
            try {
              socket.end(`HTTP/1.1 ${status} ${text}\r\nConnection: close\r\nContent-Length: 0\r\n\r\n`);
            } catch {
              socket.destroy();
            }
          };
          const validateUpgrade = (req, socket) => {
            const rawUrl = req.url ?? "";
            const path = rawUrl.split("?")[0] ?? "";
            const route = /^\/__hubs\/([a-z0-9-]+)(\/v1(?:\/.*)?$)/.exec(path);
            if (path.startsWith("/__hubs/") && (!route || !hubs.has(route[1]))) {
              rejectUpgrade(socket, 404, "Not Found");
              return false;
            }
            if (!path.startsWith("/v1") && !route) return true;
            const apiPath = route ? route[2] : path;
            if (
              !origin ||
              req.headers.host !== new URL(origin).host ||
              (req.headers.origin && req.headers.origin !== origin)
            ) {
              rejectUpgrade(socket, 403, "Forbidden");
              return false;
            }
            let parsed;
            let parts;
            try {
              parsed = new URL(rawUrl, origin);
              parts = apiPath.split("/").map(decodeURIComponent);
            } catch {
              rejectUpgrade(socket, 400, "Bad Request");
              return false;
            }
            const safe = !parts.some(
              (part) => part === "." || part === ".." || /[\\/\u0000]/.test(part),
            );
            const isControlSocket =
              parts.length === 6 &&
              parts[1] === "v1" &&
              parts[2] === "accounts" &&
              !!parts[3] &&
              parts[4] === "calls" &&
              parts[5] === "socket";
            const isMediaSocket =
              parts.length === 7 &&
              parts[1] === "v1" &&
              parts[2] === "accounts" &&
              !!parts[3] &&
              parts[4] === "calls" &&
              !!parts[5] &&
              parts[6] === "media";
            const ticket = parsed.searchParams.get("ticket");
            const validTicket = typeof ticket === "string" && /^[0-9a-f]{64}$/i.test(ticket);
            if (!safe || (!isControlSocket && !isMediaSocket) || !validTicket) {
              rejectUpgrade(socket, 403, "Forbidden");
              return false;
            }
            // Preview proxy already verified Host and Origin; strip Origin so upstream
            // check_origin does not fail if host/origin headers differ across hops, and
            // never forward browser preview credentials on ticket-authenticated upgrades.
            delete req.headers.origin;
            delete req.headers.authorization;
            return true;
          };
          // Wrap the httpServer upgrade emit so rejected proxy upgrades never reach Vite's proxy listeners.
          const httpServer = server.httpServer;
          if (httpServer) {
            const originalEmit = httpServer.emit.bind(httpServer);
            httpServer.emit = (event, ...args) => {
              if (event === "upgrade") {
                const [req, socket] = args;
                if (!validateUpgrade(req, socket)) return true;
              }
              return originalEmit(event, ...args);
            };
          }
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
              if (!["/__dev/session", "/__dev/connections"].includes(path) || req.method !== "GET") {
                res.statusCode = 404;
                return res.end("{}");
              }
              res.setHeader(
                "Set-Cookie",
                `agentwechat-connect=${secret}; HttpOnly; SameSite=Strict; Path=/__dev/; Max-Age=2592000${
                  external ? "; Secure" : ""
                }`,
              );
              if (path === "/__dev/connections") return res.end(JSON.stringify(multiple ? [...hubs.values()].map(({ id, name, endpoint }) => ({ id, name, endpoint })) : []));
              const selected = new URL(req.url, origin).searchParams.get("hub") ?? first.id;
              const hub = hubs.get(selected);
              if (!hub) { res.statusCode = 404; return res.end("{}"); }
              return res.end(
                JSON.stringify({
                  endpoint: multiple ? hub.endpoint : origin,
                  token: secret,
                  simulator: false,
                  ...(multiple && { hub: hub.id, proxyPath: `/__hubs/${hub.id}` }),
                }),
              );
            }
            const route = /^\/__hubs\/([a-z0-9-]+)(\/v1(?:\/.*)?$)/.exec(path);
            if (path.startsWith("/__hubs/") && (!route || !hubs.has(route[1]))) { res.statusCode = 404; return res.end(); }
            if (path.startsWith("/v1") || route) {
              const apiPath = route ? route[2] : path;
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
                parts = apiPath.split("/").map(decodeURIComponent);
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
                (read && (apiPath === "/v1/status" || apiPath === "/v1/accounts")) ||
                (apiPath === "/v1/accounts" && req.method === "POST") ||
                (parts[1] === "v1" && parts[2] === "accounts" && !!parts[3]);
              if (!safe || !allowed) {
                res.statusCode = 403;
                return res.end();
              }
              req.headers.authorization = `Bearer ${(route ? hubs.get(route[1]) : first).token}`;
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
