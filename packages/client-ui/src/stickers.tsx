import { useEffect, useId, useRef, useState, type CSSProperties, type DragEvent } from "react";
import { createPortal } from "react-dom";
import type { CustomSticker, CustomStickerList, MediaAsset, NativeWeChatClient, PreparedSticker, StickerPack, StickerPackSticker, StickerSendSource } from "@agent-wechat/shared";
import { NativeApiError } from "@agent-wechat/shared";
import type { Connection } from "./connection.ts";
import { readableError } from "./session.ts";
import { t } from "./i18n.ts";
import { Button, Icon } from "./ui.tsx";
import { StickerJobError, waitStickerJob } from "./sticker-jobs.ts";
import { FPS_STOPS, MAX_STICKER_EDGE, PHONE_STICKER_EDGE, defaultOptions, exceedsStickerEdge, maxLength, readStickerSource, requestKey, sizeStops, stickerRequest, stopPosition, trimRange, withLength, withStart, type StickerOptions, type StickerRequestOptions, type StickerSource } from "./sticker-options.ts";
import "./stickers.css";
export { waitStickerJob } from "./sticker-jobs.ts";

export type StickerDraft = { source: StickerSendSource; asset: MediaAsset; label: string; key: string; jobId?: string; busy?: boolean; error?: string };

let previewTransfers = 0;
const previewQueue: Array<() => void> = [];
async function previewTransfer<T>(action: () => Promise<T>, signal: AbortSignal): Promise<T> {
  if (previewTransfers >= 4) await new Promise<void>(resolve => previewQueue.push(resolve));
  else previewTransfers++;
  try { signal.throwIfAborted(); return await action(); }
  finally { const next=previewQueue.shift(); if(next) next(); else previewTransfers--; }
}

/** Authenticated bytes become a local image URL, never a token-bearing URL. */
export function StickerImage({ client, account, asset, label, acquire = false }: { client: NativeWeChatClient; account: string; asset?: MediaAsset | null; label: string; acquire?: boolean }) {
  const [url, setUrl] = useState("");
  const element = useRef<HTMLElement | null>(null);
  const [visible, setVisible] = useState(false);
  useEffect(() => {
    const observer = new IntersectionObserver(entries => {
      if (entries.some(entry => entry.isIntersecting)) { setVisible(true); observer.disconnect(); }
    }, { rootMargin: "80px" });
    if (element.current) observer.observe(element.current);
    return () => observer.disconnect();
  }, []);
  useEffect(() => {
    if (!visible) return;
    const abort = new AbortController(); let owned = "";
    setUrl("");
    void (async () => {
      let current=asset;
      if (current && current.state!=="available" && acquire && current.id.startsWith("sticker_preview_")) {
        const id=current.id;
        current=await previewTransfer(async()=> {
          await waitStickerJob(client,account,await client.downloadSticker(account,{assetId:id},crypto.randomUUID(),abort.signal),abort.signal);
          abort.signal.throwIfAborted();return client.mediaAsset(account,id);
        },abort.signal);
      }
      if (current?.state!=="available") return;
      abort.signal.throwIfAborted();
      const preview = current.contentType === "image/x-wxgf" || matchMedia("(prefers-reduced-motion: reduce)").matches;
      const content=await client.mediaContent(account, current.id, { maxBytes: 16 * 1024 * 1024, signal: abort.signal, representation: preview && current.preview ? "preview" : "original" });
      abort.signal.throwIfAborted();
      owned = URL.createObjectURL(new Blob([new Uint8Array(content.bytes)], { type: content.contentType })); setUrl(owned);
    })().catch(()=>{});
    return () => { abort.abort(); if (owned) URL.revokeObjectURL(owned); };
  }, [client, account, asset?.id, asset?.sha256, asset?.state, acquire, visible]);
  return url ? <img ref={node => { element.current = node; }} src={url} alt={label} draggable={false} /> : <span ref={node => { element.current = node; }} className="sticker-unloaded" aria-label={label}><Icon name="smile" /></span>;
}

/** Only visible tray tiles read metadata or acquire missing media. */
function StickerTileImage({ client, account, item, pack }: { client: NativeWeChatClient; account: string; item: CustomSticker | StickerPackSticker; pack?: string }) {
  const root = useRef<HTMLSpanElement>(null);
  const [visible, setVisible] = useState(false);
  const [asset, setAsset] = useState<MediaAsset | null | undefined>(item.original);
  const [error, setError] = useState("");
  useEffect(() => {
    const observer = new IntersectionObserver(entries => {
      if (entries.some(entry => entry.isIntersecting)) { setVisible(true); observer.disconnect(); }
    }, { root: root.current?.closest(".sticker-grid"), rootMargin: "80px" });
    if (root.current) observer.observe(root.current);
    return () => observer.disconnect();
  }, []);
  useEffect(() => {
    if (!visible) return;
    const abort = new AbortController();
    void previewTransfer(async () => {
      let current: CustomSticker | StickerPackSticker = pack ? item : await client.customSticker(account, item.id, abort.signal);
      let media = current.original;
      // Pack thumbnails are much smaller than the full animation archive.
      if (media?.state !== "available" && pack && (current as StickerPackSticker).thumbnail) {
        media = (current as StickerPackSticker).thumbnail;
      } else if (media?.state !== "available") {
        await waitStickerJob(client, account, await client.downloadSticker(account,
          { stickerId: item.id, ...(pack ? { packId: pack } : {}) }, crypto.randomUUID(), abort.signal), abort.signal);
        current = pack ? await client.stickerPackItem(account, pack, item.id, "en", abort.signal)
          : await client.customSticker(account, item.id, abort.signal);
        media = current.original;
      }
      abort.signal.throwIfAborted();
      setAsset(media);
    }, abort.signal).catch(error => { if (!abort.signal.aborted) setError(readableError(error)); });
    return () => abort.abort();
  }, [client, account, item.id, pack, visible]);
  return <span ref={root} className="sticker-tile-image" title={error || undefined}>
    <StickerImage client={client} account={account} asset={visible ? asset : null} label={item.caption || t("stickers.sticker")} acquire={visible} />
  </span>;
}

