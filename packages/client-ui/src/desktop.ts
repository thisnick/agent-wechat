import { Channel, invoke, isTauri } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { connect, type Connection } from "./connection.ts";
export const desktop = isTauri();
export type DesktopSession = { id: string; endpoint: string; local: boolean };
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
  notificationStatus: string;
};
export async function desktopConnect(
  session: DesktopSession,
): Promise<Connection> {
  const connection = await connect(
    session.endpoint,
    "desktop-native-session",
    false,
    desktopFetch(session),
  );
  connection.desktopId = session.id;
  return connection;
}
export type NotificationTarget = { connectionId: string; serverId: string; accountId: string; threadId: string };
export function desktopNotifications(connection: Connection, open: () => void): () => void {
  let stopped = false;
  const take = async () => {
    const target = await invoke<NotificationTarget | null>("notifications_take_target");
    if (!target || stopped || target.connectionId !== connection.desktopId) return;
    await connection.session.activate(target.accountId);
    if (stopped) return;
    await connection.session.select(target.threadId);
    open();
  };
  const listening = listen("desktop-open-thread", () => { void take().catch(() => {}); });
  const focus = () => { void take().catch(() => {}); };
  window.addEventListener("focus", focus);
  focus();
  return () => { stopped = true; window.removeEventListener("focus", focus); void listening.then((unlisten) => unlisten()); };
}
export const desktopInvoke = invoke;
export function desktopFetch(session: DesktopSession): typeof fetch {
  return async (input, init) => {
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
      const body = request.body !== null;
      if (body) {
        await invoke("http_body_begin", { id });
        const reader = request.body!.getReader();
        try {
          for (;;) {
            request.signal.throwIfAborted();
            const next = await reader.read();
            if (next.done) break;
            for (let offset = 0; offset < next.value.length; offset += 65536) {
              request.signal.throwIfAborted();
              await invoke("http_body_chunk", {
                id,
                bytes: Array.from(next.value.subarray(offset, offset + 65536)),
              });
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
      type Packet = { type: "data"; bytes: string } | { type: "end" | "error" };
      const channel = new Channel<Packet>();
      channel.onmessage = (packet) => {
        if (finished) return;
        if (packet.type === "data") {
          const raw = atob(packet.bytes);
          queued.push(
            Uint8Array.from(raw, (character) => character.charCodeAt(0)),
          );
          pump();
        } else if (packet.type === "end") {
          ended = true;
          pump();
        } else {
          cancel();
          controller!.error(new Error("The hub connection was interrupted"));
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
            detail: "The file could not be saved. Try downloading it again.",
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
        for (let offset = 0; offset < next.value.length; offset += 65536)
          await invoke("http_body_chunk", {
            id,
            bytes: Array.from(next.value.subarray(offset, offset + 65536)),
          });
      }
    } finally {
      reader.releaseLock();
    }
    await invoke("desktop_save_file", { id, name });
  } finally {
    await invoke("http_cancel", { id });
  }
}
