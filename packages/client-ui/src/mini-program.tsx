import { createContext, useCallback, useContext, useEffect, useState, useSyncExternalStore, type ReactNode } from "react";
import { AppWindow, ArrowUpRight } from "lucide-react";
import type { MiniProgramCard as Card, NativeMessage, NativeMiniProgram } from "@agent-wechat/shared";
import type { Connection } from "./connection.ts";
import { desktopInvoke } from "./desktop.ts";
import { emojiText } from "./emoji-text.ts";
import { t } from "./i18n.ts";
import { ImageThumbnail } from "./shared-link.tsx";
import { useVisible } from "./visible.tsx";
import "./mini-program.css";

type Programs = {
  connection: Connection;
  account: string;
  accountName: string;
  items: NativeMiniProgram[] | null;
  failed: boolean;
  refresh: () => void;
  load: () => void;
};
const ProgramsContext = createContext<Programs | null>(null);

export function MiniProgramsProvider({ connection, account, accountName, active, children }: {
  connection: Connection;
  account: string;
  accountName: string;
  active: boolean;
  children: ReactNode;
}) {
  const [result, setResult] = useState<{ items: NativeMiniProgram[]; failed: boolean }>();
  const [attempt, setAttempt] = useState(0);
  const [requested, setRequested] = useState(false);
  const load = useCallback(() => setRequested(true), []);
  const enabled = !!account && (active || requested);
  useEffect(() => {
    if (!enabled) return;
    let current = true;
    void connection.session.client.listMiniPrograms(account).then(
      ({ items }) => { if (current) setResult({ items, failed: false }); },
      () => { if (current) setResult({ items: [], failed: true }); },
    );
    return () => { current = false; };
  }, [account, connection, attempt, enabled]);
  const refresh = () => {
    setResult(undefined);
    setAttempt(value => value + 1);
  };
  return <ProgramsContext.Provider value={{
    account, accountName, connection, items: result?.items ?? null, failed: result?.failed ?? false, refresh, load,
  }}>{children}</ProgramsContext.Provider>;
}

function OpenProgram({ appId, title, windowTitle, source, unavailable, blocked = false }: {
  appId: string;
  title: string;
  windowTitle?: string;
  source?: string;
  unavailable?: string;
  blocked?: boolean;
}) {
  const programs = useContext(ProgramsContext);
  const [opening, setOpening] = useState(false);
  const [notice, setNotice] = useState("");
  if (!programs) return null;
  const { account, accountName, connection } = programs;
  const preview = connection.simulator;
  const disabledReason = unavailable ||
    (!connection.desktopId && !preview ? t("miniPrograms.openInDesktop") : "");
  async function open() {
    setNotice("");
    setOpening(true);
    try {
      if (connection.desktopId) {
        await desktopInvoke("desktop_open_mini_program", { request: {
          connectionId: connection.desktopId, accountId: account, appId,
          sourceMessageId: source, title: windowTitle || title, accountName,
        } });
      } else {
        await connection.session.client.prepareMiniProgram(account, appId, { sourceMessageId: source });
        setNotice(t("miniPrograms.previewReady"));
      }
    } catch {
      setNotice(t("miniPrograms.couldNotOpen"));
    } finally {
      setOpening(false);
    }
  }
  return <div className="mini-program-action">
    <button type="button" disabled={opening || blocked || !!disabledReason} onClick={() => void open()}
      aria-label={t("miniPrograms.openNamed", { title })}>
      {opening ? t("miniPrograms.opening") : t("miniPrograms.open")} <ArrowUpRight size={14} aria-hidden="true" />
    </button>
    {(notice || disabledReason) && <small role="status">{notice || disabledReason}</small>}
  </div>;
}

