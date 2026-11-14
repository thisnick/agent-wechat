import { useEffect, useId, useRef, useState, type DragEvent } from "react";
import { createPortal } from "react-dom";
import type { CustomSticker, CustomStickerList, MediaAsset, NativeWeChatClient, PreparedSticker, StickerPack, StickerPackSticker, StickerSendSource } from "@agent-wechat/shared";
import { NativeApiError } from "@agent-wechat/shared";
import type { Connection } from "./connection.ts";
import { readableError } from "./session.ts";
import { t } from "./i18n.ts";
import { Button, Icon } from "./ui.tsx";
import { waitStickerJob } from "./sticker-jobs.ts";
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

type ImportItem = { id: string; file: File; key: string; prepareKey: string; sourceId?: string; selected: boolean; prepared?: PreparedSticker; error?: string; busy?: boolean; added?: boolean; fit: boolean; start: string; duration: string };
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
  const preparing = useRef(false);
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
    setImports(old => [...(old ?? []),...files.slice(0,Math.max(0,20-(old?.length ?? 0))).map(file => ({ id: crypto.randomUUID(), file, key: crypto.randomUUID(), prepareKey: crypto.randomUUID(), selected: true, fit: false, start: "", duration: "" }))]);
    if (count > 20) setError(t("stickers.batchLimit"));
  }
  function updateImport(id: string, update: Partial<ImportItem>) { setImports(old => old?.map(item => item.id === id ? { ...item, ...update } : item) ?? null); }
  async function prepare(item: ImportItem) {
    if (preparing.current) return;
    preparing.current=true;
    updateImport(item.id, { busy: true, error: undefined });
    try {
      if (item.file.size===0 || item.file.size>64*1024*1024) throw Error(t("stickers.sourceSize"));
      const sourceId = item.sourceId ?? (await client.upload(account, item.file, item.file.name, item.file.type || "application/octet-stream", signal())).id;
      guard();updateImport(item.id,{sourceId});
      const start = Number(item.start || 0), duration = item.duration ? Number(item.duration) : undefined;
      if (!Number.isFinite(start) || start < 0 || item.start!=="" && duration===undefined || (duration !== undefined && (!Number.isFinite(duration) || duration <= 0 || duration > 10))) throw Error(t("stickers.invalidTrim"));
      const job = await waitStickerJob(client, account, await client.prepareSticker(account, { sourceId, fit: item.fit, trim: duration === undefined ? null : { startMs: Math.round(start * 1000), durationMs: Math.round(duration * 1000) } }, item.prepareKey, signal()), signal());
      guard(); updateImport(item.id, { prepared: job.result as unknown as PreparedSticker, busy: false });
    } catch (error) { if (!signal().aborted) updateImport(item.id, { error: readableError(error), busy: false }); }
    finally {preparing.current=false;}
  }
  useEffect(()=> {
    const next=imports?.find(item=>item.selected && !item.prepared && !item.error && !item.busy && !item.added);
    if (next && !preparing.current) void prepare(next);
  },[imports]);
  async function addBatch() {
    await act(async () => {
      const selected = (imports ?? []).filter(item => item.selected);
      let allAdded = selected.length > 0;
      for (const item of selected) {
        if (item.added) continue;
        if (!item.prepared) { allAdded = false; continue; }
        try {
          await waitStickerJob(client, account, await client.addCustomSticker(account, { source: { kind: "prepared", preparationId: item.prepared.id } }, item.key, signal()), signal());
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
  const ready = imports?.filter(item => item.selected && item.prepared && !item.added).length ?? 0;
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
    {imports && <StickerModal label={t("stickers.addToSaved")} close={() => { if (!busy) setImports(null); }} onDrop={drop}>
      <header><div><h2>{t("stickers.addToSaved")}</h2><p>{t("stickers.importHint")}</p></div><Button icon="close" label={t("stickers.close")} disabled={busy} onClick={() => setImports(null)} /></header>
      <div className="sticker-import-list">{imports.map(item => <article key={item.id} className="sticker-import-item"><input type="checkbox" aria-label={t("stickers.includeFile", { name: item.file.name })} checked={item.selected} disabled={busy || item.added} onChange={event => updateImport(item.id, { selected: event.target.checked })} /><StickerImage client={client} account={account} asset={item.prepared?.original} label={item.file.name} /><div><strong>{item.file.name}</strong>{item.added ? <p>{t("stickers.added")}</p> : item.prepared ? <p>{t("stickers.preparedDimensions", { width: item.prepared.width, height: item.prepared.height, seconds: item.prepared.durationMs / 1000 })}</p> : <button disabled={item.busy || busy} onClick={() => void prepare(item)}>{item.busy ? t("stickers.preparing") : t("stickers.prepare")}</button>}
        {item.error && <p role="alert" className="sticker-error">{item.error}</p>}
        {!item.added && <details><summary>{t("stickers.adjust")}</summary><label><input type="checkbox" disabled={busy} checked={item.fit} onChange={event => updateImport(item.id, { fit: event.target.checked, prepared: undefined, prepareKey: crypto.randomUUID(), key: crypto.randomUUID() })} />{t("stickers.fit")}</label><label>{t("stickers.startSeconds")}<input type="number" disabled={busy} min="0" step="0.1" value={item.start} onChange={event => updateImport(item.id, { start: event.target.value, prepared: undefined, prepareKey: crypto.randomUUID(), key: crypto.randomUUID() })} /></label><label>{t("stickers.durationSeconds")}<input type="number" disabled={busy} min="0.1" max="10" step="0.1" value={item.duration} onChange={event => updateImport(item.id, { duration: event.target.value, prepared: undefined, prepareKey: crypto.randomUUID(), key: crypto.randomUUID() })} /></label></details>}
      </div></article>)}</div>
      <footer><button disabled={busy} onClick={() => fileInput.current?.click()}>{t("stickers.chooseFiles")}</button><button className="sticker-add-submit" disabled={busy || !ready} onClick={() => void addBatch()}>{busy ? t("stickers.saving") : t("stickers.addCount", { count: ready })}</button></footer>{error && <p role="alert">{error}</p>}
    </StickerModal>}
  </div>;
}

function StickerModal({ label, close, children, onDrop }: { label: string; close: () => void; children: React.ReactNode; onDrop?: (event: DragEvent) => void }) {
  const ref = useRef<HTMLDialogElement>(null);
  useEffect(() => { ref.current?.showModal(); return () => ref.current?.close(); }, []);
  return createPortal(<dialog ref={ref} className="sticker-modal" aria-label={label} onCancel={event => { event.preventDefault(); close(); }} onClick={event => { if (event.target === event.currentTarget) close(); }} onDragOver={event => { if (onDrop) { event.preventDefault(); event.stopPropagation(); } }} onDrop={onDrop}><div className="sticker-modal-body">{children}</div></dialog>, document.body);
}
