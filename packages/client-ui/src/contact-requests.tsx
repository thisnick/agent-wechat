import { Command } from "cmdk";
import { useEffect, useRef, useState } from "react";
import {
  NativeApiError,
  isNativeTombstone,
  type NativeContactRequest,
  type NativeContactSearch,
  type NativeOperation,
} from "@agent-wechat/shared";
import type { Connection } from "./connection.ts";
import { t } from "./i18n.ts";
import { readableError } from "./session.ts";
import { HubAvatar } from "./avatars.tsx";
import { Icon } from "./ui.tsx";

const contactActions = {
  send: "send_request",
  accept: "accept_request",
  delete: "delete_request",
  remove: "remove_contact",
} as const;
type ContactReceipt = {
  key: string;
  operation?: NativeOperation;
  input?: { recipientRef: string; message: string };
  action: keyof typeof contactActions;
};
export type RequestSelection =
  | { request: NativeContactRequest }
  | { discovery: NativeContactSearch }
  | { removeContact: { id: string; displayName: string } };
export function useContactRequests(connection: Connection, account: string) {
  const [items, setItems] = useState<NativeContactRequest[]>([]),
    [count, setCount] = useState(0),
    [error, setError] = useState("");
  const [revision, setRevision] = useState(0);
  const refresh = () => setRevision((n) => n + 1);
  useEffect(() => {
    setItems([]);
    setCount(0);
    setError("");
  }, [connection, account]);
  useEffect(() => {
    const controller = new AbortController();
    void (async () => {
      try {
        const all: NativeContactRequest[] = [];
        let cursor: string | undefined;
        let pages = 0;
        do {
          const page = await connection.session.client.contactRequests(
            account,
            { limit: 200, cursor },
            controller.signal
          );
          if (controller.signal.aborted) return;
          all.push(
            ...page.items.filter((item): item is NativeContactRequest => !isNativeTombstone(item))
          );
          cursor = page.nextCursor ?? undefined;
          if (++pages > 50) throw Error(t("contactsUi.tooManyRequests"));
          setCount(page.pendingIncomingCount);
        } while (cursor);
        if (!controller.signal.aborted) {
          setItems(all);
          setError("");
        }
      } catch (e) {
        if (!controller.signal.aborted) setError(readableError(e));
      }
    })();
    return () => controller.abort();
  }, [connection, account, revision]);
  useEffect(() => {
    const off = connection.session.onEvent((event, a) => {
      if (a === account && ["friend_request.updated", "contact.updated"].includes(event.kind))
        refresh();
    });
    const gap = connection.session.onEventGap((a) => {
      if (a === account) refresh();
    });
    return () => {
      off();
      gap();
    };
  }, [connection, account]);
  return { items, count, error, refresh };
}
function requestLabel(request: NativeContactRequest) {
  return request.isContact
    ? t("contactsUi.saved")
    : request.direction === "outgoing"
    ? t("contactsUi.sent")
    : t("contactsUi.incoming");
}
function requestAction(request: NativeContactRequest) {
  return request.isContact
    ? t("contactsUi.message")
    : request.actionable
    ? t("contactsUi.review")
    : t("contactsUi.view");
}
export function FriendRequests({
  connection,
  account,
  requests,
  review,
  open,
  back,
}: {
  connection: Connection;
  account: string;
  back: () => void;
  requests: ReturnType<typeof useContactRequests>;
  review: (selection: RequestSelection) => void;
  open: (id: string) => void;
}) {
  const [direction, setDirection] = useState("all");
  return (
    <main className="workspace friend-requests">
      <header>
        <button className="back-btn" onClick={back} aria-label={t("main.back")}>
          <Icon name="back" />
        </button>
        <h2>{t("contactsUi.requests")}</h2>
        <p>{t("contactsUi.needsReview", { count: requests.count })}</p>
      </header>
      <div className="omni-scopes">
        {(["all", "incoming", "outgoing"] as const).map((d) => (
          <button key={d} aria-pressed={direction === d} onClick={() => setDirection(d)}>
            {t(`contactsUi.${d === "outgoing" ? "sent" : d}`)}
          </button>
        ))}
      </div>
      {requests.error && (
        <p role="alert">
          {requests.error} <button onClick={requests.refresh}>{t("main.refresh")}</button>
        </p>
      )}
      {requests.items
        .filter((r) => direction === "all" || r.direction === direction)
        .map((r) => (
          <div className="omni-result" key={r.id}>
            <HubAvatar
              connection={connection}
              account={account}
              username={r.peerId}
              name={r.displayName}
            />
            <button
              className="omni-result-name"
              onClick={() => (r.isContact ? open(r.peerId) : review({ request: r }))}
            >
              <strong>{r.displayName}</strong>
              <span>
                {requestLabel(r)} · {new Date(r.updatedAt * 1000).toLocaleString()}
              </span>
              <span>{r.message}</span>
            </button>
            <button
              className="omni-action"
              onClick={() => (r.isContact ? open(r.peerId) : review({ request: r }))}
            >
              {requestAction(r)}
            </button>
          </div>
        ))}
      {!requests.items.length && !requests.error && <p>{t("contactsUi.noRequests")}</p>}
    </main>
  );
}
const scopeLabel = (scope: string) =>
  ({
    all: t("contactsUi.all"),
    contacts: t("contactsUi.contacts"),
    groups: t("contactsUi.groups"),
    messages: t("contactsUi.messages"),
  }[scope]);
