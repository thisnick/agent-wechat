import { Command } from "cmdk";
import { useEffect, useMemo, useRef, useState } from "react";
import {
  NativeApiError,
  isNativeTombstone,
  type NativeContact,
  type NativeGroup,
  type NativeMessage,
  type NativeContactRequest,
  type NativeContactSearch,
  type NativeOperation,
} from "@agent-wechat/shared";
import type { Connection } from "./connection.ts";
import { t, formatLocale } from "./i18n.ts";
import { readableError } from "./session.ts";
import { HubAvatar } from "./avatars.tsx";
import { Icon } from "./ui.tsx";

import {
  contactDetailActions,
  readContactPrivacyWithPendingRetry,
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
    contacts: t("contactsUi.friends"),
    groups: t("contactsUi.groups"),
    messages: t("contactsUi.messages"),
    discovery: t("contactsUi.addSomeoneOnWechat"),
    otherPeople: t("contactsUi.otherPeople"),
  })[scope];
type SearchSource = "contacts" | "groups" | "messages" | "non_contact" | "unknown";
const sectionSources: Record<string, SearchSource[]> = {
  contacts: ["contacts"], groups: ["groups"], messages: ["messages"],
  otherPeople: ["non_contact", "unknown"], discovery: [],
};
type SearchPage = { nextCursor: string | null; loading: boolean; error: string };
function messageSearchDate(value: string | null): string | undefined {
  if (!value) return undefined;
  const date = new Date(/^\d+$/.test(value) ? Number(value) * 1000 : value);
  return Number.isFinite(date.getTime()) ? date.toISOString() : undefined;
}
type Hit = {
  id: string;
  name: string;
  subtitle?: string;
  kind: "contacts" | "groups" | "messages" | "discovery" | "otherPeople";
  source?: SearchSource;
  createdAt?: string;
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
    [pages, setPages] = useState<Partial<Record<SearchSource, SearchPage>>>({}),
    [requestLimits, setRequestLimits] = useState({ contacts: 15, otherPeople: 15 }),
    [remoteError, setRemoteError] = useState(""),
    [loading, setLoading] = useState(false);
  const readMore = useRef<(source: SearchSource) => void>(() => {});
  useEffect(() => {
    const abort = new AbortController();
    setHits([]);
    setPages({});
    setRequestLimits({ contacts: 15, otherPeople: 15 });
    const search = query.trim();
    const client = connection.session.client;
    const pending = new Set<SearchSource>();
    const cursors = new Map<SearchSource, string | null>();
    async function read(source: SearchSource, cursor?: string) {
      if (pending.has(source) || abort.signal.aborted) return;
      pending.add(source);
      setPages(current => ({ ...current, [source]: { nextCursor: cursors.get(source) ?? null, loading: true, error: "" } }));
      try {
        let rows: Hit[], nextCursor: string | null;
        if (source === "groups") {
          const page = await client.groups(account, { query: search, limit: 20, cursor }, abort.signal);
          rows = page.items.filter((g): g is NativeGroup => !isNativeTombstone(g)).map(g => ({
            id: g.id, name: g.displayName ?? g.id, subtitle: t("contactsUi.group"), kind: "groups", source,
          }));
          nextCursor = page.nextCursor;
        } else if (source === "messages") {
          const page = await client.searchMessages(account, { query: search, limit: 20, cursor }, abort.signal);
          rows = page.items.filter((m): m is NativeMessage => !isNativeTombstone(m)).map(m => {
            const snapshot = connection.session.snapshot();
            return { id: m.id, thread: m.threadId, kind: "messages", source,
              name: snapshot.account === account
                ? (snapshot.threads.find(thread => thread.id === m.threadId)?.displayName
                  ?? snapshot.contacts.find(contact => contact.id === m.threadId)?.displayName ?? m.threadId)
                : m.threadId,
              subtitle: m.body ?? "", createdAt: messageSearchDate(m.createdAt),
            };
          });
          nextCursor = page.nextCursor;
        } else {
          const relation = source === "contacts" ? "friend" : source;
          const page = await client.contacts(account, { query: search, limit: source === "contacts" ? 30 : 15, relation, cursor }, abort.signal);
          rows = page.items.filter((c): c is NativeContact => !isNativeTombstone(c) && c.relation === relation).map(c => ({
            id: c.id, name: c.displayName ?? c.id, source,
            kind: source === "contacts" ? "contacts" : "otherPeople",
            subtitle: t(source === "contacts" ? "contactsUi.saved" : source === "non_contact" ? "contactsUi.notFriendsYet" : "contactsUi.fromLocalHistory"),
          }));
          nextCursor = page.nextCursor;
        }
        if (abort.signal.aborted) return;
        cursors.set(source, nextCursor);
        setHits(current => {
          const retained = cursor ? current : current.filter(hit => hit.source !== source);
          const seen = new Set(retained.filter(hit => hit.source === source).map(hit => hit.id));
          return [...retained, ...rows.filter(hit => { if (seen.has(hit.id)) return false; seen.add(hit.id); return true; })];
        });
        setPages(current => ({ ...current, [source]: { nextCursor, loading: false, error: "" } }));
      } catch (error) {
        if (!abort.signal.aborted)
          setPages(current => ({ ...current, [source]: { nextCursor: cursors.get(source) ?? null, loading: false, error: readableError(error) } }));
      } finally { pending.delete(source); }
    }
    readMore.current = source => { void read(source, cursors.get(source) ?? undefined); };
    if (scope === "all" || scope === "contacts") void read("contacts");
    if (search && (scope === "all" || scope === "otherPeople")) {
      void read("non_contact");
      void read("unknown");
    }
    if (scope === "all" || scope === "groups") void read("groups");
    if (search && (scope === "all" || scope === "messages")) void read("messages");
    return () => abort.abort();
  }, [connection, account, query, scope]);
  useEffect(() => {
    const abort = new AbortController();
    const search = query.trim();
    setRemote(null);
    setRemoteError("");
    setLoading(false);
    const timer = setTimeout(() => {
      if (!search || !["all", "otherPeople"].includes(scope)) return;
      setLoading(true);
      void connection.session.client
        .searchContacts(account, search, abort.signal)
        .then((r) => {
          if (!abort.signal.aborted) setRemote(r);
        })
        .catch((e) => {
          if (!abort.signal.aborted) setRemoteError(e instanceof NativeApiError && e.code === "contact_discovery_rejected" ? t("contactsUi.discoveryRejected") : t("contactsUi.discoveryUnavailable"));
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
  const filteredRequests = requests.filter(r =>
    (scope === "all" || (scope === "contacts" && r.isContact) || (scope === "otherPeople" && !r.isContact)) &&
    (!query.trim() || `${r.displayName} ${r.peerId} ${r.history.map(h => h.message).join(" ")}`.toLowerCase().includes(query.trim().toLowerCase())),
  );
  const requestKind = (r: NativeContactRequest) => r.isContact ? "contacts" : "otherPeople";
  const visibleRequests = (["contacts", "otherPeople"] as const).flatMap(kind => filteredRequests.filter(r => requestKind(r) === kind).slice(0, requestLimits[kind]));
  const merged = hits.filter(
    (h) =>
      !["contacts", "otherPeople"].includes(h.kind) || !visibleRequests.some((r) => r.peerId === h.id),
  );
  for (const r of visibleRequests)
    merged.push({
      id: r.peerId,
      name: r.displayName,
      subtitle: requestLabel(r),
      kind: requestKind(r),
      request: r,
    });
  if (remote?.request) {
    const r = remote.request;
    if (!merged.some((h) => h.request?.id === r.id || h.id === r.peerId))
      merged.push({
        id: r.peerId,
        name: r.displayName,
        subtitle: requestLabel(r),
        kind: requestKind(r),
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
      kind: "discovery",
      discovery: remote,
    });
  function sectionHasMore(kind: string) {
    return sectionSources[kind].some(source => pages[source]?.nextCursor || pages[source]?.error) ||
      ((kind === "contacts" || kind === "otherPeople") && filteredRequests.filter(r => requestKind(r) === kind).length > requestLimits[kind]);
  }
  function sectionLoading(kind: string) { return sectionSources[kind].some(source => pages[source]?.loading); }
  function more(kind: string) {
    for (const source of sectionSources[kind])
      if (pages[source]?.nextCursor || pages[source]?.error) readMore.current(source);
    if (kind === "contacts" || kind === "otherPeople")
      setRequestLimits(current => ({ ...current, [kind]: current[kind] + 15 }));
  }
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
        {(["all", "contacts", "otherPeople", "groups", "messages"] as const).map((s) => (
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
        {(["contacts", "groups", "messages", "otherPeople", "discovery"] as const)
          .filter((kind) => merged.some((hit) => hit.kind === kind) || sectionLoading(kind) || sectionHasMore(kind))
          .map((kind) => (
            <Command.Group key={kind} heading={scopeLabel(kind)}>
              {merged
                .filter((h) => h.kind === kind)
                .sort((a, b) => kind === "otherPeople" ? a.name.localeCompare(b.name) || a.id.localeCompare(b.id) : 0)
                .map((h) => (
                  <Command.Item
                    key={`${kind}:${h.id}`}
                    value={`${kind}:${h.id}`}
                    onSelect={() => activate(h)}
                  >
                    <span className="omni-result-copy">
                      <strong>{h.name}</strong>
                      {h.subtitle && <span className={h.kind === "messages" ? "omni-message-snippet" : undefined}>{h.subtitle}</span>}
                      {h.createdAt && <time dateTime={h.createdAt}>{new Date(h.createdAt).toLocaleString(formatLocale(), { year: "numeric", month: "short", day: "numeric", hour: "numeric", minute: "2-digit" })}</time>}
                    </span>
                    {(h.kind === "contacts" || h.kind === "otherPeople") &&
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
                          <Icon name="panel" />
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
              {sectionSources[kind].map(source => pages[source]?.error && <p key={source} role="alert">{pages[source]!.error}</p>)}
              {(sectionHasMore(kind) || sectionLoading(kind)) && <button className="omni-load-more"
                aria-label={t("contactsUi.loadMoreSection", { section: scopeLabel(kind) })}
                disabled={sectionLoading(kind)}
                onKeyDown={e => { if (e.key === "Enter" || e.key === " ") e.stopPropagation(); }}
                onClick={() => more(kind)}>
                {t(sectionLoading(kind) ? "contactsUi.loadingResults" : "contactsUi.loadMore")}
              </button>}
            </Command.Group>
          ))}
        {!merged.length && !Object.values(pages).some(page => page?.loading || page?.error) && !loading && <Command.Empty>{t("main.noMatches")}</Command.Empty>}
      </Command.List>
      <div className="omni-foot" aria-live="polite">
        {loading
          ? t("contactsUi.searchingWechat")
          : remoteError
            ? remoteError
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
  review,
}: {
  connection: Connection;
  account: string;
  selection: RequestSelection;
  close: () => void;
  changed: () => void;
  open?: (id: string) => void;
  remove?: (contact: { id: string; displayName: string }) => void;
  review: (selection: RequestSelection) => void;
}) {
  const ref = useRef<HTMLDialogElement>(null);
  const actions = useMemo(
    () => contactDetailActions(connection, account),
    [connection, account],
  );
  const lookupAbort = useRef<AbortController | null>(null);
  useEffect(() => () => lookupAbort.current?.abort(), [connection, account]);
  const request = "request" in selection ? selection.request : null;
  const discovery = "discovery" in selection ? selection.discovery : null;
  const contact = "contact" in selection ? selection.contact : null;
  const removal = "removeContact" in selection ? selection.removeContact : null;
  const savedContact = contact && connection.session.snapshot().contacts.find((row) => row.id === contact.id);
  const friendId = (savedContact?.relation === "friend" ? contact?.id : null) ?? (request?.isContact ? request.peerId : null);
  const relationshipLabel = friendId || removal
    ? t("contactsUi.saved")
    : request ? requestLabel(request)
    : savedContact?.relation === "non_contact" ? t("contactsUi.notFriendsYet")
    : null;
  const name =
    contact?.displayName ??
    removal?.displayName ??
    request?.displayName ??
    discovery?.displayName ??
    "";
  const peer = contact?.id ?? friendId ?? removal?.id ?? request?.peerId;
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
  const [privacyWaiting, setPrivacyWaiting] = useState(false);
  const [busy, setBusy] = useState(false),
    [error, setError] = useState(""),
    [retryOriginal, setRetryOriginal] = useState(false);
  const [permissionDenied, setPermissionDenied] = useState(false);
  async function findForRequest() {
    if (!contact || busy) return;
    lookupAbort.current?.abort();
    const abort = new AbortController();
    lookupAbort.current = abort;
    setBusy(true);
    setError("");
    try {
      const result = await connection.session.client.searchContacts(
        account, savedContact?.alias || contact.id, abort.signal,
      );
      if (abort.signal.aborted || connection.session.snapshot().account !== account) return;
      if (result.recipientRef) review({ discovery: result });
      else setError(t("contactsUi.requestUnavailable"));
    } catch (error) {
      if (!abort.signal.aborted) setError(error instanceof NativeApiError && error.code === "contact_discovery_rejected"
        ? t("contactsUi.discoveryRejected") : t("contactsUi.discoveryUnavailable"));
    } finally {
      if (!abort.signal.aborted) setBusy(false);
    }
  }
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
    setPrivacyWaiting(false);
    if (!friendId) return;
    const abort = new AbortController();
    setPrivacyLoading(true);
    const current = () => {
      const state = connection.session.snapshot();
      return (
        state.account === account &&
        state.signInRequired !== account &&
        !state.accounts.some(
          (item) => item.id === account && item.authenticated === false,
        )
      );
    };
    const unsubscribe = connection.session.subscribe(() => {
      if (!current()) abort.abort();
    });
    if (!current()) abort.abort();
    void readContactPrivacyWithPendingRetry(
      (signal) => actions.readPrivacy(friendId, signal),
      abort.signal,
      {
        pending: () => {
          if (!abort.signal.aborted) setPrivacyWaiting(true);
        },
      },
    )
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
              : t("contactsUi.privacyReadFailed"),
          );
      })
      .finally(() => {
        if (!abort.signal.aborted) {
          setPrivacyLoading(false);
          setPrivacyWaiting(false);
        }
      });
    return () => {
      unsubscribe();
      abort.abort();
    };
  }, [actions, friendId, privacyRevision, connection, account]);
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
        ? receipt.action === "privacy"
          ? t(
              privacyLoading
                ? "contactsUi.privacyWaiting"
                : savedPrivacy && !privacyError
                  ? "contactsUi.privacySaved"
                  : "contactsUi.privacyReadbackUnavailable",
            )
          : t(
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
  const privacyDirty = !!savedPrivacy && !sameContactPrivacy(privacy, savedPrivacy.privacy);
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
        {!removal && (request || friendId) && <details className="contact-more">
          <summary aria-label={t("contactsUi.moreActions")}><Icon name="more" /></summary>
          <div className="contact-more-menu">
            {request && !receipt && <button disabled={busy} onClick={() => { if (window.confirm(t("contactsUi.deleteExplanation"))) void change("delete"); }}>{t("contactsUi.deleteLocal")}</button>}
            {friendId && remove && savedPrivacy && !privacyLoading && !privacyError && !permissionDenied && <button disabled={locked} onClick={() => remove({ id: friendId, displayName: name })}>{t("contactsUi.removeContact")}</button>}
          </div>
        </details>}
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
          {relationshipLabel && <p>{relationshipLabel}</p>}
        </div>
      </div>
      {removal && <p>{t("contactsUi.removeExplanation")}</p>}
      {!friendId && request?.actionable && <div className="contact-latest-request"><small>{t("contactsUi.incoming")}</small><p>{request.message}</p></div>}
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
          {privacyLoading && !privacySucceeded && (
            <p role="status">
              {t(
                privacyWaiting
                  ? "contactsUi.privacyWaiting"
                  : "contactsUi.privacyLoading",
              )}
            </p>
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
          {friendId && privacyError && (
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
      {request && request.history.length > 0 && (
        <details
          className="contact-detail-history"
          aria-label={t("contactsUi.requestHistory")}
        >
          <summary>{t("contactsUi.requestHistory")} · {request.history.length}</summary>
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
        </details>
      )}
      {request?.direction === "outgoing" && !friendId && (
        <p>{t("contactsUi.sentExplanation")}</p>
      )}
      {receipt && (
        <p role="status" className="contact-change-status">
          {statusText}
        </p>
      )}
      {error && <p role="alert">{error}</p>}
      <div className="contact-request-actions">
        {friendId && privacyDirty && !locked && savedPrivacy && <button onClick={() => setPrivacy(savedPrivacy.privacy)}>{t("contactsUi.cancel")}</button>}
        {(friendId || contact?.id) && open && !privacyDirty && (
          <button
            onClick={() => {
              close();
              open(friendId ?? contact!.id);
            }}
          >
            <Icon name="chat" />
            {t("contactsUi.message")}
          </button>
        )}
        {contact && !friendId && !receipt && (
          <button className="contact-primary-action" disabled={busy} onClick={() => void findForRequest()}>
            {t("contactsUi.addContact")}
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
        {friendId && privacyDirty && (
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
          review={setSelection}
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
