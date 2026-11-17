import { Command } from "cmdk";
import { useEffect, useMemo, useRef, useState } from "react";
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

import {
  contactDetailActions,
  contactChangeActions,
  restoreContactChange,
  defaultContactPrivacy,
  sameContactPrivacy,
  type ContactChange,
  type ContactPrivacy,
  type ContactPrivacySnapshot,
} from "./contact-privacy.ts";
export type RequestSelection =
  | { request: NativeContactRequest }
  | { discovery: NativeContactSearch }
  | { contact: { id: string; displayName: string } }
  | { removeContact: { id: string; displayName: string } };
export function contactSelectionKey(selection: RequestSelection) {
  return "request" in selection
    ? selection.request.id
    : "discovery" in selection
      ? (selection.discovery.recipientRef ?? selection.discovery.displayName)
      : "contact" in selection
        ? selection.contact.id
        : `remove:${selection.removeContact.id}`;
}
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
            controller.signal,
          );
          if (controller.signal.aborted) return;
          all.push(
            ...page.items.filter(
              (item): item is NativeContactRequest => !isNativeTombstone(item),
            ),
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
      if (
        a === account &&
        ["friend_request.updated", "contact.updated"].includes(event.kind)
      )
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
    ? t("contactsUi.details")
    : request.actionable
      ? t("contactsUi.review")
      : t("contactsUi.view");
}
export function FriendRequests({
  connection,
  account,
  requests,
  review,
  back,
}: {
  connection: Connection;
  account: string;
  back: () => void;
  requests: ReturnType<typeof useContactRequests>;
  review: (selection: RequestSelection) => void;
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
          <button
            key={d}
            aria-pressed={direction === d}
            onClick={() => setDirection(d)}
          >
            {t(`contactsUi.${d === "outgoing" ? "sent" : d}`)}
          </button>
        ))}
      </div>
      {requests.error && (
        <p role="alert">
          {requests.error}{" "}
          <button onClick={requests.refresh}>{t("main.refresh")}</button>
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
              onClick={() => review({ request: r })}
            >
              <strong>{r.displayName}</strong>
              <span>
                {requestLabel(r)} ·{" "}
                {new Date(r.updatedAt * 1000).toLocaleString()}
              </span>
              <span>{r.message}</span>
            </button>
            <button
              className="omni-action"
              onClick={() => review({ request: r })}
            >
              {requestAction(r)}
            </button>
          </div>
        ))}
      {!requests.items.length && !requests.error && (
        <p>{t("contactsUi.noRequests")}</p>
      )}
    </main>
  );
}
const scopeLabel = (scope: string) =>
  ({
    all: t("contactsUi.all"),
    contacts: t("contactsUi.contacts"),
    groups: t("contactsUi.groups"),
    messages: t("contactsUi.messages"),
  })[scope];
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
            ? client.contacts(
                account,
                { query: search, limit: 30 },
                abort.signal,
              )
            : null,
          scope === "all" || scope === "groups"
            ? client.groups(account, { query: search, limit: 20 }, abort.signal)
            : null,
          search && (scope === "all" || scope === "messages")
            ? client.searchMessages(
                account,
                { query: search, limit: 20 },
                abort.signal,
              )
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
                  ? (connection.session
                      .snapshot()
                      .threads.find((t) => t.id === m.threadId)?.displayName ??
                    connection.session
                      .snapshot()
                      .contacts.find((c) => c.id === m.threadId)?.displayName ??
                    m.threadId)
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
            .includes(query.toLowerCase()),
      )
    : [];
  const merged = hits.filter(
    (h) =>
      h.kind !== "contacts" || !filteredRequests.some((r) => r.peerId === h.id),
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
  const details = (h: Hit) => {
    if (h.discovery) review({ discovery: h.discovery });
    else if (h.request) review({ request: h.request });
    else review({ contact: { id: h.id, displayName: h.name } });
  };
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
          <button
            key={s}
            aria-pressed={scope === s}
            onClick={() => setScope(s)}
          >
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
                    {h.kind === "contacts" &&
                    !h.discovery &&
                    (!h.request || h.request.isContact) ? (
                      <span className="omni-result-actions">
                        <button
                          className="omni-action"
                          onClick={(e) => {
                            e.stopPropagation();
                            activate(h);
                          }}
                        >
                          <Icon name="chat" />
                          {t("contactsUi.message")}
                        </button>
                        <button
                          className="omni-action"
                          aria-label={t("contactsUi.detailsFor", {
                            name: h.name,
                          })}
                          onKeyDown={(e) => {
                            if (e.key === "Enter" || e.key === " ")
                              e.stopPropagation();
                          }}
                          onClick={(e) => {
                            e.stopPropagation();
                            details(h);
                          }}
                        >
                          {t("contactsUi.details")}
                        </button>
                      </span>
                    ) : (
                      <span className="omni-action">
                        {h.discovery ? (
                          h.discovery.recipientRef ? (
                            t("main.findPerson")
                          ) : (
                            t("contactsUi.view")
                          )
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
                    )}
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
function PrivacyFields({
  value,
  disabled,
  change,
}: {
  value: ContactPrivacy;
  disabled: boolean;
  change: (value: ContactPrivacy) => void;
}) {
  return (
    <fieldset className="contact-privacy-fields" disabled={disabled}>
      <legend>{t("contactsUi.privacy")}</legend>
      <div className="contact-access-options">
        {[false, true].map((chatsOnly) => (
          <label key={String(chatsOnly)}>
            <input
              type="radio"
              name="contact-access"
              checked={value.chatsOnly === chatsOnly}
              onChange={() => change({ ...value, chatsOnly })}
            />
            <span>
              {t(chatsOnly ? "contactsUi.chatsOnly" : "contactsUi.fullAccess")}
            </span>
          </label>
        ))}
      </div>
      {!value.chatsOnly && (
        <div className="contact-moments-options">
          {(["hideMyPosts", "hideTheirPosts"] as const).map((key) => (
            <label key={key}>
              <span>
                {t(
                  key === "hideMyPosts"
                    ? "contactsUi.hideMyPosts"
                    : "contactsUi.hideTheirPosts",
                )}
              </span>
              <input
                type="checkbox"
                role="switch"
                checked={value[key]}
                onChange={(e) => change({ ...value, [key]: e.target.checked })}
              />
            </label>
          ))}
        </div>
      )}
    </fieldset>
  );
}
export function ContactDetailDialog({
  connection,
  account,
  selection,
  close,
  changed,
  open,
  remove,
}: {
  connection: Connection;
  account: string;
  selection: RequestSelection;
  close: () => void;
  changed: () => void;
  open?: (id: string) => void;
  remove?: (contact: { id: string; displayName: string }) => void;
}) {
  const ref = useRef<HTMLDialogElement>(null);
  const actions = useMemo(
    () => contactDetailActions(connection, account),
    [connection, account],
  );
  const request = "request" in selection ? selection.request : null;
  const discovery = "discovery" in selection ? selection.discovery : null;
  const contact = "contact" in selection ? selection.contact : null;
  const removal = "removeContact" in selection ? selection.removeContact : null;
  const friendId = contact?.id ?? (request?.isContact ? request.peerId : null);
  const name =
    contact?.displayName ??
    removal?.displayName ??
    request?.displayName ??
    discovery?.displayName ??
    "";
  const peer = friendId ?? removal?.id ?? request?.peerId;
  const [receiptId] = useState(
    () =>
      `contact-change:${removal ? `remove:${removal.id}` : contact ? `privacy:${contact.id}` : request ? `${request.id}:${request.version}` : discovery?.recipientRef}`,
  );
  const [receipt, setReceipt] = useState<ContactChange | undefined>(() => {
    const saved = restoreContactChange(
      connection.cache.readState<ContactChange>(account, receiptId) ??
        undefined,
      {
        requestId: request?.id,
        version: request?.version,
        contactId: removal?.id,
      },
    );
    return (removal || contact) &&
      saved?.operation &&
      ["succeeded", "failed", "cancelled"].includes(saved.operation.state)
      ? undefined
      : saved;
  });
  const [greeting, setGreeting] = useState(() =>
    receipt?.action === "send" ? receipt.input.message : "",
  );
  const [privacy, setPrivacy] = useState<ContactPrivacy>(() =>
    receipt?.action === "send"
      ? (receipt.input.privacy ?? defaultContactPrivacy())
      : receipt?.action === "accept"
        ? (receipt.privacy ?? defaultContactPrivacy())
        : receipt?.action === "privacy"
          ? receipt.input.privacy
          : defaultContactPrivacy(),
  );
  const [savedPrivacy, setSavedPrivacy] =
    useState<ContactPrivacySnapshot | null>(null);
  const [privacyLoading, setPrivacyLoading] = useState(false),
    [privacyError, setPrivacyError] = useState("");
  const [privacyRevision, setPrivacyRevision] = useState(0);
  const [busy, setBusy] = useState(false),
    [error, setError] = useState(""),
    [retryOriginal, setRetryOriginal] = useState(false);
  const [permissionDenied, setPermissionDenied] = useState(false);
  const ended =
    !!receipt?.operation &&
    ["succeeded", "failed", "cancelled", "needs_review"].includes(
      receipt.operation.state,
    );
  const privacySucceeded =
    receipt?.action === "privacy" && receipt.operation?.state === "succeeded";
  const completedFriendAction =
    !!friendId &&
    receipt?.operation?.state === "succeeded" &&
    ["accept", "delete"].includes(receipt.action);
  const editableReceipt = privacySucceeded || completedFriendAction;
  const locked = busy || (!!receipt && !editableReceipt);
  useEffect(() => {
    const dialog = ref.current!;
    dialog.showModal();
    return () => dialog.close();
  }, []);
  useEffect(() => {
    setSavedPrivacy(null);
    setPrivacyError("");
    if (!friendId) return;
    const abort = new AbortController();
    setPrivacyLoading(true);
    void actions
      .readPrivacy(friendId, abort.signal)
      .then((value) => {
        if (!abort.signal.aborted) {
          setSavedPrivacy(value);
          if (!(receipt?.action === "privacy" && !privacySucceeded))
            setPrivacy(value.privacy);
        }
      })
      .catch((e) => {
        if (!abort.signal.aborted)
          setPrivacyError(
            e instanceof NativeApiError && e.status === 403
              ? t("contactsUi.privacyPermission")
              : readableError(e),
          );
      })
      .finally(() => {
        if (!abort.signal.aborted) setPrivacyLoading(false);
      });
    return () => abort.abort();
  }, [actions, friendId, privacyRevision]);
  const save = (value: ContactChange) => {
    connection.cache.writeState(account, receiptId, value);
    setReceipt(value);
  };
  const finished = (pending: ContactChange, operation: NativeOperation) => {
    save({ ...pending, operation });
    if (
      ["succeeded", "failed", "cancelled", "needs_review"].includes(
        operation.state,
      )
    )
      changed();
    if (pending.action === "privacy" && operation.state === "succeeded") {
      setSavedPrivacy(null);
      setPrivacyLoading(true);
      setPrivacyRevision((n) => n + 1);
    }
  };
  async function submit(pending: ContactChange) {
    if (busy) return;
    setBusy(true);
    setError("");
    setRetryOriginal(false);
    try {
      await connection.cache.flush();
      finished(pending, await actions.submit(pending));
    } catch (e) {
      setError(readableError(e));
      if (
        e instanceof NativeApiError &&
        e.status === 403 &&
        ["send", "accept", "privacy"].includes(pending.action)
      ) {
        setPermissionDenied(true);
        setError(t("contactsUi.privacyPermission"));
      }
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
        if (pending.action === "privacy") setPrivacyRevision((n) => n + 1);
      }
    } finally {
      setBusy(false);
    }
  }
  async function change(action: ContactChange["action"]) {
    if (
      locked ||
      (receipt && !editableReceipt) ||
      (permissionDenied && ["send", "accept", "privacy"].includes(action))
    )
      return;
    const key = crypto.randomUUID();
    let pending: ContactChange;
    if (action === "send" && discovery?.recipientRef)
      pending = {
        key,
        action,
        input: {
          recipientRef: discovery.recipientRef,
          message: greeting,
          privacy: { ...privacy },
        },
      };
    else if (action === "accept" && request?.actionable)
      pending = {
        key,
        action,
        requestId: request.id,
        version: request.version,
        privacy: { ...privacy },
      };
    else if (action === "delete" && request)
      pending = {
        key,
        action,
        requestId: request.id,
        version: request.version,
      };
    else if (action === "remove" && removal)
      pending = { key, action, contactId: removal.id };
    else if (
      action === "privacy" &&
      friendId &&
      savedPrivacy &&
      !privacyError &&
      !privacyLoading
    )
      pending = {
        key,
        action,
        contactId: friendId,
        input: {
          privacy: { ...privacy },
          expectedVersion: savedPrivacy.version,
        },
      };
    else return;
    save(pending);
    await submit(pending);
  }
  async function check() {
    if (!receipt || busy) return;
    setBusy(true);
    setError("");
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
          contactChangeActions[receipt.action] ||
        (receipt.operation && operation.id !== receipt.operation.id)
      )
        throw Error(t("contactsUi.operationMismatch"));
      finished(receipt, operation);
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
  const statusText =
    !receipt?.operation || receipt.operation.state === "needs_review"
      ? t("contactsUi.checkBeforeRetry")
      : receipt.operation.state === "succeeded"
        ? t(
            (
              {
                send: "contactsUi.sent",
                accept: "contactsUi.accepted",
                delete: "contactsUi.historyDeleted",
                remove: "contactsUi.contactRemoved",
                privacy: "contactsUi.privacySaved",
              } as const
            )[receipt.action],
          )
        : receipt.operation.state === "failed"
          ? t("contactsUi.failed")
          : receipt.operation.state === "cancelled"
            ? t("contactsUi.cancelled")
            : t("contactsUi.working");
  const title = removal
    ? t("contactsUi.removeContact")
    : t("contactsUi.contactDetails");
  const showPrivacy =
    !removal &&
    (!!friendId || !!discovery?.recipientRef || !!request?.actionable);
  return (
    <dialog
      ref={ref}
      className="contact-request-dialog contact-detail-dialog"
      aria-label={title}
      onCancel={(e) => {
        e.preventDefault();
        close();
      }}
      onClose={close}
    >
      <div className="contact-request-heading">
        <h2>{title}</h2>
        <button onClick={close} aria-label={t("contactsUi.close")}>
          <Icon name="close" />
        </button>
      </div>
      <div className="contact-detail-identity">
        <HubAvatar
          connection={connection}
          account={account}
          username={peer ?? name}
          name={name}
        />
        <div>
          <h3>{name}</h3>
          <p>
            {friendId || removal
              ? t("contactsUi.saved")
              : request
                ? requestLabel(request)
                : t("contactsUi.onWechat")}
          </p>
        </div>
      </div>
      {removal && <p>{t("contactsUi.removeExplanation")}</p>}
      {request && request.history.length > 0 && (
        <section
          className="contact-detail-history"
          aria-label={t("contactsUi.requestHistory")}
        >
          <h4>{t("contactsUi.requestHistory")}</h4>
          {request.history.map((h, i) => (
            <div className="request-history" key={`${h.serverId}:${i}`}>
              <small>
                {t(
                  `contactsUi.${h.direction === "outgoing" ? "sent" : "incoming"}`,
                )}{" "}
                · {new Date(h.createdAt * 1000).toLocaleString()}
              </small>
              <p>{h.message}</p>
            </div>
          ))}
        </section>
      )}
      {discovery && !discovery.recipientRef && (
        <p>{t("contactsUi.requestUnavailable")}</p>
      )}
      {discovery?.recipientRef && (
        <label className="contact-greeting">
          {t("contactsUi.greeting")}
          <textarea
            value={greeting}
            onChange={(e) => setGreeting(e.target.value)}
            maxLength={4096}
            disabled={locked}
          />
        </label>
      )}
      {showPrivacy && (
        <section className="contact-detail-privacy">
          <p>
            {t(
              friendId
                ? "contactsUi.privacyForContact"
                : "contactsUi.privacyBeforeRequest",
            )}
          </p>
          {privacyLoading && (
            <p role="status">{t("contactsUi.privacyLoading")}</p>
          )}
          {privacyError && (
            <div role="alert">
              <p>{privacyError}</p>
              <p>{t("contactsUi.privacyUnavailable")}</p>
            </div>
          )}
          {(!friendId || savedPrivacy || receipt?.action === "privacy") && (
            <PrivacyFields
              value={privacy}
              disabled={
                locked ||
                permissionDenied ||
                (!!friendId &&
                  (!savedPrivacy || privacyLoading || !!privacyError))
              }
              change={(value) => {
                if (editableReceipt) {
                  connection.cache.writeState(account, receiptId, null);
                  setReceipt(undefined);
                }
                setPrivacy(value);
              }}
            />
          )}
          {friendId && (
            <button
              className="omni-action"
              disabled={busy || privacyLoading || (!!receipt && !ended)}
              onClick={() => setPrivacyRevision((n) => n + 1)}
            >
              {t("contactsUi.refreshPrivacy")}
            </button>
          )}
        </section>
      )}
      {request?.direction === "outgoing" && !friendId && (
        <p>{t("contactsUi.sentExplanation")}</p>
      )}
      {request && !receipt && (
        <p className="contact-delete-note">
          {t("contactsUi.deleteExplanation")}
        </p>
      )}
      {receipt && (
        <p role="status" className="contact-change-status">
          {statusText}
        </p>
      )}
      {error && <p role="alert">{error}</p>}
      <div className="contact-request-actions">
        {request && !receipt && (
          <button
            className="contact-delete-action"
            disabled={busy}
            onClick={() => void change("delete")}
          >
            {t("contactsUi.deleteLocal")}
          </button>
        )}
        <button onClick={close}>{t("contactsUi.close")}</button>
        {friendId && open && (
          <button
            onClick={() => {
              close();
              open(friendId);
            }}
          >
            <Icon name="chat" />
            {t("contactsUi.message")}
          </button>
        )}
        {receipt && retryOriginal && (
          <button disabled={busy} onClick={() => void submit(receipt)}>
            {t("contactsUi.retryOriginal")}
          </button>
        )}
        {receipt && !privacySucceeded && (
          <button disabled={busy} onClick={() => void check()}>
            {t("contactsUi.checkStatus")}
          </button>
        )}
        {friendId && (
          <button
            className="contact-primary-action"
            disabled={
              locked ||
              permissionDenied ||
              !savedPrivacy ||
              privacyLoading ||
              !!privacyError ||
              sameContactPrivacy(privacy, savedPrivacy.privacy)
            }
            onClick={() => void change("privacy")}
          >
            {t("contactsUi.savePrivacy")}
          </button>
        )}
        {!receipt &&
          (removal || discovery?.recipientRef || request?.actionable) && (
            <button
              className="contact-primary-action"
              disabled={
                busy ||
                (!removal && permissionDenied) ||
                (!!discovery && !greeting.trim())
              }
              onClick={() =>
                void change(removal ? "remove" : discovery ? "send" : "accept")
              }
            >
              {removal
                ? t("contactsUi.removeContact")
                : discovery
                  ? t("contactsUi.sendRequest")
                  : t("contactsUi.accept")}
            </button>
          )}
      </div>
      {friendId &&
        remove &&
        savedPrivacy &&
        !privacyLoading &&
        !privacyError &&
        !permissionDenied && (
          <div className="contact-detail-danger">
            <button
              className="omni-action"
              disabled={locked}
              onClick={() => remove({ id: friendId, displayName: name })}
            >
              {t("contactsUi.removeContact")}
            </button>
          </div>
        )}
    </dialog>
  );
}

/** Conversation details opens the same card and uses its membership/permission checks. */
export function ContactDetails({
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
  const [selection, setSelection] = useState<RequestSelection | null>(null);
  return (
    <div className="detail-section">
      <button
        className="omni-action"
        onClick={() => setSelection({ contact: { id, displayName: name } })}
      >
        {t("contactsUi.details")}
      </button>
      {selection && (
        <ContactDetailDialog
          key={contactSelectionKey(selection)}
          connection={connection}
          account={account}
          selection={selection}
          close={() => setSelection(null)}
          open={() => setSelection(null)}
          remove={(contact) => setSelection({ removeContact: contact })}
          changed={() => {
            void connection.session.refresh().catch(() => {});
          }}
        />
      )}
    </div>
  );
}