type Hit = {
  id: string;
  name: string;
  subtitle?: string;
  kind: "contacts" | "groups" | "messages";
  thread?: string;
  request?: NativeContactRequest;
  discovery?: NativeContactSearch;
};
export function OmniSearch({
  connection,
  account,
  initialScope,
  requests,
  suspended,
  close,
  open,
  review,
}: {
  connection: Connection;
  account: string;
  initialScope: string;
  requests: NativeContactRequest[];
  suspended: boolean;
  close: () => void;
  open: (thread: string, message?: string) => void;
  review: (selection: RequestSelection) => void;
}) {
  const [query, setQuery] = useState(""),
    [scope, setScope] = useState(initialScope),
    [hits, setHits] = useState<Hit[]>([]),
    [remote, setRemote] = useState<NativeContactSearch | null>(null),
    [error, setError] = useState(""),
    [remoteError, setRemoteError] = useState(""),
    [loading, setLoading] = useState(false);
  useEffect(() => {
    const abort = new AbortController();
    setRemote(null);
    setRemoteError("");
    setError("");
    setHits([]);
    const search = query.trim();
    void (async () => {
      try {
        const client = connection.session.client;
        const [contacts, groups, messages] = await Promise.all([
          scope === "all" || scope === "contacts"
            ? client.contacts(account, { query: search, limit: 30 }, abort.signal)
            : null,
          scope === "all" || scope === "groups"
            ? client.groups(account, { query: search, limit: 20 }, abort.signal)
            : null,
          search && (scope === "all" || scope === "messages")
            ? client.searchMessages(account, { query: search, limit: 20 }, abort.signal)
            : null,
        ]);
        if (abort.signal.aborted) return;
        const rows: Hit[] = [];
        for (const c of contacts?.items ?? [])
          if (!isNativeTombstone(c))
            rows.push({
              id: c.id,
              name: c.displayName ?? c.id,
              subtitle: t("contactsUi.saved"),
              kind: "contacts",
            });
        for (const g of groups?.items ?? [])
          if (!isNativeTombstone(g))
            rows.push({
              id: g.id,
              name: g.displayName ?? g.id,
              subtitle: t("contactsUi.group"),
              kind: "groups",
            });
        for (const m of messages?.items ?? [])
          if (!isNativeTombstone(m))
            rows.push({
              id: m.id,
              thread: m.threadId,
              name:
                connection.session.snapshot().account === account
                  ? connection.session.snapshot().threads.find((t) => t.id === m.threadId)
                      ?.displayName ??
                    connection.session.snapshot().contacts.find((c) => c.id === m.threadId)
                      ?.displayName ??
                    m.threadId
                  : m.threadId,
              subtitle: m.body ?? "",
              kind: "messages",
            });
        setHits(rows);
      } catch (e) {
        if (!abort.signal.aborted) setError(readableError(e));
      }
    })();
    const timer = setTimeout(() => {
      if (!search || !["all", "contacts"].includes(scope)) return;
      setLoading(true);
      void connection.session.client
        .searchContacts(account, search, abort.signal)
        .then((r) => {
          if (!abort.signal.aborted) setRemote(r);
        })
        .catch((e) => {
          if (!abort.signal.aborted) setRemoteError(readableError(e));
        })
        .finally(() => {
          if (!abort.signal.aborted) setLoading(false);
        });
    }, 600);
    return () => {
      clearTimeout(timer);
      abort.abort();
      setLoading(false);
    };
  }, [connection, account, query, scope, requests]);
  const filteredRequests = ["all", "contacts"].includes(scope)
    ? requests.filter(
        (r) =>
          !query.trim() ||
          `${r.displayName} ${r.peerId} ${r.history.map((h) => h.message).join(" ")}`
            .toLowerCase()
            .includes(query.toLowerCase())
      )
    : [];
  const merged = hits.filter(
    (h) => h.kind !== "contacts" || !filteredRequests.some((r) => r.peerId === h.id)
  );
  for (const r of filteredRequests)
    merged.push({
      id: r.peerId,
      name: r.displayName,
      subtitle: requestLabel(r),
      kind: "contacts",
      request: r,
    });
  if (remote?.request) {
    const r = remote.request;
    if (!merged.some((h) => h.request?.id === r.id || h.id === r.peerId))
      merged.push({
        id: r.peerId,
        name: r.displayName,
        subtitle: requestLabel(r),
        kind: "contacts",
        request: r,
      });
  } else if (remote?.contactId) {
    if (!merged.some((h) => h.id === remote.contactId))
      merged.push({
        id: remote.contactId,
        name: remote.displayName,
        subtitle: t("contactsUi.saved"),
        kind: "contacts",
      });
  } else if (remote)
    merged.push({
      id: "discovery",
      name: remote.displayName,
      subtitle: t("contactsUi.onWechat"),
      kind: "contacts",
      discovery: remote,
    });
  const activate = (h: Hit) => {
    if (h.discovery) review({ discovery: h.discovery });
    else if (h.request && !h.request.isContact) review({ request: h.request });
    else {
      close();
      open(h.thread ?? h.id, h.kind === "messages" ? h.id : undefined);
    }
  };
  return (
    <Command.Dialog
      open={!suspended}
      onOpenChange={(value) => {
        if (!value) close();
      }}
      label={t("contactsUi.searchEverything")}
      contentClassName="omni-search"
      overlayClassName="omni-overlay"
      shouldFilter={false}
    >
      <div className="omni-input">
        <Icon name="search" />
        <Command.Input
          aria-label={t("contactsUi.searchEverything")}
          placeholder={t("contactsUi.searchPlaceholder")}
          value={query}
          onValueChange={setQuery}
        />
        <button onClick={close} aria-label={t("contactsUi.close")}>
          Esc
        </button>
      </div>
      <div className="omni-scopes">
        {(["all", "contacts", "groups", "messages"] as const).map((s) => (
          <button key={s} aria-pressed={scope === s} onClick={() => setScope(s)}>
            {scopeLabel(s)}
          </button>
        ))}
      </div>
      <Command.List>
        {error && <p role="alert">{error}</p>}
        {(["contacts", "groups", "messages"] as const)
          .filter((kind) => merged.some((hit) => hit.kind === kind))
          .map((kind) => (
            <Command.Group key={kind} heading={scopeLabel(kind)}>
              {merged
                .filter((h) => h.kind === kind)
                .map((h) => (
                  <Command.Item
                    key={`${kind}:${h.id}`}
                    value={`${kind}:${h.id}`}
                    onSelect={() => activate(h)}
                  >
                    <span className="omni-result-copy">
                      <strong>{h.name}</strong>
                      <span>{h.subtitle}</span>
                    </span>
                    <span className="omni-action">
                      {h.discovery ? (
                        t("main.findPerson")
                      ) : h.request ? (
                        requestAction(h.request)
                      ) : h.kind === "messages" ? (
                        t("contactsUi.view")
                      ) : (
                        <>
                          <Icon name="chat" />
                          {t("contactsUi.message")}
                        </>
                      )}
                    </span>
                  </Command.Item>
                ))}
            </Command.Group>
          ))}
        {!merged.length && <Command.Empty>{t("main.noMatches")}</Command.Empty>}
      </Command.List>
      <div className="omni-foot" aria-live="polite">
        {loading
          ? t("contactsUi.searchingWechat")
          : remoteError
          ? t("contactsUi.discoveryUnavailable")
          : t("contactsUi.contactsIncludeWechat")}
      </div>
    </Command.Dialog>
  );
}
export function ContactRequestDialog({
  connection,
  account,
  selection,
  close,
  changed,
}: {
  connection: Connection;
  account: string;
  selection: RequestSelection;
  close: () => void;
  changed: () => void;
}) {
  const ref = useRef<HTMLDialogElement>(null);
  const request = "request" in selection ? selection.request : null;
  const discovery = "discovery" in selection ? selection.discovery : null;
  const removal = "removeContact" in selection ? selection.removeContact : null;
  const receiptId = `contact-change:${
    removal
      ? `remove:${removal.id}`
      : request
      ? `${request.id}:${request.version}`
      : discovery?.recipientRef
  }`;
  const [greeting, setGreeting] = useState(
      () => connection.cache.readState<ContactReceipt>(account, receiptId)?.input?.message ?? ""
    ),
    [busy, setBusy] = useState(false),
    [error, setError] = useState(""),
    [retryOriginal, setRetryOriginal] = useState(false);
  const [receipt, setReceipt] = useState<ContactReceipt | undefined>(() => {
    const saved = connection.cache.readState<ContactReceipt>(account, receiptId);
    return removal && saved?.operation && ["succeeded", "failed", "cancelled"].includes(saved.operation.state) ? undefined : saved;
  });
  useEffect(() => {
    const dialog = ref.current!;
    dialog.showModal();
    return () => dialog.close();
  }, []);
  const save = (value: ContactReceipt) => {
    connection.cache.writeState(account, receiptId, value);
    setReceipt(value);
  };
  const ended =
    receipt?.operation &&
    ["succeeded", "failed", "cancelled", "needs_review"].includes(receipt.operation.state);
  async function submit(pending: ContactReceipt) {
    if (busy) return;
    setBusy(true);
    setError("");
    setRetryOriginal(false);
    try {
      await connection.cache.flush();
      const client = connection.session.client;
      const operation =
        pending.action === "remove"
          ? await client.removeContact(account, removal!.id, pending.key)
          : pending.action === "send"
          ? await client.sendContactRequest(account, pending.input!, pending.key)
          : pending.action === "accept"
          ? await client.acceptContactRequest(account, request!.id, request!.version, pending.key)
          : await client.deleteContactRequest(account, request!.id, request!.version, pending.key);
      save({ ...pending, operation });
      if (["succeeded", "failed", "cancelled", "needs_review"].includes(operation.state)) changed();
    } catch (e) {
      setError(readableError(e));
      // Admission rejections cannot have submitted this intent. Uncertain
      // transport failures retain their original key until inspected.
      if (
        e instanceof NativeApiError &&
        e.status >= 400 &&
        e.status < 500 &&
        e.code !== "idempotency_conflict" &&
        ![408, 429].includes(e.status)
      ) {
        connection.cache.writeState(account, receiptId, null);
        setReceipt(undefined);
        changed();
      }
    } finally {
      setBusy(false);
    }
  }
  async function change(action: "send" | "accept" | "delete" | "remove") {
    if (busy || receipt) return;
    const pending: ContactReceipt = {
      key: crypto.randomUUID(),
      action,
      ...(discovery ? { input: { recipientRef: discovery.recipientRef, message: greeting } } : {}),
    };
    save(pending);
    await submit(pending);
  }
  async function check() {
    if (!receipt || busy) return;
    setBusy(true);
    setRetryOriginal(false);
    try {
      const client = connection.session.client;
      const operation = receipt.operation
        ? await client.operation(account, receipt.operation.id)
        : await client.contactMutationByKey(account, receipt.key);
      if (
        operation.accountId !== account ||
        operation.kind !== "contact_mutation" ||
        (operation.result as { action?: unknown } | null)?.action !==
          contactActions[receipt.action] ||
        (receipt.operation && operation.id !== receipt.operation.id)
      )
        throw Error(t("contactsUi.operationMismatch"));
      save({ ...receipt, operation });
      if (["succeeded", "failed", "cancelled", "needs_review"].includes(operation.state)) changed();
    } catch (e) {
      setError(readableError(e));
      if (!receipt.operation && e instanceof NativeApiError && e.status === 404)
        setRetryOriginal(true);
    } finally {
      setBusy(false);
    }
  }
  useEffect(() => {
    if (!receipt?.operation || ended || busy) return;
    const timer = setTimeout(() => void check(), 800);
    return () => clearTimeout(timer);
  }, [receipt, ended, busy]);
  const statusText = !receipt?.operation
    ? t("contactsUi.checkBeforeRetry")
    : receipt.operation.state === "succeeded"
    ? receipt.action === "send"
      ? t("contactsUi.sent")
      : receipt.action === "accept"
      ? t("contactsUi.accepted")
      : receipt.action === "delete"
      ? t("contactsUi.historyDeleted")
      : t("contactsUi.contactRemoved")
    : receipt.operation.state === "needs_review"
    ? t("contactsUi.checkBeforeRetry")
    : receipt.operation.state === "failed"
    ? t("contactsUi.failed")
    : receipt.operation.state === "cancelled"
    ? t("contactsUi.cancelled")
    : t("contactsUi.working");
  const title = removal
    ? t("contactsUi.removeContact")
    : discovery
    ? t("main.findPerson")
    : request?.direction === "outgoing"
    ? t("contactsUi.sent")
    : t("contactsUi.review");
  return (
    <dialog
      ref={ref}
      className="contact-request-dialog"
      onCancel={(e) => {
        e.preventDefault();
        close();
      }}
      onClose={close}
      aria-label={title}
    >
      <div className="contact-request-heading">
        <h2>{title}</h2>
        <button onClick={close} aria-label={t("contactsUi.close")}>
          ×
        </button>
      </div>
      <h3>{removal?.displayName ?? request?.displayName ?? discovery?.displayName}</h3>
      {removal && <p>{t("contactsUi.removeExplanation")}</p>}
      {request && <p>{requestLabel(request)}</p>}
      {request?.history.map((h, i) => (
        <div className="request-history" key={`${h.serverId}:${i}`}>
          <small>
            {t(`contactsUi.${h.direction === "outgoing" ? "sent" : "incoming"}`)} ·{" "}
            {new Date(h.createdAt * 1000).toLocaleString()}
          </small>
          <p>{h.message}</p>
        </div>
      ))}
      {discovery && (
        <label>
          {t("contactsUi.greeting")}
          <textarea
            value={greeting}
            onChange={(e) => setGreeting(e.target.value)}
            maxLength={4096}
            disabled={!!receipt}
          />
        </label>
      )}
      {request?.direction === "outgoing" && <p>{t("contactsUi.sentExplanation")}</p>}
      {receipt && <p role="status">{statusText}</p>}
      {error && <p role="alert">{error}</p>}
      <div className="contact-request-actions">
        {request && !receipt && (
          <button onClick={() => void change("delete")}>{t("contactsUi.deleteLocal")}</button>
        )}
        <button onClick={close}>{t("contactsUi.close")}</button>
        {receipt && retryOriginal && (
          <button disabled={busy} onClick={() => void submit(receipt)}>
            {t("contactsUi.retryOriginal")}
          </button>
        )}
        {receipt && (
          <button disabled={busy} onClick={() => void check()}>
            {t("contactsUi.checkStatus")}
          </button>
        )}
        {!receipt && (removal || discovery || request?.actionable) && (
          <button
            disabled={busy || (!!discovery && !greeting.trim())}
            onClick={() => void change(removal ? "remove" : discovery ? "send" : "accept")}
          >
            {removal
              ? t("contactsUi.removeContact")
              : discovery
              ? t("contactsUi.sendRequest")
              : t("contactsUi.accept")}
          </button>
        )}
      </div>
    </dialog>
  );
}

/** Contact removal lives with conversation details and preserves saved chat history. */
export function RemoveContact({
  connection,
  account,
  id,
  name,
}: {
  connection: Connection;
  account: string;
  id: string;
  name: string;
}) {
  const [show, setShow] = useState(false);
  return (
    <div className="detail-section">
      <button className="omni-action" onClick={() => setShow(true)}>
        {t("contactsUi.removeContact")}
      </button>
      {show && (
        <ContactRequestDialog
          connection={connection}
          account={account}
          selection={{ removeContact: { id, displayName: name } }}
          close={() => setShow(false)}
          changed={() => {
            void connection.session.refresh().catch(() => {});
          }}
        />
      )}
    </div>
  );
}
