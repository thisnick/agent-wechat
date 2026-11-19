import { useEffect, useRef, useState, useSyncExternalStore } from "react";
import { isCancelledError, useMutation, useQuery } from "@tanstack/react-query";
import { NativeApiError, type NativeOperation } from "@agent-wechat/shared";
import type { Connection } from "./connection.ts";
import { t } from "./i18n.ts";
import { pause, readableError } from "./session.ts";
import {
  contactChangeActions,
  defaultContactPrivacy,
  readContactPrivacyWithPendingRetry,
  sameContactPrivacy,
  MAX_CONTACT_REMARK_BYTES,
  PRIVACY_AUTOSAVE_MS,
  REMARK_AUTOSAVE_MS,
  REMARK_POLL_LIMIT,
  REMARK_POLL_MS,
  SAVED_FLASH_MS,
  type ContactChange,
  type ContactDetailActions,
  type ContactPrivacy,
  type ContactPrivacySnapshot,
} from "./contact-privacy.ts";
import { contactKey, type ContactSnapshot } from "./contact-queries.ts";

export type EditField = "privacy" | "remark";
export type SaveState = "saving" | "syncing" | "unsynced" | "saved" | "failed" | "uncertain";
type EditChange = Extract<ContactChange, { action: EditField }>;

/** The submission's outcome is unknown: only Check (or Retry with the same key) may continue it. */
class EditUncertain extends Error {
  readonly change: EditChange;
  constructor(change: EditChange) {
    super("contact_edit_uncertain");
    this.change = change;
  }
}
/** Another submission's outcome is still unknown; this edit waits and is not sent. */
export class EditBlocked extends Error {
  constructor() {
    super("contact_edit_blocked");
  }
}
/** WeChat answered and the change was not applied. */
class EditFailed extends Error {
  readonly operation: NativeOperation;
  constructor(operation: NativeOperation) {
    super("contact_edit_failed");
    this.operation = operation;
  }
}

const isEdit = (change: ContactChange | null | undefined): change is EditChange =>
  change?.action === "privacy" || change?.action === "remark";
/** A stored submission that hasn't reached a final outcome. */
export const unsettled = (change: ContactChange | null | undefined): change is EditChange =>
  isEdit(change) && !["succeeded", "failed", "cancelled"].includes(change.operation?.state ?? "");
/**
 * The one receipt slot a contact's friend edits share. A submission writes, updates
 * or clears it only while the slot is free, settled, or already holds its own key, so
 * an edit queued behind one whose outcome is unknown can't erase that receipt.
 */
export function editReceipts(
  cache: {
    readState<T>(account: string, id: string): T | undefined;
    writeState(account: string, id: string, value: unknown): void;
  },
  account: string,
  contactId: string,
) {
  const id = `contact-change:privacy:${contactId}`;
  return {
    /** A submission of unknown outcome left by an earlier card, if any. */
    restore(): EditChange | null {
      const stored = cache.readState<ContactChange>(account, id);
      return unsettled(stored) ? stored : null;
    },
    write(change: EditChange, value: EditChange | null) {
      const stored = cache.readState<ContactChange>(account, id);
      if (unsettled(stored) && stored.key !== change.key) throw new EditBlocked();
      cache.writeState(account, id, value);
    },
  };
}
/** The card closing or the account changing ended a read; neither is a failed save. */
const interrupted = (error: unknown) =>
  isCancelledError(error) || (error instanceof DOMException && error.name === "AbortError");
const rejected = (error: unknown) =>
  error instanceof NativeApiError && error.status >= 400 && error.status < 500
  && error.code !== "idempotency_conflict" && ![408, 429].includes(error.status);
/** A newer version or a queued edit: worth one fresh read and another attempt. */
const retryLater = (error: unknown) =>
  error instanceof NativeApiError && ["stale_contact_privacy", "contact_privacy_pending"].includes(error.code ?? "");
/** Following an accepted operation outlives the card, like the receipt it updates. */
const NEVER = new AbortController().signal;

/**
 * A friend's remark and relationship privacy on the contact card. Edits save on
 * their own, one change per request: privacy shortly after a change, the remark
 * once typing pauses or the field is left. Both saves share one mutation scope, so
 * they run one after another and each reads the contact's current version when it
 * starts. Receipts keep the idempotency key of every submission, so a lost response
 * is checked, never resent with a new key.
 */