export function MiniProgramsList() {
  const programs = useContext(ProgramsContext);
  if (!programs) return null;
  const { items, failed, refresh, accountName } = programs;
  return <section className="mini-program-list" aria-labelledby="mini-programs-heading">
    <header className="mini-program-list-header">
      <div>
        <div className="mini-program-heading">
          <h1 id="mini-programs-heading">{t("miniPrograms.title")}</h1>
          <span className="mini-program-experimental">{t("miniPrograms.experimental")}</span>
        </div>
        <p>{t("miniPrograms.availableFor", { account: accountName })}</p>
      </div>
      <button type="button" onClick={refresh}>{t("miniPrograms.refresh")}</button>
    </header>
    {items === null ? <p role="status">{t("miniPrograms.loading")}</p>
      : failed ? <p role="status">{t("miniPrograms.couldNotLoad")}</p>
      : !items.length ? <p>{t("miniPrograms.empty")}</p>
      : <ul>{items.map(program => <li key={program.appId}>
        <span className="mini-program-icon"><AppWindow size={20} aria-hidden="true" /></span>
        <div className="mini-program-name"><strong>{emojiText(program.title)}</strong>
          <small>{program.available ? t("miniPrograms.available") : t("miniPrograms.packageUnavailable")}</small></div>
        <OpenProgram appId={program.appId} title={program.title} blocked={!program.available} />
      </li>)}</ul>}
  </section>;
}

export function MiniProgramCard({ card, message, connection }: {
  card: Card;
  message?: NativeMessage;
  connection: Connection;
}) {
  const title = card.title || card.displayName || t("miniPrograms.singular");
  return message ? <SavedMiniProgram card={card} title={title} message={message} connection={connection} />
    : <div className="mini-program-quote">
        <div className="mini-program-card-label">
          <AppWindow size={17} aria-hidden="true" />
          <span>{emojiText(card.displayName || t("miniPrograms.singular"))}</span>
        </div>
        <strong>{emojiText(title)}</strong>
      </div>;
}

function CardOpen({ card, title, message }: { card: Card; title: string; message: NativeMessage }) {
  const programs = useContext(ProgramsContext);
  let unavailable: string | undefined;
  if (!card.pagePath) unavailable = t("miniPrograms.sharedPageUnavailable");
  else if (programs?.failed) unavailable = t("miniPrograms.availabilityUnknown");
  else if (!programs?.items) unavailable = t("miniPrograms.checkingAvailability");
  else if (!programs.items.some(item => item.appId === card.appId && item.available)) {
    unavailable = t("miniPrograms.packageUnavailable");
  }
  return <OpenProgram appId={card.appId} source={message.id} title={title} windowTitle={card.displayName || undefined} unavailable={unavailable} />;
}

function SavedMiniProgram({ card, title, message, connection }: {
  card: Card; title: string; message: NativeMessage; connection: Connection;
}) {
  const { element, visible } = useVisible<HTMLDivElement>();
  const model = connection.incomingMedia.get(message.accountId, message.id);
  const { assets } = useSyncExternalStore(model.subscribe, model.snapshot);
  const { opening } = useSyncExternalStore(connection.session.subscribe, connection.session.snapshot);
  const loadPrograms = useContext(ProgramsContext)?.load;
  const icon = assets.find(asset => asset.filename === "mini-program-icon");
  const thumbnail = assets.find(asset => asset.filename === "mini-program-preview");
  useEffect(() => {
    if (visible) {
      loadPrograms?.();
      void model.load();
    }
  }, [model, visible, loadPrograms]);
  useEffect(() => {
    if (visible && !opening) {
      if (icon) model.ensure(icon);
      if (thumbnail) model.ensure(thumbnail);
    }
  }, [model, opening, icon, thumbnail, visible]);
  return <div ref={element} className="bubble mini-program-card">
    <div className="mini-program-card-label">
      <span className="mini-program-card-icon" aria-hidden="true">
        <ImageThumbnail asset={icon} connection={connection} fallback={<AppWindow size={17} />} />
      </span>
      <span>{emojiText(card.displayName || t("miniPrograms.singular"))}</span>
    </div>
    <strong>{emojiText(title)}</strong>
    {thumbnail && <div className="mini-program-art" aria-hidden="true">
      <ImageThumbnail asset={thumbnail} connection={connection} fallback={<AppWindow size={36} />} />
    </div>}
    <CardOpen card={card} title={title} message={message} />
  </div>;
}