export function StickerDraftPreview({ connection, account, draft, remove }: { connection: Connection; account: string; draft: StickerDraft; remove: () => void }) {
  return <section className="sticker-draft" aria-label={t("stickers.draft")}>
    <StickerImage client={connection.session.client} account={account} asset={draft.asset} label={draft.label} />
    <span>{draft.busy ? t("stickers.sending") : t("stickers.readyToSend")}{draft.error && <small role="alert">{draft.error}</small>}</span>
    <Button icon="close" label={t("stickers.discardDraft")} disabled={draft.busy} onClick={remove} />
  </section>;
}

/** One preparation per distinct option set, each with its own idempotency keys. */
type PreparedEntry = { key: string; addKey: string; prepared?: PreparedSticker; error?: string; retry?: boolean };
/**
 * `options` follow the sliders; `want` follows them after a pause and is what gets
 * prepared. `shown` is the last result that matched the options, kept on screen while
 * the next one is prepared.
 */
type ImportItem = { id: string; file: File; selected: boolean; sourceId?: string; source?: StickerSource; options?: StickerOptions; want?: string; results: Record<string, PreparedEntry>; shown?: PreparedSticker; error?: string; added?: boolean };
const liveKey = (item: ImportItem) => item.source && item.options ? requestKey(stickerRequest(item.source, item.options)) : undefined;
function liveEntry(item: ImportItem): PreparedEntry | undefined { const key = liveKey(item); return key ? item.results[key] : undefined; }
function isPreparing(item: ImportItem) { const entry = liveEntry(item); return item.selected && !item.added && !entry?.prepared && !entry?.error; }
type DragPreview = { id: string; x: number; y: number; width: number; height: number; src?: string; dropId?: string; side?: "before" | "after" };
type Props = { connection: Connection; account: string; enabled: boolean; stage: (draft: StickerDraft) => void };
export function StickerPicker({ connection, account, enabled, stage }: Props) {
  const client = connection.session.client;
  const [open, setOpen] = useState(false), [edit, setEdit] = useState(false), [tab, setTab] = useState("saved");
  const [saved, setSaved] = useState<CustomStickerList>({ items: [], revision: "", freshness: "profile" });
  const [added, setAdded] = useState<StickerPack[]>([]);
  const [savedLoaded, setSavedLoaded] = useState(false);
  const [packItems, setPackItems] = useState<{ id: string; items: StickerPackSticker[]; loading: boolean }>({ id: "", items: [], loading: false });
  const [browser, setBrowser] = useState<"browse" | "added" | null>(null), [catalog, setCatalog] = useState<StickerPack[]>([]), [cursor, setCursor] = useState<string | null>(null);
  const [detail, setDetail] = useState<StickerPack | null>(null), [detailItems, setDetailItems] = useState<StickerPackSticker[]>([]);
  const [detailPreview,setDetailPreview]=useState<StickerPackSticker|null>(null);
  const [imports, setImports] = useState<ImportItem[] | null>(null), [busy, setBusy] = useState(false), [error, setError] = useState("");
  const [savedLimit,setSavedLimit]=useState(25);
  const root = useRef<HTMLDivElement>(null), fileInput = useRef<HTMLInputElement>(null), hold = useRef<ReturnType<typeof setTimeout> | undefined>(undefined), held = useRef(false);
  const scope = useRef(new AbortController()), keys = useRef(new Map<string, string>()), id = useId();
  const preparing = useRef(false), [queue, setQueue] = useState(0);
  const adjustTimers = useRef(new Map<string, ReturnType<typeof setTimeout>>()), importScope = useRef(new AbortController());
  const catalogLoaded = useRef(0);
  const packViews = useRef(new Map<string, { at: number; detail: StickerPack; items: StickerPackSticker[] }>());
  const viewEpoch = useRef(0);
  const clearPackViews = () => { viewEpoch.current++; packViews.current.clear(); catalogLoaded.current = 0; };
  const pointerDrag=useRef<{id:string;x:number;y:number;moved:boolean;pointer:number;target:HTMLButtonElement;offsetX:number;offsetY:number;width:number;height:number}|null>(null);
  const [dragPreview, setDragPreview] = useState<DragPreview | null>(null);
  function clearDrag() {
    clearTimeout(hold.current);
    const drag = pointerDrag.current; pointerDrag.current = null;
    if (drag?.target.hasPointerCapture(drag.pointer)) drag.target.releasePointerCapture(drag.pointer);
    setDragPreview(null);
  }
  function dragPlacement(x: number, y: number, moving: string) {
    const cell = document.elementFromPoint(x,y)?.closest<HTMLElement>("[data-sticker-id]");
    const target = cell?.dataset.stickerId;
    if (!cell || !root.current?.contains(cell) || !target || target === moving) return null;
    const others = saved.items.filter(item => item.id !== moving), index = others.findIndex(item => item.id === target);
    if (index < 0) return null;
    const rect = cell.getBoundingClientRect(), before = x < rect.x + rect.width / 2;
    return { id: target, side: before ? "before" as const : "after" as const, afterId: before ? (index > 0 ? others[index-1].id : null) : target };
  }
  function showDrag(x: number, y: number) {
    const drag = pointerDrag.current;
    if (!drag) return;
    const placement = dragPlacement(x,y,drag.id);
    setDragPreview({ id:drag.id, x:Math.max(8,Math.min(innerWidth-drag.width-8,x-drag.offsetX)), y:Math.max(8,Math.min(innerHeight-drag.height-8,y-drag.offsetY)), width:drag.width, height:drag.height, src:drag.target.querySelector("img")?.src, dropId:placement?.id, side:placement?.side });
  }
  useEffect(() => { clearDrag(); }, [account, connection]);
  useEffect(() => { if (!edit || !open) clearDrag(); }, [edit, open]);
  useEffect(()=>{if(!edit) held.current=false;},[edit]);
  useEffect(() => { const abort = new AbortController(); scope.current = abort; return () => abort.abort(); }, [account, connection]);
  const signal = () => scope.current.signal;
  const guard = () => signal().throwIfAborted();
  const key = (intent: string) => { let value = keys.current.get(intent); if (!value) { value = crypto.randomUUID(); keys.current.set(intent, value); } return value; };
  async function refresh() {
    const [list, packs] = await Promise.all([client.customStickers(account, signal()), client.stickerPacks(account, { membership: "added" }, signal())]);
    guard(); setSaved(old => { const cached=new Map(old.items.map(item=>[item.id,item.original])); return {...list,items:list.items.map(item=>({...item,original:cached.get(item.id) ?? item.original}))}; }); setSavedLoaded(true); setAdded(packs.items); return packs;
  }
  async function act(action: () => Promise<unknown>) {
    if (busy) return;
    setBusy(true); setError("");
    try { await action(); } catch (error) {
      if (!signal().aborted) {
        if (error instanceof NativeApiError && error.status === 412 || error instanceof Error && error.message.includes("sticker_revision_mismatch")) await refresh().catch(()=>{});
        setError(readableError(error));
      }
    }
    finally { if (!signal().aborted) setBusy(false); }
  }
  useEffect(() => {
    if (!open) { setEdit(false); return; }
    void act(refresh);
    const off = connection.session.onEvent((event, changed) => {
      if (changed === account && (event.kind === "sticker-collections.changed" || event.kind === "account.push" && event.data.state === "connected")) { clearPackViews(); void refresh().catch(error => { if (!signal().aborted) setError(readableError(error)); }); }
    });
    const offGap=connection.session.onEventGap(changed=> {
      if(changed===account) { clearPackViews(); void refresh().catch(error=>{if(!signal().aborted) setError(readableError(error));}); }
    });
    return () => { off(); offGap(); clearTimeout(hold.current); };
  }, [open, account, connection]);
  useEffect(() => {
    if (!open) return;
    const dismiss = (event: PointerEvent) => { if (!root.current?.contains(event.target as Node) && !browser && !imports) setOpen(false); };
    document.addEventListener("pointerdown", dismiss);
    return () => document.removeEventListener("pointerdown", dismiss);
  }, [open, browser, imports !== null]);
  useEffect(() => { clearPackViews(); }, [account, connection]);
  useEffect(() => {
    if (!open || tab === "saved") return;
    const abort = new AbortController();
    const active = AbortSignal.any([signal(), abort.signal]);
    setPackItems({ id: tab, items: [], loading: true });
    setError("");
    void client.stickerPackItems(account, tab, "en", active).then(result => {
      active.throwIfAborted(); setPackItems({ id: tab, items: result.items, loading: false });
    }).catch(error => {
      if (!active.aborted) { setPackItems({ id: tab, items: [], loading: false }); setError(readableError(error)); }
    });
    return () => abort.abort();
  }, [open, tab, added]);
  async function choose(item: CustomSticker | StickerPackSticker, pack?: string) {
    if (edit || held.current || busy) return;
    await act(async () => {
      let asset = item.original;
      if (asset?.state !== "available") {
        await waitStickerJob(client, account, await client.downloadSticker(account, { stickerId: item.id, ...(pack ? { packId: pack } : {}) }, key(`download:${pack ?? "saved"}:${item.id}`), signal()), signal());
        keys.current.delete(`download:${pack ?? "saved"}:${item.id}`);
        asset = pack ? (await client.stickerPackItem(account, pack, item.id, "en", signal())).original : (await client.customSticker(account, item.id, signal())).original;
      }
      guard(); if (!asset || asset.state !== "available") throw Error(t("stickers.originalUnavailable"));
      stage({ source: pack ? { kind: "pack", packId: pack, stickerId: item.id } : { kind: "saved", stickerId: item.id }, asset, label: item.caption || t("stickers.sticker"), key: crypto.randomUUID() });
      setOpen(false);setBrowser(null);setDetail(null);
    });
  }
  async function move(item: string, after: string | null) {
    await act(async () => {
      // Refresh before resolving placement; the submitted If-Match still guards
      // phone edits that arrive between this read and the daemon's fresh read.
      const list = await client.customStickers(account, signal());
      await waitStickerJob(client, account, await client.moveCustomSticker(account, item, { position: { afterId: after } }, list.revision, crypto.randomUUID(), signal()), signal());
      await refresh();
    });
  }
  async function remove(item: string) {
    await act(async () => {
      await waitStickerJob(client, account, await client.removeCustomSticker(account, item, saved.revision, key(`remove:${item}:${saved.revision}`), signal()), signal());
      await refresh();
    });
  }
  async function browse(mode: "browse" | "added", next?: string) {
    setBrowser(mode); setDetail(null);
    if (mode === "added" || !next && Date.now() - catalogLoaded.current < 60_000) return;
    const epoch = viewEpoch.current;
    await act(async () => { const result = await client.stickerPacks(account, next ? { cursor: next } : {}, signal()); guard(); setCatalog(old => next ? [...old, ...result.items] : result.items); setCursor(result.nextCursor); if (epoch === viewEpoch.current) catalogLoaded.current = Date.now(); });
  }
  async function inspect(pack: StickerPack) {
    const epoch = viewEpoch.current;
    const cached = packViews.current.get(pack.id);
    if (cached && Date.now() - cached.at < 60_000) {
      setDetail(cached.detail); setDetailPreview(null); setDetailItems(cached.items); return;
    }
    await act(async () => {
      const [detail, contents] = await Promise.all([client.stickerPack(account, pack.id, signal()), client.stickerPackItems(account, pack.id, "en", signal())]);
      guard(); setDetail(detail); setDetailPreview(null); setDetailItems(contents.items);
      if (packViews.current.size >= 20) packViews.current.delete(packViews.current.keys().next().value!);
      if (epoch === viewEpoch.current) packViews.current.set(pack.id, { at: Date.now(), detail, items: contents.items });
    });
  }
  async function acquire(pack: string, selected?: string) {
    await act(async () => {
      await waitStickerJob(client, account, await client.downloadSticker(account, { packId: pack }, key(`pack-download:${pack}`), signal()), signal());
      keys.current.delete(`pack-download:${pack}`); packViews.current.delete(pack);
      const contents = await client.stickerPackItems(account, pack, "en", signal()); guard(); setDetailItems(contents.items);
      if(selected) setDetailPreview(contents.items.find(item=>item.id===selected) ?? null);
    });
  }
  async function membership(pack: StickerPack, add: boolean) {
    await act(async () => {
      const intent = `membership:${pack.id}:${add}`;
      await waitStickerJob(client, account, await (add ? client.addStickerPack(account, pack.id, key(intent), signal()) : client.removeStickerPack(account, pack.id, key(intent), signal())), signal());
      keys.current.delete(intent); clearPackViews();
      const packs = await refresh();
      const member = packs.items.find(item => item.id === pack.id);
      const fresh: StickerPack = { ...pack, membership: member ? "added" : "not_added", contentReadiness: member?.contentReadiness ?? "unverified" };
      // The completed mutation and canonical Added read establish membership.
      // Refresh media references after removal without repeating remote pack detail.
      guard(); setDetail(fresh); setDetailItems([]); setDetailPreview(null);
      setCatalog(old => old.map(item => item.id === pack.id ? fresh : item));
      if (!add && tab === pack.id) setTab("saved");
      // Membership is already confirmed. Loading previews must not delay that feedback.
      const contents = await client.stickerPackItems(account,pack.id,"en",signal());
      guard(); setDetailItems(contents.items);
    });
  }
  function drop(event: DragEvent) {
    event.preventDefault(); event.stopPropagation();
    if (event.dataTransfer.files.length) review(Array.from(event.dataTransfer.files));
  }
  function review(files: File[]) {
    if (busy) return;
    setEdit(false);
    const count=(imports?.length ?? 0)+files.length;
    const fresh: ImportItem[] = files.slice(0, 20).map(file => ({ id: crypto.randomUUID(), file, selected: true, results: {} }));
    setImports(old => [...(old ?? []),...fresh.slice(0,Math.max(0,20-(old?.length ?? 0)))]);
    if (count > 20) setError(t("stickers.batchLimit"));
    // Sizes and lengths are read locally so the first preparation already uses the defaults.
    for (const item of fresh) void readStickerSource(item.file).then(source => {
      const options = defaultOptions(source);
      updateImport(item.id, { source, options, want: requestKey(stickerRequest(source, options)) });
    });
  }
  function updateImport(id: string, update: Partial<ImportItem>) { setImports(old => old?.map(item => item.id === id ? { ...item, ...update } : item) ?? null); }
  const importOpen = imports !== null;
  useEffect(() => {
    if (!importOpen) return;
    const abort = new AbortController(); importScope.current = abort;
    const timers = adjustTimers.current;
    return () => { abort.abort(); for (const timer of timers.values()) clearTimeout(timer); timers.clear(); };
  }, [importOpen, account, connection]);
  const importSignal = () => AbortSignal.any([signal(), importScope.current.signal]);
  /** Follows a slider at once; prepares only after the slider has rested for a moment. */
  function adjust(id: string, change: (source: StickerSource, options: StickerOptions) => StickerOptions) {
    setImports(old => old?.map(item => {
      if (item.id !== id || !item.source || !item.options) return item;
      const options = change(item.source, item.options);
      const cached = item.results[requestKey(stickerRequest(item.source, options))]?.prepared;
      return { ...item, options, error: undefined, ...(cached ? { shown: cached } : {}) };
    }) ?? null);
    clearTimeout(adjustTimers.current.get(id));
    adjustTimers.current.set(id, setTimeout(() => {
      adjustTimers.current.delete(id);
      setImports(old => old?.map(item => item.id === id ? { ...item, want: liveKey(item) } : item) ?? null);
    }, 400));
  }
  /** A failed preparation is submitted again under a new key; the old job keeps its result. */
  function retry(id: string) {
    setImports(old => old?.map(item => {
      const key = liveKey(item);
      if (item.id !== id || !key) return item;
      const { [key]: _failed, ...results } = item.results;
      return { ...item, results, want: key, error: undefined };
    }) ?? null);
  }
  async function prepare(item: ImportItem, want: string) {
    preparing.current = true;
    const entry: PreparedEntry = { key: crypto.randomUUID(), addKey: crypto.randomUUID() };
    const store = (update: Partial<PreparedEntry>, more: (current: ImportItem) => Partial<ImportItem> = () => ({})) =>
      setImports(old => old?.map(current => current.id === item.id ? { ...current, ...more(current), results: { ...current.results, [want]: { ...entry, ...update } } } : current) ?? null);
    const active = importSignal();
    try {
      if (item.file.size === 0 || item.file.size > 64 * 1024 * 1024) { store({ error: t("stickers.sourceSize") }); return; }
      store({}); // An entry in progress keeps the row from being picked again.
      const sourceId = item.sourceId ?? (await client.upload(account, item.file, item.file.name, item.file.type || "application/octet-stream", active)).id;
      active.throwIfAborted(); updateImport(item.id, { sourceId });
      const request = { sourceId, ...(JSON.parse(want) as StickerRequestOptions) };
      const job = await waitStickerJob(client, account, await client.prepareSticker(account, request, entry.key, active), active);
      active.throwIfAborted();
      const prepared = job.result as unknown as PreparedSticker;
      // A result for options the row has since left is kept for later, not shown.
      store({ prepared }, current => liveKey(current) === want ? { shown: prepared } : {});
    } catch (error) {
      if (!active.aborted) { const failure = preparationError(error, item.source); store({ error: failure.text, retry: failure.retry }); }
    } finally { preparing.current = false; setQueue(value => value + 1); }
  }
  // One preparation at a time, for the first selected row whose settled options have none.
  useEffect(() => {
    if (preparing.current || !imports) return;
    const next = imports.find(item => item.selected && !item.added && item.want !== undefined && !item.results[item.want]);
    if (next?.want !== undefined) void prepare(next, next.want);
  }, [imports, queue]);
  async function addBatch() {
    await act(async () => {
      const selected = (imports ?? []).filter(item => item.selected);
      let allAdded = selected.length > 0;
      for (const item of selected) {
        if (item.added) continue;
        // Only the result prepared for the row's current options is added.
        const entry = liveEntry(item);
        if (!entry?.prepared) { allAdded = false; continue; }
        try {
          await waitStickerJob(client, account, await client.addCustomSticker(account, { source: { kind: "prepared", preparationId: entry.prepared.id } }, entry.addKey, signal()), signal());
          guard(); updateImport(item.id, { added: true, error: undefined });
        } catch (error) { allAdded = false; if (!signal().aborted) updateImport(item.id, { error: readableError(error) }); }
      }
      await refresh();
      guard();
      if (allAdded) { setTab("saved"); setImports(null); }
    });
  }
  const visible = tab === "saved" ? saved.items.slice(0,savedLimit) : packItems.id === tab ? packItems.items : [];
  const loading = tab === "saved" ? !savedLoaded || busy : packItems.id !== tab || packItems.loading;
  const ready = imports?.filter(item => item.selected && !item.added && liveEntry(item)?.prepared).length ?? 0;
  const pending = imports?.some(isPreparing) ?? false;
  return <div className="sticker-picker" ref={root} onKeyDown={event => { if (event.key === "Escape" && pointerDrag.current) { clearDrag(); event.preventDefault(); event.stopPropagation(); } else if (event.key === "Escape" && !browser && !imports) { setOpen(false); event.stopPropagation(); } }}>
    <button type="button" className="icon-btn" aria-label={t("stickers.open")} title={t("stickers.open")} aria-expanded={open} aria-controls={id} disabled={!enabled} onClick={() => setOpen(value => !value)}><Icon name="smile" /></button>
    {open && !browser && !imports && <section id={id} className="sticker-tray" data-composer-file-drop aria-label={t("stickers.picker")} onDragOver={event => { event.preventDefault(); event.stopPropagation(); }} onDrop={drop}>
      <div className="sticker-tabs" role="tablist" aria-label={t("stickers.collections")}>
        <button role="tab" aria-selected={tab === "saved"} onClick={() => { setTab("saved"); setEdit(false); }}>{t("stickers.saved")}</button>
        {added.slice(0, 3).map(pack => <button role="tab" key={pack.id} aria-selected={tab === pack.id} onClick={() => { setTab(pack.id); setEdit(false); }}>{pack.name}</button>)}
        <button disabled={busy} onClick={() => void browse("added")}>{t("stickers.allAdded")}</button>
      </div>
      <div key={tab} className={`sticker-grid ${edit ? "is-editing" : ""}`} role="group" aria-label={edit ? t("stickers.editSaved") : t("stickers.choose")}>
        {visible.map((item, index) => <div className={`sticker-cell${dragPreview?.id === item.id ? " is-drag-source" : ""}`} data-sticker-id={item.id} data-drop-side={dragPreview?.dropId === item.id ? dragPreview.side : undefined} key={item.id}>
          <button type="button" className="sticker-tile" draggable={false} disabled={busy} aria-label={item.caption || t("stickers.item", { index: index + 1 })}
            onPointerDown={event => {
              if (event.button !== 0 || !event.isPrimary) return;
              held.current=false;
              if(tab!=="saved" || busy) return;
              const target=event.currentTarget,pointer=event.pointerId,x=event.clientX,y=event.clientY,touch=event.pointerType==="touch";
              const begin=()=>{const rect=target.getBoundingClientRect();held.current=true;setEdit(true);pointerDrag.current={id:item.id,x,y,moved:false,pointer,target,width:rect.width,height:rect.height,offsetX:touch ? rect.width/2 : x-rect.x,offsetY:touch ? rect.height+14 : y-rect.y};try{target.setPointerCapture(pointer);}catch{}showDrag(x,y);};
              if(edit) begin();else hold.current=setTimeout(begin,500);
            }}
            onPointerMove={event=>{const drag=pointerDrag.current;if(!drag) return;if(Math.hypot(event.clientX-drag.x,event.clientY-drag.y)>6) drag.moved=true;showDrag(event.clientX,event.clientY);}}
            onPointerUp={event=> {
              const drag=pointerDrag.current;clearDrag();
              if(!drag?.moved) return;
              const placement=dragPlacement(event.clientX,event.clientY,drag.id);
              if(placement) void move(drag.id,placement.afterId);
            }} onPointerCancel={clearDrag} onLostPointerCapture={clearDrag} onPointerLeave={()=>clearTimeout(hold.current)}
            onClick={() => void choose(item, tab === "saved" ? undefined : tab)}>
            <StickerTileImage key={`${account}:${tab}:${item.id}`} client={client} account={account} item={item} pack={tab === "saved" ? undefined : tab} />
          </button>
          {edit && <><button className="sticker-remove" aria-label={t("stickers.removeItem", { index: index + 1 })} disabled={busy} onClick={() => void remove(item.id)}>×</button>
            <div className="sticker-keyboard-move"><button disabled={busy || index === 0} aria-label={t("stickers.moveEarlier", { index: index + 1 })} onClick={() => void move(item.id, index > 1 ? saved.items[index - 2].id : null)}>←</button><button disabled={busy || index === saved.items.length - 1} aria-label={t("stickers.moveLater", { index: index + 1 })} onClick={() => void move(item.id, saved.items[index + 1].id)}>→</button></div></>}
        </div>)}
        {!visible.length && !error && <p className="sticker-empty" role="status">{loading ? t("stickers.loading") : t("stickers.empty")}</p>}
        {tab==="saved" && savedLimit<saved.items.length && <button onClick={()=>setSavedLimit(count=>count+25)}>{t("stickers.moreSaved")}</button>}
      </div>
      {error && <p className="sticker-error" role="alert">{error}</p>}
      <footer>{edit ? <><span>{t("stickers.dragToReorder")}</span><button onClick={() => setEdit(false)}>{t("stickers.done")}</button></> : <><button disabled={busy} onClick={() => { setImports([]); fileInput.current?.click(); }}>{t("stickers.addStickers")}</button><button disabled={busy} onClick={() => void browse("browse")}>{t("stickers.browsePacks")}</button>{tab === "saved" && <button disabled={busy} onClick={() => setEdit(true)}>{t("stickers.edit")}</button>}</>}</footer>
    </section>}
    <input className="attachment-input" ref={fileInput} type="file" multiple accept="image/*,video/mp4,video/quicktime" aria-label={t("stickers.chooseFiles")} onChange={event => { const files = Array.from(event.target.files ?? []); event.target.value = ""; if (files.length) review(files); }} />
    {dragPreview && createPortal(<div className="sticker-drag-preview" data-sticker-drag-id={dragPreview.id} aria-hidden="true" style={{ width:dragPreview.width, height:dragPreview.height, transform:`translate3d(${dragPreview.x}px,${dragPreview.y}px,0)` }}>{dragPreview.src ? <img src={dragPreview.src} alt="" draggable={false} /> : <span className="sticker-unloaded"><Icon name="smile" /></span>}</div>,document.body)}
    {browser && <StickerModal label={detail?.name || t("stickers.browsePacks")} close={() => { setBrowser(null); setDetail(null); }}>
      <header><div><h2>{detail?.name || t("stickers.browsePacks")}</h2>{detail && <p>{detail.description}</p>}</div><Button icon="close" label={t("stickers.close")} onClick={() => { setBrowser(null); setDetail(null); }} /></header>
      {detail ? <><div className="sticker-detail-cover"><StickerImage client={client} account={account} asset={detailPreview?.original || detailPreview?.thumbnail || detail.cover || detail.icon} label={detailPreview?.caption || detail.name} acquire /></div><div className="sticker-pack-actions"><button onClick={() => setDetail(null)}>{t("stickers.back")}</button><span>{detail.membership === "added" ? t("stickers.added") : t("stickers.notAdded")}</span><button disabled={busy || (detail.membership !== "added" && detail.eligible !== true)} onClick={() => void membership(detail, detail.membership !== "added")}>{detail.membership === "added" ? t("stickers.removePack") : t("stickers.addPack")}</button></div>
        {detail.membership!=="added" && <p>{t("stickers.previewHint")}</p>}<div className="sticker-grid pack-detail-grid">{detailItems.map((item, index) => <button key={item.id} className="sticker-pack-item" disabled={busy} aria-label={t(detail.membership === "added" ? "stickers.useItem" : "stickers.previewItem", { name: item.caption || String(index + 1) })} onClick={() => { if(detail.membership==="added") void choose(item, detail.id); else if(item.original?.state==="available") setDetailPreview(item); else void acquire(detail.id,item.id); }}><StickerImage client={client} account={account} asset={item.original?.state === "available" ? item.original : item.thumbnail} label={item.caption || t("stickers.sticker")} acquire /><span>{item.caption}</span></button>)}</div>
        <details className="sticker-pack-id"><summary>{t("stickers.packId")}</summary><code>{detail.id}</code><button onClick={() => void navigator.clipboard.writeText(detail.id).catch(() => setError(t("stickers.copyFailed")))}>{t("stickers.copy")}</button></details>
      </> : <><nav className="sticker-browser-tabs"><button aria-pressed={browser === "browse"} onClick={() => void browse("browse")}>{t("stickers.browse")}</button><button aria-pressed={browser === "added"} onClick={() => void browse("added")}>{t("stickers.added")}</button></nav><div className="sticker-pack-cards">{(browser === "added" ? added : catalog).map(pack => <button key={pack.id} onClick={() => void inspect(pack)} className="sticker-pack-card" disabled={busy}><StickerImage client={client} account={account} asset={pack.icon} label={pack.name} acquire /><span><strong>{pack.name}</strong><small>{pack.membership === "added" ? t("stickers.added") : t("stickers.previewPack")}</small></span><Icon name="chevron" /></button>)}</div>{browser === "browse" && cursor && <button disabled={busy} onClick={() => void browse("browse", cursor)}>{t("stickers.morePacks")}</button>}</>}
      {busy && <p role="status">{t("stickers.working")}</p>}{error && <p role="alert" className="sticker-error">{error}</p>}
    </StickerModal>}
    {imports && <StickerModal className="sticker-import-modal" label={t("stickers.addToSaved")} close={() => { if (!busy) setImports(null); }} onDrop={drop}>
      <header><div><h2>{t("stickers.addToSaved")}</h2><p>{t("stickers.importHint")}</p></div><Button icon="close" label={t("stickers.close")} disabled={busy} onClick={() => setImports(null)} /></header>
      <div className="sticker-import-list">{imports.map(item => <StickerImportRow key={item.id} client={client} account={account} item={item} disabled={busy}
        select={selected => updateImport(item.id, { selected })} adjust={change => adjust(item.id, change)} retry={() => retry(item.id)} />)}</div>
      <footer><button disabled={busy} onClick={() => fileInput.current?.click()}>{t("stickers.chooseFiles")}</button><button className="sticker-add-submit" disabled={busy || !ready || pending} onClick={() => void addBatch()}>{busy ? t("stickers.saving") : t("stickers.addCount", { count: ready })}</button></footer>{error && <p role="alert">{error}</p>}
    </StickerModal>}
  </div>;
}

