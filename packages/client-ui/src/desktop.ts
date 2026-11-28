import { Channel, invoke, isTauri } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { connect, type Connection } from "./connection.ts";
import type { NativeStatus } from "@agent-wechat/shared";
import { DesktopCallTransport } from "./calls/desktop-transport.ts";
import { desktopMultipart } from "./desktop-multipart.ts";
import type { Route } from "./routes.ts";
import { t } from "./i18n.ts";
export const desktop = isTauri();
export type DesktopSession = { id: string; endpoint: string; local: boolean; status?: NativeStatus };
export type DesktopSettings = {
  connections: { id: string; name: string; endpoint: string; local: boolean }[];
  active: string | null;
  localPort: number;
  networkPort: number;
  networkEnabled: boolean;
  networkAddresses: string[];
  certificate: string | null;
  cliPath: string;
  notificationsEnabled: boolean;
  notificationSoundEnabled: boolean;
  notificationStatus: string;
  /** Whether this device's hub keeps running while another hub is selected. */
  keepLocalHubRunning: boolean;
  /** Why this device's hub last failed to start, until it starts. */
  localHubError: string | null;
  /** A hub from an earlier run of the app holds the local port and can be stopped. */
  leftoverHub: boolean;
};
export async function desktopConnect(
  session: DesktopSession,
): Promise<Connection> {
  const connection = await connect(
    session.endpoint,
    "desktop-native-session",
    false,
    desktopFetch(session),
    session.status,
    {
      callTransportFactory: (account, client) =>
        new DesktopCallTransport(session.id, account, client),
    },
  );
  connection.desktopId = session.id;
  return connection;
}
export type DesktopCallAction = {
  connectionId: string;
  serverId: string;
  accountId: string;
  peerId: string;
  callId: string;
  revision: number;
  callType: "voice" | "video";
  action: "open" | "answer";
};
/** Calls `onAction` for each incoming-call OS notification click or Answer action on this hub. */
export function desktopCallActions(
  connection: Connection,
  onAction: (action: DesktopCallAction) => void,
): () => void {
  let stopped = false;
  const take = async () => {
    if (!connection.desktopId) return;
    const action = await invoke<DesktopCallAction | null>("notifications_take_call_action", {
      connectionId: connection.desktopId,
    });
    if (!action || stopped || action.connectionId !== connection.desktopId) return;
    onAction(action);
  };
  const listening = listen("desktop-call-action", () => {
    void take().catch(() => {});
  });
  const focus = () => {
    void take().catch(() => {});
  };
  window.addEventListener("focus", focus);
  focus();
  return () => {
    stopped = true;
    window.removeEventListener("focus", focus);
    void listening.then((unlisten) => unlisten());
  };
}
export type NotificationTarget = {
  connectionId: string;
  serverId: string;
  accountId: string;
  threadId: string;
  /** The message that raised the alert, when the notification knows it. */
  messageId?: string;
};
/** Where a clicked notification leads: the message when known, else its conversation. */
export function notificationRoute(target: NotificationTarget): Route {
  const where = { server: target.serverId, account: target.accountId, thread: target.threadId };
  return target.messageId
    ? { name: "message", ...where, message: target.messageId }
    : { name: "chat", ...where };
}
/** Calls `open` with the route of each notification the user clicks on this hub. */
export function desktopNotifications(connection: Connection, open: (route: Route) => void): () => void {
  let stopped = false;
  const take = async () => {
    // Name this window's connection so a target for another connection stays pending.
    if (!connection.desktopId) return;
    const target = await invoke<NotificationTarget | null>("notifications_take_target", { connectionId: connection.desktopId });
    if (!target || stopped || target.connectionId !== connection.desktopId) return;
    open(notificationRoute(target));
  };
  const listening = listen("desktop-open-thread", () => { void take().catch(() => {}); });
  const focus = () => { void take().catch(() => {}); };
  window.addEventListener("focus", focus);
  focus();
  return () => { stopped = true; window.removeEventListener("focus", focus); void listening.then((unlisten) => unlisten()); };
}
export const desktopInvoke = invoke;

const UPLOAD_CHUNK_BYTES = 256 * 1024;
const FRAME_DATA = 0;
const FRAME_END = 1;
const FRAME_ERROR = 2;

async function sendUploadChunk(id: string, chunk: Uint8Array): Promise<void> {
  const payload =
    chunk.byteOffset === 0 && chunk.byteLength === chunk.buffer.byteLength
      ? chunk
      : chunk.slice();
  await invoke("http_body_chunk", payload, {
    headers: { "x-upload-id": id },
  });
}

