import { Command } from "cmdk";
import { useEffect, useId, useMemo, useRef, useState, useSyncExternalStore, type ReactNode } from "react";
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
import { pause, readableError } from "./session.ts";
import { HubAvatar } from "./avatars.tsx";
import { Icon } from "./ui.tsx";
import { ComposerHelp } from "./composer-help.tsx";

import {
  contactDetailActions,
  readContactPrivacyWithPendingRetry,
  contactChangeActions,
  restoreContactChange,
  defaultContactPrivacy,
  sameContactPrivacy,
  MAX_CONTACT_REMARK_BYTES,
  REMARK_POLL_LIMIT,
  REMARK_POLL_MS,
  PRIVACY_AUTOSAVE_MS,
  REMARK_AUTOSAVE_MS,
  SAVED_FLASH_MS,
  type ContactChange,
  type ContactPrivacy,
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
type SaveState = "saving" | "syncing" | "unsynced" | "saved" | "failed" | "uncertain";
/** A field's autosave state, shown at the end of its heading. */
function SaveStatus({
  state,
  retry,
  check,
  retryOriginal,
  busy,
}: {
  state: SaveState | null;
  retry?: () => void;
  check: () => void;
  retryOriginal?: () => void;
  busy: boolean;
}) {
  return (
    <span className={`contact-save-status${state ? ` contact-save-${state}` : ""}`} role="status">
      {state && (
        <>
          {state === "saving" || state === "syncing" ? (
            <span className="contact-save-spinner" aria-hidden="true" />
          ) : (
            <Icon name={state === "saved" ? "check" : state === "unsynced" ? "info" : "alert"} />
          )}
          {t(
            ({
              saving: "contactsUi.saving",
              syncing: "contactsUi.syncing",
              unsynced: "contactsUi.notSynced",
              saved: "contactsUi.savedField",
              failed: "contactsUi.notSaved",
              uncertain: "contactsUi.notConfirmed",
            } as const)[state],
          )}
          {state === "failed" && retry && (
            <button type="button" onClick={retry}>{t("contactsUi.retry")}</button>
          )}
          {state === "uncertain" && (
            <button type="button" disabled={busy} onClick={check}>{t("contactsUi.check")}</button>
          )}
          {state === "uncertain" && retryOriginal && (
            <button type="button" disabled={busy} onClick={retryOriginal}>{t("contactsUi.retry")}</button>
          )}
        </>
      )}
    </span>
  );
}
function PrivacyFields({
  value,
  disabled,
  change,
  hint,
  status,
}: {
  value: ContactPrivacy;
  disabled: boolean;
  change: (value: ContactPrivacy) => void;
  hint: ReactNode;
  status?: ReactNode;
}) {
  return (
    <fieldset className="contact-privacy-fields" disabled={disabled} aria-label={t("contactsUi.privacy")}>
      {/* Controls in the first legend stay usable while the fieldset is disabled. */}
      <legend>
        {t("contactsUi.privacy")}
        <ComposerHelp label={t("contactsUi.aboutPrivacy")} icon="info" className="contact-hint">{hint}</ComposerHelp>
        {status}
      </legend>
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
  const refreshAbort = useRef<AbortController | null>(null);
  useEffect(() => () => {
    lookupAbort.current?.abort();
    refreshAbort.current?.abort();
  }, [connection, account]);
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
  const privacyCache = connection.session.contactPrivacy;
  const savedPrivacy = useSyncExternalStore(privacyCache.subscribe, () => privacyCache.get(account, friendId));
  const privacyDenied = useSyncExternalStore(privacyCache.subscribe, () => privacyCache.denied(account));
  // Contact events (our own saves emit one) drop the cached snapshot and reload it.
  // Keep rendering the last one meanwhile so the card doesn't collapse and regrow;
  // saving still waits for the fresh version in `savedPrivacy`.
  const lastPrivacy = useRef(savedPrivacy);
  if (savedPrivacy) lastPrivacy.current = savedPrivacy;
  const shownPrivacy = privacyDenied ? undefined : savedPrivacy ?? lastPrivacy.current;
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
  const [initialPrivacy, setInitialPrivacy] = useState<ContactPrivacy>(() =>
    receipt?.action === "send"
      ? (receipt.input.privacy ?? defaultContactPrivacy())
      : receipt?.action === "accept"
        ? (receipt.privacy ?? defaultContactPrivacy())
        : receipt?.action === "privacy"
          ? receipt.input.privacy
          : defaultContactPrivacy(),
  );
  const [privacyLoading, setPrivacyLoading] = useState(false),
    [privacyReadError, setPrivacyError] = useState("");
  // Friend edits save on their own, one change per request. A pending privacy edit
  // keeps only the keys the person changed, applied over the latest saved or
  // submitted settings, so it never reverts a change made on another device.
  const [privacyEdits, setPrivacyEdits] = useState<Partial<ContactPrivacy>>({});
  const privacyReceipt = receipt?.action === "privacy" ? receipt : undefined;
  // A submitted change shows until its saved settings are read back.
  const privacySubmitted = privacyReceipt
    && (privacyReceipt.operation?.state !== "succeeded" || !savedPrivacy || privacyLoading)
    ? privacyReceipt.input.privacy
    : undefined;
  const privacyBase = privacySubmitted ?? shownPrivacy?.privacy ?? initialPrivacy;
  const privacy = friendId ? { ...privacyBase, ...privacyEdits } : initialPrivacy;
  const privacyEdited = !!friendId && !sameContactPrivacy(privacy, privacyBase);
  const [remarkDraft, setRemarkDraft] = useState<string | null>(null);
  // Leaving the field saves at once instead of waiting for the typing pause.
  const [remarkFlush, setRemarkFlush] = useState(false);
  const remarkInput = useRef<HTMLInputElement>(null);
  // The last accepted remark, shown until a read newer than its save confirms it.
  const [confirmingRemark, setConfirmingRemark] = useState<{ value: string | null; over: object | null } | null>(null);
  const remarkReceipt = receipt?.action === "remark" ? receipt : undefined;
  const remarkSubmitted = remarkReceipt && remarkReceipt.operation?.state !== "succeeded"
    ? remarkReceipt.input.remark ?? ""
    : undefined;
  const remarkConfirming = !!confirmingRemark
    && (!savedPrivacy || savedPrivacy === confirmingRemark.over || savedPrivacy.remarkPending);
  const remarkBase = remarkSubmitted
    ?? (remarkConfirming ? confirmingRemark?.value ?? "" : undefined)
    ?? (shownPrivacy ? shownPrivacy.remark : savedContact?.remark)
    ?? "";
  const remarkValue = remarkDraft ?? remarkBase;
  const remarkEdited = remarkDraft !== null && remarkDraft.trim() !== remarkBase;
  const remarkTooLong = new TextEncoder().encode(remarkValue.trim()).length > MAX_CONTACT_REMARK_BYTES;
  const remarkPolls = useRef(0);
  const remarkFieldId = useId();
  // A failed poll changes no snapshot, so it bumps this to schedule the next one.
  const [remarkPollFailures, setRemarkPollFailures] = useState(0);
  // A field's failure shows beside its heading and pauses its autosave until the
  // field is edited again or retried.
  const [fieldError, setFieldError] = useState<{ field: "privacy" | "remark"; message: string; retry: boolean } | null>(null);
  const [savedFlash, setSavedFlash] = useState<{ privacy: boolean; remark: boolean }>({ privacy: false, remark: false });
  // Edits confirmed in this card; only these say "Saved".
  const savedHere = useRef(new Set<string>());
  const staleRetries = useRef(0);
  // A point read is newer than the catalog row. Name the contact by an accepted
  // remark, or by the nickname after a clear the catalog hasn't caught up with.
  const liveRemark = !friendId ? undefined
    : remarkConfirming ? confirmingRemark?.value ?? null
    : shownPrivacy && !shownPrivacy.remarkPending ? shownPrivacy.remark
    : undefined;
  const shownRemark = liveRemark !== undefined ? liveRemark : savedContact?.remark;
  const remarkName = liveRemark
    || (liveRemark === null && savedContact?.remark ? savedContact.nickname : null)
    || undefined;
  const name =
    remarkName ??
    savedContact?.displayName ??
    contact?.displayName ??
    removal?.displayName ??
    request?.displayName ??
    discovery?.displayName ??
    "";
  const privacyError = privacyDenied ? t("contactsUi.privacyReadPermission") : privacyReadError;
  const [contactRefreshing, setContactRefreshing] = useState(false);
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
  const contactEditSucceeded =
    privacySucceeded || (receipt?.action === "remark" && receipt.operation?.state === "succeeded");
  const completedFriendAction =
    !!friendId &&
    receipt?.operation?.state === "succeeded" &&
    ["accept", "delete"].includes(receipt.action);
  const editableReceipt = contactEditSucceeded || completedFriendAction;
  const locked = busy || (!!receipt && !editableReceipt);
  const editReceipt = receipt?.action === "privacy" || receipt?.action === "remark";
  const operationRunning = !!receipt?.operation && ["queued", "running"].includes(receipt.operation.state);
  // A lost response or needs_review: only Check (or Retry) may continue it.
  const uncertainEdit = editReceipt && !busy && !operationRunning && !contactEditSucceeded;
  const editingLocked = permissionDenied || contactRefreshing || !!privacyError || uncertainEdit
    || (!shownPrivacy && !editReceipt);
  const savingField = editReceipt && (busy || operationRunning)
    ? (receipt!.action as "privacy" | "remark")
    : null;
  useEffect(() => {
    const dialog = ref.current!;
    dialog.showModal();
    return () => dialog.close();
  }, []);
  useEffect(() => {
    if (!contactRefreshing) setPrivacyError("");
    setPrivacyWaiting(false);
    if (!friendId || savedPrivacy || privacyDenied || contactRefreshing) {
      setPrivacyLoading(false);
      return;
    }
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
      .then((snapshot) => {
        if (!abort.signal.aborted && !snapshot) lastPrivacy.current = undefined;
      })
      .catch((e) => {
        if (!abort.signal.aborted)
          setPrivacyError(
            e instanceof NativeApiError && e.status === 403
              ? t("contactsUi.privacyReadPermission")
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
  }, [actions, savedPrivacy, friendId, privacyDenied, privacyRevision, contactRefreshing, connection, account]);
  useEffect(() => {
    if (!friendId || !(savedPrivacy?.remarkPending || remarkConfirming) || contactRefreshing
      || remarkPolls.current >= REMARK_POLL_LIMIT) return;
    const abort = new AbortController();
    const timer = setTimeout(() => {
      remarkPolls.current++;
      void actions.readPrivacy(friendId, abort.signal).catch(() => {
        if (!abort.signal.aborted) setRemarkPollFailures((n) => n + 1);
      });
    }, REMARK_POLL_MS);
    return () => {
      clearTimeout(timer);
      abort.abort();
    };
  }, [actions, friendId, savedPrivacy, remarkConfirming, remarkPollFailures, contactRefreshing]);
  // Drop edits that the saved or submitted settings now already have.
  useEffect(() => {
    setPrivacyEdits((edits) => {
      const kept = Object.fromEntries(Object.entries(edits).filter(
        ([key, value]) => privacyBase[key as keyof ContactPrivacy] !== value,
      ));
      return Object.keys(kept).length === Object.keys(edits).length ? edits : kept;
    });
  }, [privacyBase.chatsOnly, privacyBase.hideMyPosts, privacyBase.hideTheirPosts]);
  useEffect(() => {
    if (privacyReceipt?.operation?.state === "succeeded" && savedPrivacy && !privacyLoading
      && savedHere.current.delete(privacyReceipt.key))
      setSavedFlash((flash) => ({ ...flash, privacy: true }));
  }, [privacyReceipt, savedPrivacy, privacyLoading]);
  useEffect(() => {
    if (confirmingRemark && !remarkConfirming) {
      setConfirmingRemark(null);
      setSavedFlash((flash) => ({ ...flash, remark: true }));
    }
  }, [confirmingRemark, remarkConfirming]);
  useEffect(() => {
    if (!savedFlash.privacy && !savedFlash.remark) return;
    const timer = setTimeout(() => setSavedFlash({ privacy: false, remark: false }), SAVED_FLASH_MS);
    return () => clearTimeout(timer);
  }, [savedFlash]);
  function refreshPrivacy() {
    if (friendId) privacyCache.invalidate(account, friendId);
    setPrivacyRevision((n) => n + 1);
  }
  async function refreshContact() {
    if (!friendId || refreshAbort.current) return;
    const session = connection.session;
    const abort = new AbortController();
    refreshAbort.current = abort;
    const signal = AbortSignal.any([abort.signal, AbortSignal.timeout(125_000)]);
    const unsubscribe = session.subscribe(() => {
      const current = session.snapshot();
      if (current.account !== account || current.signInRequired === account) abort.abort();
    });
    setContactRefreshing(true);
    setError("");
    try {
      let operation = await session.client.refreshContact(account, friendId, signal);
      while (["queued", "running"].includes(operation.state)) {
        await pause(700, signal);
        signal.throwIfAborted();
        operation = await session.client.operation(account, operation.id, signal);
      }
      signal.throwIfAborted();
      if (operation.state !== "succeeded" || !operation.result
        || typeof operation.result !== "object" || !("recordCount" in operation.result)
        || operation.result.recordCount !== 1) {
        throw Error(t("contactsUi.contactRefreshFailed"));
      }
      refreshPrivacy();
      void session.refreshConversations(account, true).catch(() => {});
    } catch {
      if (!abort.signal.aborted) setError(t("contactsUi.contactRefreshFailed"));
    } finally {
      unsubscribe();
      if (refreshAbort.current === abort) refreshAbort.current = null;
      if (!abort.signal.aborted) setContactRefreshing(false);
    }
  }
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
      staleRetries.current = 0;
      savedHere.current.add(pending.key);
      setPrivacyLoading(true);
      refreshPrivacy();
    } else if (pending.action === "remark" && operation.state === "succeeded") {
      staleRetries.current = 0;
      const submitted = pending.input.remark;
      // Re-read quietly until WeChat confirms; keep what is still being typed.
      setConfirmingRemark({ value: submitted, over: savedPrivacy ?? null });
      remarkPolls.current = 0;
      setRemarkDraft((draft) => draft !== null && draft.trim() === (submitted ?? "")
        && document.activeElement !== remarkInput.current ? null : draft);
    } else if ((pending.action === "privacy" || pending.action === "remark")
      && (operation.state === "failed" || operation.state === "cancelled")) {
      connection.cache.writeState(account, receiptId, null);
      setReceipt(undefined);
      keepEdit(pending);
      setFieldError({ field: pending.action, message: t("contactsUi.saveFailedDetail"), retry: true });
      if (pending.action === "privacy") refreshPrivacy();
    }
  };
  /** Put a change that didn't save back into the pending edits, so it shows and can be retried. */
  function keepEdit(pending: ContactChange) {
    if (pending.action !== "privacy") return;
    const before = savedPrivacy?.privacy;
    const wanted = pending.input.privacy;
    const changedKeys = (Object.keys(wanted) as (keyof ContactPrivacy)[]).filter((key) => before?.[key] !== wanted[key]);
    setPrivacyEdits((edits) => ({
      ...Object.fromEntries(changedKeys.map((key) => [key, wanted[key]])),
      ...edits,
    }));
  }
  async function submit(pending: ContactChange) {
    if (busy) return;
    setBusy(true);
    setError("");
    setRetryOriginal(false);
    if (pending.action === "privacy" || pending.action === "remark") setFieldError(null);
    try {
      await connection.cache.flush();
      finished(pending, await actions.submit(pending));
    } catch (e) {
      const edit = pending.action === "privacy" || pending.action === "remark";
      // Friend edits report beside their field, not at the bottom of the card.
      if (!edit) setError(readableError(e));
      const permission = e instanceof NativeApiError && e.status === 403
        && ["send", "accept", "privacy", "remark"].includes(pending.action);
      if (permission) {
        setPermissionDenied(true);
        if (!edit) setError(t("contactsUi.privacyPermission"));
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
        if (pending.action === "privacy" || pending.action === "remark") {
          refreshPrivacy();
          keepEdit(pending);
          // A newer version or a queued edit: save again once the contact reloads.
          const retryLater = ["stale_contact_privacy", "contact_privacy_pending"].includes(e.code ?? "")
            && staleRetries.current++ < 2;
          if (!retryLater)
            setFieldError({
              field: pending.action,
              message: permission
                ? t(pending.action === "remark" ? "contactsUi.remarkPermission" : "contactsUi.privacyPermission")
                : readableError(e),
              retry: !permission,
            });
        }
      }
    } finally {
      setBusy(false);
    }
  }
  async function change(action: ContactChange["action"]) {
    if (
      locked ||
      (receipt && !editableReceipt) ||
      (permissionDenied && ["send", "accept", "privacy", "remark"].includes(action))
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
      privacyEdited &&
      !privacyError &&
      !privacyLoading &&
      !contactRefreshing
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
    else if (
      action === "remark" &&
      friendId &&
      savedPrivacy &&
      remarkEdited &&
      !remarkTooLong &&
      !privacyError &&
      !privacyLoading &&
      !contactRefreshing
    )
      pending = {
        key,
        action,
        contactId: friendId,
        input: {
          remark: remarkDraft!.trim() || null,
          expectedVersion: savedPrivacy.version,
        },
      };
    else return;
    save(pending);
    if (pending.action === "privacy") setPrivacyEdits({});
    if (pending.action === "remark") setRemarkFlush(false);
    if (pending.action === "privacy" || pending.action === "remark") setFieldError(null);
    await submit(pending);
  }
  async function check() {
    if (!receipt || busy) return;
    setBusy(true);
    setError("");
    setFieldError(null);
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
      if (receipt.action === "privacy" || receipt.action === "remark")
        setFieldError({ field: receipt.action, message: readableError(e), retry: false });
      else setError(readableError(e));
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
  // Autosave: privacy first, then the remark once typing pauses or the field is left.
  const autosaveReady = !!friendId && !removal && !busy && !uncertainEdit && !operationRunning
    && !!savedPrivacy && !privacyLoading && !contactRefreshing && !privacyError
    && !permissionDenied && !privacyDenied;
  const privacyQueued = privacyEdited && fieldError?.field !== "privacy";
  const remarkQueued = remarkEdited && !remarkTooLong && fieldError?.field !== "remark";
  useEffect(() => {
    if (!autosaveReady || (!privacyQueued && !remarkQueued)) return;
    const timer = setTimeout(
      () => void change(privacyQueued ? "privacy" : "remark"),
      privacyQueued ? PRIVACY_AUTOSAVE_MS : remarkFlush ? 0 : REMARK_AUTOSAVE_MS,
    );
    return () => clearTimeout(timer);
  }, [autosaveReady, privacyQueued, remarkQueued, privacyEdits, remarkDraft, remarkFlush, savedPrivacy]);
  function fieldStatus(field: "privacy" | "remark"): SaveState | null {
    if (receipt?.action === field && uncertainEdit) return "uncertain";
    if (savingField === field) return "saving";
    if (field === "privacy" && privacyReceipt?.operation?.state === "succeeded"
      && (privacyLoading || !savedPrivacy) && !privacyError)
      return "saving";
    if (fieldError?.field === field) return "failed";
    if (field === "remark" && remarkConfirming)
      return remarkPolls.current < REMARK_POLL_LIMIT ? "syncing" : "unsynced";
    // Confirmed this render: say so now rather than one render later, when the flash starts.
    if (field === "remark" && confirmingRemark) return "saved";
    if (field === "privacy" && privacyReceipt?.operation?.state === "succeeded" && savedPrivacy
      && !privacyLoading && savedHere.current.has(privacyReceipt.key))
      return "saved";
    return savedFlash[field] ? "saved" : null;
  }
  function fieldDetail(field: "privacy" | "remark") {
    if (fieldError?.field === field) return fieldError.message;
    if (receipt?.action === field && uncertainEdit) return t("contactsUi.saveUncertainDetail");
    return null;
  }
  function statusFor(field: "privacy" | "remark") {
    return (
      <SaveStatus
        state={fieldStatus(field)}
        retry={fieldError?.field === field && fieldError.retry ? () => setFieldError(null) : undefined}
        check={() => void check()}
        retryOriginal={retryOriginal && receipt ? () => void submit(receipt) : undefined}
        busy={busy}
      />
    );
  }
  /** Closing the card saves a remark still waiting for its typing pause. */
  function closeCard() {
    if (autosaveReady && remarkQueued && !privacyQueued) void change("remark");
    close();
  }
  const statusText =
    !receipt?.operation || receipt.operation.state === "needs_review"
      ? t("contactsUi.checkBeforeRetry")
      : receipt.operation.state === "succeeded"
        ? receipt.action === "privacy" || receipt.action === "remark"
          ? ""
          : t(
              (
                {
                  send: "contactsUi.sent",
                  accept: "contactsUi.accepted",
                  delete: "contactsUi.historyDeleted",
                  remove: "contactsUi.contactRemoved",
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
  const showPrivacy = !removal && (friendId
    ? !!(shownPrivacy || privacyReceipt || privacyEdited || privacyLoading || privacyError)
    : !!(discovery?.recipientRef || request?.actionable));
  return (
    <dialog
      ref={ref}
      className="contact-request-dialog contact-detail-dialog"
      aria-label={title}
      onCancel={(e) => {
        e.preventDefault();
        closeCard();
      }}
      onClose={close}
    >
      <div className="contact-request-heading">
        <h2>{title}</h2>
        {!removal && (request || friendId) && <details className="contact-more">
          <summary aria-label={t("contactsUi.moreActions")}><Icon name="more" /></summary>
          <div className="contact-more-menu">
            {request && !receipt && <button disabled={busy} onClick={() => { if (window.confirm(t("contactsUi.deleteExplanation"))) void change("delete"); }}>{t("contactsUi.deleteLocal")}</button>}
            {friendId && !privacyDenied && <button disabled={locked || contactRefreshing} onClick={() => void refreshContact()}>
              {t(contactRefreshing ? "contactsUi.refreshingContact" : "contactsUi.refreshPrivacy")}
            </button>}
            {friendId && remove && !privacyDenied && !permissionDenied && <button disabled={locked || contactRefreshing} onClick={() => remove({ id: friendId, displayName: name })}>{t("contactsUi.removeContact")}</button>}
          </div>
        </details>}
        <button onClick={closeCard} aria-label={t("contactsUi.close")}>
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
          {shownRemark && savedContact?.nickname && savedContact.nickname !== shownRemark && (
            <p>{t("contactsUi.nicknameValue", { name: savedContact.nickname })}</p>
          )}
          {savedContact?.alias && <p>{t("contactsUi.wechatIdValue", { id: savedContact.alias })}</p>}
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
      {friendId && !removal && !privacyDenied && (shownPrivacy || editReceipt || remarkDraft !== null) && (
        <div className="contact-remark">
          <div className="contact-field-heading">
            <label htmlFor={remarkFieldId}>{t("contactsUi.remark")}</label>
            <ComposerHelp label={t("contactsUi.aboutRemarks")} icon="info" className="contact-hint">
              {t("contactsUi.remarkHint")}
            </ComposerHelp>
            {statusFor("remark")}
          </div>
          <input
            ref={remarkInput}
            id={remarkFieldId}
            value={remarkValue}
            placeholder={t("contactsUi.remarkPlaceholder")}
            disabled={editingLocked}
            aria-invalid={remarkTooLong || fieldError?.field === "remark" || undefined}
            onChange={(e) => {
              setRemarkDraft(e.target.value);
              setRemarkFlush(false);
              if (fieldError?.field === "remark" && fieldError.retry) setFieldError(null);
            }}
            onBlur={() => {
              if (remarkEdited) setRemarkFlush(true);
              else if (remarkDraft !== null && !remarkSubmitted) setRemarkDraft(null);
            }}
          />
          {(remarkTooLong || fieldDetail("remark")) && (
            <small className="contact-field-problem" role="alert">
              {remarkTooLong ? t("contactsUi.remarkTooLong") : fieldDetail("remark")}
            </small>
          )}
        </div>
      )}
      {showPrivacy && (
        <section className="contact-detail-privacy">
          {privacyLoading && !shownPrivacy && !privacyReceipt && (
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
          {(!friendId || (!privacyDenied && (shownPrivacy || privacyReceipt || privacyEdited))) && (
            <PrivacyFields
              hint={t(friendId ? "contactsUi.privacyForContact" : "contactsUi.privacyBeforeRequest")}
              status={friendId ? statusFor("privacy") : null}
              value={privacy}
              disabled={friendId ? editingLocked : locked || permissionDenied}
              change={(value) => {
                if (!friendId) {
                  setInitialPrivacy(value);
                  return;
                }
                const keys = Object.keys(value) as (keyof ContactPrivacy)[];
                setPrivacyEdits(Object.fromEntries(
                  keys.filter((key) => value[key] !== privacyBase[key]).map((key) => [key, value[key]]),
                ));
                if (fieldError?.field === "privacy" && fieldError.retry) setFieldError(null);
              }}
            />
          )}
          {friendId && fieldDetail("privacy") && (
            <small className="contact-field-problem" role="alert">{fieldDetail("privacy")}</small>
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
      {receipt && !editReceipt && (
        <p role="status" className="contact-change-status">
          {statusText}
        </p>
      )}
      {error && <p role="alert">{error}</p>}
      <div className="contact-request-actions">
        {(friendId || contact?.id) && open && (
          <button
            onClick={() => {
              closeCard();
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
        {receipt && !editReceipt && retryOriginal && (
          <button disabled={busy} onClick={() => void submit(receipt)}>
            {t("contactsUi.retryOriginal")}
          </button>
        )}
        {receipt && !editReceipt && (
          <button disabled={busy} onClick={() => void check()}>
            {t("contactsUi.checkStatus")}
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