/** Plain-language results for the limits a person can fix with the controls. */
function preparationError(error: unknown, source?: StickerSource): { text: string; retry: boolean } {
  const reason = error instanceof StickerJobError ? error.reason : error instanceof NativeApiError ? error.code : "";
  const video = source?.kind === "video";
  if (reason === "sticker_output_size_limit") return { text: t(video ? "stickers.tooLargeVideo" : "stickers.tooLargeImage"), retry: false };
  if (reason === "sticker_adjustment_required") return { text: t(video ? "stickers.tooLongVideo" : "stickers.tooLongGif"), retry: false };
  if (reason === "sticker_input_unsupported") return { text: t("stickers.inputUnsupported"), retry: false };
  return { text: readableError(error), retry: true };
}

function preparedSummary(prepared: PreparedSticker) {
  const bytes = prepared.original.byteLength;
  const values = { width: prepared.width, height: prepared.height, size: bytes ? Math.max(1, Math.round(bytes / 1024)) : "–" };
  return prepared.durationMs > 0 ? t("stickers.preparedDimensions", { ...values, seconds: (prepared.durationMs / 1000).toFixed(1) }) : t("stickers.preparedStill", values);
}

const seconds = (ms: number) => (ms / 1000).toFixed(1);

type ImportRowProps = { client: NativeWeChatClient; account: string; item: ImportItem; disabled: boolean; select: (selected: boolean) => void; adjust: (change: (source: StickerSource, options: StickerOptions) => StickerOptions) => void; retry: () => void };
function StickerImportRow({ client, account, item, disabled, select, adjust, retry }: ImportRowProps) {
  const entry = liveEntry(item), prepared = entry?.prepared ?? item.shown, preparing = isPreparing(item);
  return <article className="sticker-import-item">
    <input type="checkbox" aria-label={t("stickers.includeFile", { name: item.file.name })} checked={item.selected} disabled={disabled || item.added} onChange={event => select(event.target.checked)} />
    <StickerImage client={client} account={account} asset={prepared?.original} label={item.file.name} />
    <div className="sticker-import-info">
      <strong>{item.file.name}</strong>
      <p className="sticker-import-status">{item.added ? t("stickers.added") : prepared && <span className={entry?.prepared ? undefined : "is-stale"}>{preparedSummary(prepared)}</span>}<span role="status">{preparing ? t("stickers.preparing") : ""}</span></p>
      {(entry?.error || item.error) && <p role="alert" className="sticker-error">{entry?.error || item.error}</p>}
      {entry?.retry && !item.added && <button className="sticker-import-retry" disabled={disabled} onClick={retry}>{t("stickers.prepare")}</button>}
    </div>
    {!item.added && item.source && item.options && <StickerImportControls name={item.file.name} id={item.id} source={item.source} options={item.options} disabled={disabled} adjust={adjust} />}
  </article>;
}