export function useContactEditor({
  connection,
  account,
  friendId,
  actions,
  paused,
  fallbackRemark,
  changed,
}: {
  connection: Connection;
  account: string;
  friendId: string | null;
  actions: ContactDetailActions;
  /** True while a contact refresh owns the contact; reads and saves wait. */
  paused: boolean;
  /** The catalog's remark, shown until the contact has been read. */
  fallbackRemark: string | null | undefined;
  changed: () => void;
}) {
  const queries = connection.session.contactQueries;
  const queryClient = queries.client;
  const client = connection.session.client;
  const id = friendId ?? "";
  const key = contactKey(account, id);
  const receipts = editReceipts(connection.cache, account, id);
  const denied = useSyncExternalStore(queries.subscribe, () => queries.denied(account));
  const [waiting, setWaiting] = useState(false);
  const read = (signal: AbortSignal) =>
    readContactPrivacyWithPendingRetry<ContactSnapshot>((s) => queries.read(account, id, s), signal, {
      pending: () => setWaiting(true),
    }).finally(() => setWaiting(false));

  // Remark confirmation: WeChat's own lookup saves the new remark a moment after
  // the write, and no event announces it, so the card re-reads (up to a limit) until
  // a read taken after the save no longer reports it pending. Reads are counted, not
  // timed, so a paused or frozen clock can't confirm or expire it.
  const [confirm, setConfirm] = useState<{ value: string | null; after: number } | null>(null);
  const pollBase = useRef<number | null>(null);
  const reads = () => {
    const state = queryClient.getQueryState(key);
    return { data: state?.dataUpdateCount ?? 0, all: (state?.dataUpdateCount ?? 0) + (state?.errorUpdateCount ?? 0) };
  };
  const confirmingAt = (data: ContactSnapshot | undefined, dataReads: number) =>
    !!confirm && (!data || dataReads <= confirm.after || data.remarkPending);
  const query = useQuery({
    queryKey: key,
    queryFn: ({ signal }) => read(signal),
    enabled: !!friendId && !denied && !paused,
    refetchInterval: (current) => {
      const data = current.state.data;
      const done = current.state.dataUpdateCount + current.state.errorUpdateCount;
      if (!data?.remarkPending && !confirmingAt(data, current.state.dataUpdateCount)) {
        pollBase.current = null;
        return false;
      }
      pollBase.current ??= done;
      return done - pollBase.current < REMARK_POLL_LIMIT ? REMARK_POLL_MS : false;
    },
  }, queryClient);
  const saved: ContactPrivacySnapshot | undefined = query.data ?? undefined;
  const readError = denied
    ? t("contactsUi.privacyReadPermission")
    : query.error && !(query.error instanceof DOMException && query.error.name === "AbortError")
      ? t(query.error instanceof NativeApiError && query.error.status === 403
        ? "contactsUi.privacyReadPermission"
        : "contactsUi.privacyReadFailed")
      : "";
  const remarkConfirming = confirmingAt(saved, reads().data);
  const pollExpired = remarkConfirming && pollBase.current !== null
    && reads().all - pollBase.current >= REMARK_POLL_LIMIT;
  // Once confirmed, forget the submitted value so a later edit elsewhere shows as itself.
  useEffect(() => {
    if (confirm && !remarkConfirming) setConfirm(null);
  }, [confirm, remarkConfirming]);

  // A submission whose outcome is unknown, kept across reopening the card.
  const [uncertain, setUncertain] = useState<EditChange | null>(() => {
    return friendId ? receipts.restore() : null;
  });
  const [checking, setChecking] = useState(false);
  const [retryOriginal, setRetryOriginal] = useState(false);
  const [permissionDenied, setPermissionDenied] = useState(false);
  // A field's failure shows beside its heading and pauses its autosave until the
  // field is edited again or retried.
  const [fieldError, setFieldError] = useState<{ field: EditField; message: string; retry: boolean } | null>(null);
  const [flashDone, setFlashDone] = useState<Record<EditField, boolean>>({ privacy: true, remark: true });
  // A field whose uncertain submission turned out to be saved (Check or Retry).
  const [recovered, setRecovered] = useState<EditField | null>(null);

  /** Submit once with the change's key, then follow its operation to an outcome. */
  const writeReceipt = receipts.write;
  async function submitEdit(change: EditChange): Promise<NativeOperation> {
    // One receipt slot per contact: an edit queued behind an unsettled one waits.
    writeReceipt(change, change);
    await connection.cache.flush();
    let operation: NativeOperation;
    try {
      operation = await actions.submit(change);
    } catch (error) {
      if (!rejected(error)) throw new EditUncertain(change);
      writeReceipt(change, null);
      throw error;
    }
    return settle(change, operation);
  }
  async function settle(change: EditChange, first: NativeOperation): Promise<NativeOperation> {
    let operation = first;
    try {
      while (["queued", "running"].includes(operation.state)) {
        writeReceipt(change, { ...change, operation });
        await pause(800, NEVER);
        operation = await client.operation(account, operation.id);
      }
    } catch {
      throw new EditUncertain({ ...change, operation });
    }
    if (operation.state === "needs_review") {
      writeReceipt(change, { ...change, operation });
      throw new EditUncertain({ ...change, operation });
    }
    writeReceipt(change, null);
    changed();
    if (operation.state !== "succeeded") throw new EditFailed(operation);
    return operation;
  }
  /** The contact's current settings; re-read when the cached copy may be behind. */
  async function current(): Promise<ContactPrivacySnapshot> {
    const state = queryClient.getQueryState<ContactSnapshot>(key);
    const snapshot = !state?.data || state.isInvalidated || state.fetchStatus === "fetching"
      ? await readFresh()
      : state.data;
    if (!snapshot) throw new NativeApiError(409, "contact_privacy_unresolved");
    return snapshot;
  }
  /** Run one edit against the current version; after a version conflict, read again and retry (twice at most). */
  async function attempt(build: (snapshot: ContactPrivacySnapshot) => EditChange | null) {
    for (let tries = 0; ; tries++) {
      const change = build(await current());
      if (!change) return null;
      try {
        return await submitEdit(change);
      } catch (error) {
        if (tries >= 2 || !retryLater(error)) throw error;
        refresh();
      }
    }
  }
  /**
   * Read the contact now for a save. A save outlives the card, so its read isn't tied
   * to the card's query signal; if closing the card cancels a read it joined, read again.
   */
  async function readFresh(): Promise<ContactSnapshot> {
    for (let tries = 0; ; tries++) {
      try {
        return await queryClient.fetchQuery({ queryKey: key, queryFn: () => read(NEVER), staleTime: 0 });
      } catch (error) {
        if (tries >= 2 || !isCancelledError(error)) throw error;
      }
    }
  }
  /** After an accepted privacy change, read back the settings WeChat saved. */
  const readBack = readFresh;

  /** Mark the contact changed: refetch it, and keep older catalog pages from overwriting it. */
  function refresh() {
    if (friendId) queries.invalidate(account, friendId);
  }
  function failed(field: EditField, error: unknown) {
    if (error instanceof EditUncertain) {
      setUncertain(error.change);
      return;
    }
    // Waiting behind an unresolved save, or interrupted after WeChat accepted it: not a failure.
    if (error instanceof EditBlocked || interrupted(error)) return;
    const permission = error instanceof NativeApiError && error.status === 403;
    if (permission) setPermissionDenied(true);
    setFieldError({
      field,
      message: permission
        ? t(field === "remark" ? "contactsUi.remarkPermission" : "contactsUi.privacyPermission")
        : error instanceof EditFailed ? t("contactsUi.saveFailedDetail") : readableError(error),
      retry: !permission,
    });
    refresh();
  }

  const scope = { id: `contact-edit:${account}:${id}` };
  const privacyMutation = useMutation({
    scope,
    mutationFn: async (edits: Partial<ContactPrivacy>) => {
      const operation = await attempt((snapshot) => {
        const privacy = { ...snapshot.privacy, ...edits };
        return sameContactPrivacy(privacy, snapshot.privacy) ? null : {
          key: crypto.randomUUID(),
          action: "privacy",
          contactId: id,
          input: { privacy, expectedVersion: snapshot.version },
        };
      });
      // WeChat accepted the change; a failed read-back is a read error, shown with
      // the card's other read errors, not a failed save to retry.
      if (operation) await readBack().catch(() => undefined);
    },
    onMutate: () => setFlashDone((done) => ({ ...done, privacy: false })),
    onError: (error, edits) => {
      failed("privacy", error);
      // Keep showing the change so it can be retried.
      if (!(error instanceof EditUncertain)) setPrivacyEdits((local) => ({ ...edits, ...local }));
    },
  }, queryClient);
  const remarkMutation = useMutation({
    scope,
    mutationFn: async (remark: string | null) => {
      const operation = await attempt((snapshot) =>
        (snapshot.remark ?? null) === remark && !snapshot.remarkPending ? null : {
          key: crypto.randomUUID(),
          action: "remark",
          contactId: id,
          input: { remark, expectedVersion: snapshot.version },
        });
      if (!operation) return;
      pollBase.current = null;
      setConfirm({ value: remark, after: reads().data });
      refresh();
    },
    onMutate: () => setFlashDone((done) => ({ ...done, remark: false })),
    onSuccess: (_, remark) => {
      // Keep what is still being typed; drop a draft that only matches the saved remark.
      setRemarkDraft((draft) => draft !== null && (draft.trim() || null) === remark
        && document.activeElement !== remarkInput.current ? null : draft);
    },
    onError: (error) => failed("remark", error),
  }, queryClient);

  // Check a submission whose outcome is unknown, or resend it with its original key.
  async function recover(resend: boolean) {
    const change = uncertain;
    if (!change || checking) return;
    setChecking(true);
    setFieldError(null);
    setRetryOriginal(false);
    setRecovered(null);
    try {
      let operation: NativeOperation;
      if (resend) {
        operation = await submitEdit(change);
      } else {
        operation = change.operation
          ? await client.operation(account, change.operation.id)
          : await client.contactMutationByKey(account, change.key);
        if (operation.accountId !== account || operation.kind !== "contact_mutation"
          || (operation.result as { action?: unknown } | null)?.action !== contactChangeActions[change.action]
          || (change.operation && operation.id !== change.operation.id))
          throw Error(t("contactsUi.operationMismatch"));
        operation = await settle(change, operation);
      }
      // Settled: a failed read-back is a read error, not an unresolved save.
      if (change.action === "privacy") await readBack().catch(() => undefined);
      else {
        pollBase.current = null;
        setConfirm({ value: change.input.remark, after: reads().data });
      }
      setUncertain(null);
      setFlashDone((done) => ({ ...done, [change.action]: false }));
      setRecovered(change.action);
      if (change.action === "remark") refresh();
    } catch (error) {
      if (error instanceof EditUncertain) setUncertain(error.change);
      else if (error instanceof EditFailed || rejected(error) && resend) {
        setUncertain(null);
        failed(change.action, error);
      } else {
        if (!change.operation && !resend && error instanceof NativeApiError && error.status === 404)
          setRetryOriginal(true);
        setFieldError({ field: change.action, message: readableError(error), retry: false });
      }
    } finally {
      setChecking(false);
    }
  }
  // A submission restored mid-flight keeps being followed.
  useEffect(() => {
    if (!uncertain?.operation || !["queued", "running"].includes(uncertain.operation.state) || checking) return;
    const timer = setTimeout(() => void recover(false), 800);
    return () => clearTimeout(timer);
  }, [uncertain, checking]);

  // What the card shows: saved settings, then a submission in flight or of unknown
  // outcome, then the keys changed here and not yet submitted.
  const [privacyEdits, setPrivacyEdits] = useState<Partial<ContactPrivacy>>({});
  const savingPrivacy = privacyMutation.isPending ? privacyMutation.variables : undefined;
  const privacyBase: ContactPrivacy = {
    ...(saved?.privacy ?? defaultContactPrivacy()),
    ...(uncertain?.action === "privacy" ? uncertain.input.privacy : {}),
    ...savingPrivacy,
  };
  const privacy = { ...privacyBase, ...privacyEdits };
  const privacyEdited = !sameContactPrivacy(privacy, privacyBase);
  // Drop changes the saved or submitted settings already have.
  useEffect(() => {
    setPrivacyEdits((edits) => {
      const kept = Object.fromEntries(Object.entries(edits).filter(
        ([field, value]) => privacyBase[field as keyof ContactPrivacy] !== value,
      ));
      return Object.keys(kept).length === Object.keys(edits).length ? edits : kept;
    });
  }, [privacyBase.chatsOnly, privacyBase.hideMyPosts, privacyBase.hideTheirPosts]);

  const [remarkDraft, setRemarkDraft] = useState<string | null>(null);
  // Leaving the field saves at once instead of waiting for the typing pause.
  const [remarkFlush, setRemarkFlush] = useState(false);
  const remarkInput = useRef<HTMLInputElement>(null);
  const savingRemark = remarkMutation.isPending ? remarkMutation.variables : undefined;
  const remarkBase = (uncertain?.action === "remark" ? uncertain.input.remark ?? "" : undefined)
    ?? (savingRemark !== undefined ? savingRemark ?? "" : undefined)
    ?? (remarkConfirming ? confirm?.value ?? "" : undefined)
    ?? (saved ? saved.remark : fallbackRemark)
    ?? "";
  const remarkValue = remarkDraft ?? remarkBase;
  const remarkEdited = remarkDraft !== null && remarkDraft.trim() !== remarkBase;
  const remarkTooLong = new TextEncoder().encode(remarkValue.trim()).length > MAX_CONTACT_REMARK_BYTES;

  // Autosave once the contact is current and nothing is unresolved: privacy first
  // (a burst of changes becomes one save), then the remark after a typing pause.
  const ready = !!friendId && !!saved && !query.isFetching && !uncertain && !checking
    && !permissionDenied && !readError && !paused;
  const privacyQueued = privacyEdited && !privacyMutation.isPending && fieldError?.field !== "privacy";
  const remarkQueued = remarkEdited && !remarkTooLong && !remarkMutation.isPending && fieldError?.field !== "remark";
  useEffect(() => {
    if (!ready || (!privacyQueued && !remarkQueued)) return;
    const timer = setTimeout(() => {
      setFieldError(null);
      if (privacyQueued) {
        setPrivacyEdits({});
        privacyMutation.mutate(privacyEdits);
      } else {
        setRemarkFlush(false);
        remarkMutation.mutate(remarkDraft!.trim() || null);
      }
    }, privacyQueued ? PRIVACY_AUTOSAVE_MS : remarkFlush ? 0 : REMARK_AUTOSAVE_MS);
    return () => clearTimeout(timer);
  }, [ready, privacyQueued, remarkQueued, privacyEdits, remarkDraft, remarkFlush, saved]);

  function status(field: EditField): SaveState | null {
    if (uncertain?.action === field) return "uncertain";
    const mutation = field === "privacy" ? privacyMutation : remarkMutation;
    if (mutation.isPending) return "saving";
    if (fieldError?.field === field) return "failed";
    if (field === "remark" && remarkConfirming) return pollExpired ? "unsynced" : "syncing";
    return (mutation.isSuccess || recovered === field) && !flashDone[field] ? "saved" : null;
  }
  const remarkStatus = status("remark");
  const privacyStatus = status("privacy");
  // "Saved" shows for a moment after the change is confirmed.
  useEffect(() => {
    const fields = (["privacy", "remark"] as const).filter((field) => status(field) === "saved");
    if (!fields.length) return;
    const timer = setTimeout(
      () => setFlashDone((done) => ({ ...done, ...Object.fromEntries(fields.map((field) => [field, true])) })),
      SAVED_FLASH_MS,
    );
    return () => clearTimeout(timer);
  }, [remarkStatus, privacyStatus]);

  return {
    denied,
    snapshot: saved,
    /** The first read is still running (or waiting on contact sync). */
    loading: !!friendId && !denied && query.isPending && query.fetchStatus === "fetching",
    waiting,
    readError,
    permissionDenied,
    /** Edits are closed while the outcome of a submission is unknown or the contact can't be read. */
    locked: permissionDenied || paused || !!readError || !!uncertain || checking || (!saved && !uncertain),
    privacy,
    privacyEdited,
    setPrivacy(value: ContactPrivacy) {
      const keys = Object.keys(value) as (keyof ContactPrivacy)[];
      setPrivacyEdits(Object.fromEntries(
        keys.filter((field) => value[field] !== privacyBase[field]).map((field) => [field, value[field]]),
      ));
      if (fieldError?.field === "privacy" && fieldError.retry) setFieldError(null);
    },
    remarkInput,
    remarkValue,
    remarkTooLong,
    remarkConfirming,
    /** An accepted remark the catalog may not show yet. */
    acceptedRemark: remarkConfirming ? confirm?.value ?? null : undefined,
    setRemark(value: string) {
      setRemarkDraft(value);
      setRemarkFlush(false);
      if (fieldError?.field === "remark" && fieldError.retry) setFieldError(null);
    },
    leaveRemark() {
      if (remarkEdited) setRemarkFlush(true);
      else if (remarkDraft !== null && savingRemark === undefined) setRemarkDraft(null);
    },
    /** Save a remark still waiting for its typing pause, as the card closes. */
    flush() {
      if (ready && remarkQueued && !privacyQueued) remarkMutation.mutate(remarkDraft!.trim() || null);
    },
    status,
    detail(field: EditField) {
      if (fieldError?.field === field) return fieldError.message;
      if (uncertain?.action === field) return t("contactsUi.saveUncertainDetail");
      return null;
    },
    retry(field: EditField) {
      return fieldError?.field === field && fieldError.retry ? () => setFieldError(null) : undefined;
    },
    check: () => void recover(false),
    retryOriginal: retryOriginal && uncertain ? () => void recover(true) : undefined,
    checking,
    /** Read the contact again, e.g. after a refresh from WeChat. */
    refresh,
  };
}