export function desktopFetch(session: DesktopSession): typeof fetch {
  return async (input, init) => {
    const multipart = init?.body instanceof FormData ? desktopMultipart(init.body) : undefined;
    if (multipart) {
      const headers = new Headers(init?.headers ?? (input instanceof Request ? input.headers : undefined));
      headers.set("Content-Type", multipart.contentType);
      init = { ...init, headers, body: null };
    }
    const request = new Request(input, init);
    const url = new URL(request.url);
    if (url.origin !== new URL(session.endpoint).origin)
      throw new Error("Unexpected hub origin");
    const id = crypto.randomUUID();
    let finished = false,
      ended = false,
      waiting = false;
    let controller: ReadableStreamDefaultController<Uint8Array> | undefined;
    const queued: Uint8Array[] = [];
    const cleanup = () => request.signal.removeEventListener("abort", abort);
    const cancel = () => {
      if (!finished) {
        finished = true;
        cleanup();
        void invoke("http_cancel", { id });
      }
    };
    const abort = () => {
      cancel();
      try {
        controller?.error(
          request.signal.reason ?? new DOMException("Aborted", "AbortError"),
        );
      } catch {}
    };
    const pump = () => {
      if (finished) return;
      if (waiting && queued.length) {
        waiting = false;
        controller!.enqueue(queued.shift()!);
        void invoke("http_ack", { id });
      }
      if (ended && !queued.length) {
        finished = true;
        cleanup();
        controller!.close();
      }
    };
    request.signal.addEventListener("abort", abort, { once: true });
    try {
      request.signal.throwIfAborted();
      const body = !!multipart || request.body !== null;
      if (body) {
        await invoke("http_body_begin", { id });
        const reader = (multipart ? multipart.body.stream() : request.body!).getReader();
        try {
          for (;;) {
            request.signal.throwIfAborted();
            const next = await reader.read();
            if (next.done) break;
            for (
              let offset = 0;
              offset < next.value.length;
              offset += UPLOAD_CHUNK_BYTES
            ) {
              request.signal.throwIfAborted();
              await sendUploadChunk(
                id,
                next.value.subarray(offset, offset + UPLOAD_CHUNK_BYTES),
              );
            }
          }
        } finally {
          reader.releaseLock();
        }
      }
      const stream = new ReadableStream<Uint8Array>(
        {
          start(value) {
            controller = value;
          },
          pull() {
            waiting = true;
            pump();
          },
          cancel,
        },
        { highWaterMark: 0 },
      );
      const channel = new Channel<ArrayBuffer>();
      const interrupt = () => {
        cancel();
        controller!.error(new Error("The hub connection was interrupted"));
      };
      channel.onmessage = (packet) => {
        if (finished) return;
        if (!(packet instanceof ArrayBuffer)) {
          interrupt();
          return;
        }
        const view = new Uint8Array(packet);
        if (view.byteLength === 0) {
          interrupt();
          return;
        }
        switch (view[0]) {
          case FRAME_DATA:
            queued.push(view.subarray(1));
            pump();
            break;
          case FRAME_END:
            ended = true;
            pump();
            break;
          case FRAME_ERROR:
          default:
            interrupt();
        }
      };
      const headers = Object.fromEntries(
        [...request.headers].filter(([name]) => name !== "authorization"),
      );
      const head = await invoke<{
        status: number;
        headers: Record<string, string>;
      }>("http_request", {
        request: {
          id,
          connectionId: session.id,
          path: url.pathname + url.search,
          method: request.method,
          headers,
          body,
        },
        channel,
      });
      request.signal.throwIfAborted();
      if (
        [101, 204, 205, 304].includes(head.status) ||
        request.method === "HEAD"
      ) {
        cancel();
        return new Response(null, head);
      }
      return new Response(stream, head);
    } catch (error) {
      cancel();
      throw error;
    }
  };
}

export function installDesktopLinks(): () => void {
  const click = (event: MouseEvent) => {
    const link = (event.target as Element | null)?.closest<HTMLAnchorElement>(
      "a[href]",
    );
    if (!link || event.defaultPrevented) return;
    if (link.hasAttribute("download") && link.href.startsWith("blob:")) {
      event.preventDefault();
      void saveDesktopBlob(link.href, link.download || "download").catch(() => {
        globalThis.dispatchEvent(
          new CustomEvent("desktop-error", {
            detail: t("desktop.fileNotSaved"),
          }),
        );
      });
    } else if (/^https?:/.test(link.href)) {
      event.preventDefault();
      void invoke("plugin:opener|open_url", { url: link.href });
    }
  };
  document.addEventListener("click", click);
  return () => document.removeEventListener("click", click);
}
async function saveDesktopBlob(url: string, name: string): Promise<void> {
  const response = await globalThis.fetch(url);
  await saveDesktopFile(await response.blob(), name);
}
export async function saveDesktopFile(blob: Blob, name: string): Promise<void> {
  const id = crypto.randomUUID();
  await invoke("http_body_begin", { id });
  try {
    const reader = blob.stream().getReader();
    try {
      for (;;) {
        const next = await reader.read();
        if (next.done) break;
        for (
          let offset = 0;
          offset < next.value.length;
          offset += UPLOAD_CHUNK_BYTES
        ) {
          await sendUploadChunk(
            id,
            next.value.subarray(offset, offset + UPLOAD_CHUNK_BYTES),
          );
        }
      }
    } finally {
      reader.releaseLock();
    }
    await invoke("desktop_save_file", { id, name });
  } finally {
    await invoke("http_cancel", { id });
  }
}