function StickerImportControls({ name, id, source, options, disabled, adjust }: { name: string; id: string; source: StickerSource; options: StickerOptions; disabled: boolean; adjust: ImportRowProps["adjust"] }) {
  const stops = sizeStops(source), stop = stops[Math.min(options.size, stops.length - 1)], range = trimRange(source);
  const video = source.kind === "video", fps = FPS_STOPS[options.fps];
  return <div className="sticker-import-controls" role="group" aria-label={t("stickers.optionsFor", { name })}>
    <StickerSlider id={`${id}-size`} label={t("stickers.size")} min={0} max={stops.length - 1} value={Math.min(options.size, stops.length - 1)} disabled={disabled}
      text={t("stickers.sizeValue", { px: stop.px })} valueText={t(stop.original ? "stickers.sizeOriginalLong" : "stickers.sizeValueLong", { px: stop.px })}
      ends={[t("stickers.smaller"), !video && stops[stops.length - 1].original ? t("stickers.original") : t("stickers.larger")]}
      mark={stopPosition(stops, PHONE_STICKER_EDGE)} markLabel={t("stickers.phoneSize", { px: PHONE_STICKER_EDGE })}
      onChange={size => adjust((_, current) => ({ ...current, size }))} />
    {!video && exceedsStickerEdge(source) && stop.px === MAX_STICKER_EDGE && <p className="sticker-import-note">{t("stickers.shrunkToLimit")}</p>}
    {video && <StickerSlider id={`${id}-fps`} label={t("stickers.smoothness")} min={0} max={FPS_STOPS.length - 1} value={options.fps} disabled={disabled}
      text={t("stickers.fpsValue", { fps })} valueText={t("stickers.fpsValueLong", { fps })} ends={[t("stickers.smallerFile"), t("stickers.smoother")]}
      onChange={value => adjust((_, current) => ({ ...current, fps: value }))} />}
    {range && <>
      <StickerSlider id={`${id}-start`} label={t("stickers.start")} min={0} max={range.totalMs - 100} step={100} value={options.startMs} disabled={disabled}
        text={t("stickers.secondsValue", { seconds: seconds(options.startMs) })} valueText={t("stickers.secondsValueLong", { seconds: seconds(options.startMs) })}
        onChange={value => adjust((current, previous) => withStart(current, previous, value))} />
      <StickerSlider id={`${id}-length`} label={t("stickers.length")} min={100} max={maxLength(range, options.startMs)} step={100} value={options.lengthMs} disabled={disabled}
        text={t("stickers.secondsValue", { seconds: seconds(options.lengthMs) })} valueText={t("stickers.secondsValueLong", { seconds: seconds(options.lengthMs) })}
        onChange={value => adjust((current, previous) => withLength(current, previous, value))} />
    </>}
  </div>;
}

type SliderProps = { id: string; label: string; min: number; max: number; step?: number; value: number; text: string; valueText: string; disabled: boolean; ends?: [string, string]; mark?: number; markLabel?: string; onChange: (value: number) => void };
/** A labelled range with its current value; a single choice shows only the value. */
function StickerSlider({ id, label, min, max, step = 1, value, text, valueText, disabled, ends, mark, markLabel, onChange }: SliderProps) {
  if (max <= min) return <div className="sticker-control"><span>{label}</span><span /><span className="sticker-control-value">{text}</span></div>;
  const note = mark !== undefined ? `${id}-mark` : undefined;
  return <div className="sticker-control">
    <label htmlFor={id}>{label}</label>
    <div className="sticker-range">
      <input id={id} type="range" min={min} max={max} step={step} value={value} disabled={disabled} aria-valuetext={valueText} aria-describedby={note} onChange={event => onChange(Number(event.currentTarget.value))} />
      {(ends || note) && <div className="sticker-range-scale" aria-hidden="true">
        {ends && <><span>{ends[0]}</span><span>{ends[1]}</span></>}
        {note && <span className="sticker-range-mark" style={{ "--at": mark } as CSSProperties} title={markLabel} />}
      </div>}
      {note && <span id={note} className="sr-only">{markLabel}</span>}
    </div>
    <span className="sticker-control-value">{text}</span>
  </div>;
}

function StickerModal({ label, close, children, onDrop, className }: { label: string; close: () => void; children: React.ReactNode; onDrop?: (event: DragEvent) => void; className?: string }) {
  const ref = useRef<HTMLDialogElement>(null);
  useEffect(() => { ref.current?.showModal(); return () => ref.current?.close(); }, []);
  return createPortal(<dialog ref={ref} className={`sticker-modal${className ? ` ${className}` : ""}`} aria-label={label} onCancel={event => { event.preventDefault(); close(); }} onClick={event => { if (event.target === event.currentTarget) close(); }} onDragOver={event => { if (onDrop) { event.preventDefault(); event.stopPropagation(); } }} onDrop={onDrop}><div className="sticker-modal-body">{children}</div></dialog>, document.body);
}
