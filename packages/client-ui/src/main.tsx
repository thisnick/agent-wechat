import { ConversationRow } from "./conversation-row.tsx";
import { ThreadPinSwitch, useThreadPinPreview } from "./thread-pinning.tsx";
import { ThreadNotifications } from "./thread-notifications.tsx";
import { emojiText } from "./emoji-text.ts";
import {
  dateLocale,
  setLocale,
  syncDesktopLocale,
  t,
  useLocale,
  type Locale,
} from "./i18n.ts";
import { DesktopUpdates } from "./desktop-updates.tsx";
import { desktop, desktopConnect, desktopInvoke, desktopNotifications, installDesktopLinks, type DesktopSession, type DesktopSettings } from "./desktop.ts";
import { DesktopWelcome, DesktopPreferences } from "./desktop-ui.tsx";
import { LocalCacheSettings } from "./local-cache-settings.tsx";
import { GroupSettings } from "./group-settings.tsx";
import { createRoot } from "react-dom/client";
import { flushSync } from "react-dom";
import {
  useEffect,
  useRef,
  useState,
  useSyncExternalStore,
  type FormEvent,
  type ReactNode,
} from "react";
import {
  NativeApiError,
  type NativeMessage,
  type NativeOperation,
  isNativeTombstone,
} from "@agent-wechat/shared";
import {
  bootstrap,
  connect,
  control,
  simulatorClock,
  previewHubs,
  type PreviewHub,
  type Connection,
} from "./connection.ts";
import {
  readableError,
  pause,
  type Account,
  type PendingSend,
  inventory,
} from "./session.ts";
import { Avatar, Button, Icon, Logo, Welcome } from "./ui.tsx";
import { HistorySearch } from "./history-search.ts";
import { MediaCard, PendingAttachments } from "./media.tsx";
import { AttachmentPicker, AttachmentTray } from "./attachment-picker.tsx";
import { sendAttachmentBatch } from "./attachment-drafts.ts";
import { ComposerHelp } from "./composer-help.tsx";
import { ConversationAvatar, HubAvatar } from "./avatars.tsx";
import { orderThreads } from "./thread-preview.ts";
import {
  messagePresentation,
  recallText,
  textMessageLink,
} from "./message-presentation.ts";
import { AccountSettings, ProfileDialog } from "./account-settings.tsx";
import { NewChat } from "./new-chat.tsx";
import { QuotedReply } from "./quoted-reply.tsx";
import { quotedText } from "./quote-preview.ts";
import { PhotoViewer } from "./photo-viewer.tsx";
import { router, useHash, useRoute } from "./router.ts";
import {
  formatRoute,
  isAccountRoute,
  isChatsRoute,
  isViewRoute,
  parseRoute,
  type ChatsRoute,
  type Route,
  type ViewRoute,
} from "./routes.ts";
import { conversationAvailable } from "./route-targets.ts";
import { RouteMessage } from "./route-message.tsx";
import { MessageActions } from "./message-actions.tsx";
import { ReplyContext } from "./reply-context.ts";
import { canReply } from "./reply.ts";
import {
  ConversationNavigation,
  useConversationNavigation,
} from "./conversation-navigation.tsx";
import {
  ConversationScroll,
  ConversationItem,
} from "./conversation-scroll.tsx";
import "./style.css";
import "./onboarding.css";
import "./client.css";
import { SharedLink } from "./shared-link.tsx";
import { LocationMessage } from "./location-message.tsx";
import { SystemNotice } from "./system-notice.tsx";
import { MentionEditor } from "./mention-editor.tsx";
import { allMentionProblem, canMentionAll, removeAllMentions, validMentionTokens, type MentionToken } from "./mentions.ts";
const EMPTY_MENTIONS: MentionToken[] = [];

function date(value: string | null): Date {
  return new Date(
    value && /^\d+$/.test(value) ? Number(value) * 1000 : value ?? 0,
  );
}
function time(value: string | null): string {
  return value
    ? date(value).toLocaleTimeString(dateLocale(), {
        hour: "2-digit",
        minute: "2-digit",
      })
    : "";
}
function useTheme() {
  const [theme, setTheme] = useState(() => {
    try {
      return localStorage.getItem("agentwechat-theme") ?? "system";
    } catch {
      return "system";
    }
  });
  useEffect(() => {
    const media = matchMedia("(prefers-color-scheme: dark)");
    const apply = () => {
      const dark = theme === "dark" || (theme === "system" && media.matches);
      document.body.classList.toggle("dark", dark);
      document.documentElement.dataset.theme = dark ? "dark" : "light";
      document.documentElement.style.colorScheme = dark ? "dark" : "light";
    };
    apply();
    media.addEventListener("change", apply);
    try {
      localStorage.setItem("agentwechat-theme", theme);
    } catch {
      /* Optional preferences. */
    }
    return () => media.removeEventListener("change", apply);
  }, [theme]);
  return [theme, setTheme] as const;
}
/** Where a hub opens when the address names nothing usable: the account in use last, else the account list. */
function homeRoute(connection: Connection, accounts: Account[]): Exclude<Route, { name: "servers" }> {
  const server = connection.serverId;
  const last = connection.cache.lastAccount();
  const returning = accounts.find((account) => account.id === last && account.authenticated === true);
  return returning
    ? { name: connection.cache.view(returning.id).section === "contacts" ? "contacts" : "chats", server, account: returning.id }
    : { name: "accounts", server };
}
function ErrorNotice({ children }: { children: ReactNode }) {
  return (
    <div className="connection-error" role="alert">
      <Icon name="help" />
      <span>{children}</span>
    </div>
  );
}
function App() {
  useLocale();
  useEffect(() => desktop ? installDesktopLinks() : undefined, []);
  const [connection, setConnection] = useState<Connection | null>(null);
  const [starting, setStarting] = useState(true);
  // Choosing a hub is a view of the address, so Back returns from it.
  const changingHub = useRoute()?.name === "servers";
  const [error, setError] = useState("");
  const [theme, setTheme] = useTheme();
  useEffect(() => {
    let cancelled = false;
    if (desktop) {
      void desktopInvoke<DesktopSettings>("desktop_settings")
        .then((settings) => {
          if (!cancelled) syncDesktopLocale(settings.language);
        })
        .catch(() => {});
    }
    void (desktop ? desktopInvoke<DesktopSession | null>("desktop_bootstrap").then((session) => session ? desktopConnect(session) : null) : bootstrap())
      .then((value) => {
        if (cancelled) {
          value?.close();
          return;
        }
        setConnection(value);
        // A reload keeps the address. Once a hub is open again, neither the hub picker nor
        // a page of the hub this window left is a place to resume: go to this hub's home.
        const current = value ? parseRoute(router.hash()) : null;
        if (value && current && (current.name === "servers" || ("server" in current && current.server !== value.serverId)))
          router.replace(homeRoute(value, value.session.snapshot().accounts));
      })
      .catch((e) => {
        if (!cancelled) setError(desktop && typeof e === "string" ? e : readableError(e));
      })
      .finally(() => {
        if (!cancelled) setStarting(false);
      });
    return () => {
      cancelled = true;
    };
  }, []);
  useEffect(
    () => () => {
      connection?.close();
    },
    [connection],
  );
  useEffect(() => {
    document.body.classList.toggle(
      "onboarding-active",
      !connection || changingHub,
    );
  }, [connection, changingHub]);
  function connected(value: Connection) {
    setConnection(value);
    const current = parseRoute(router.hash());
    // Switching hubs on purpose leaves the old hub's address; a link for another hub keeps it.
    if (current?.name === "servers" || (connection && current && "server" in current && current.server !== value.serverId))
      router.replace(homeRoute(value, value.session.snapshot().accounts));
  }
  if (starting)
    return (
      <Welcome simulator={false}>
        <h1>{t("Opening AgentWechat…", "正在打开 AgentWechat…")}</h1>
      </Welcome>
    );
  if (!connection || changingHub)
    return desktop ? (
      <DesktopWelcome error={error} onConnect={connected} />
    ) : (
      <ConnectionForm
        error={error}
        onConnect={connected}
        initialEndpoint={connection?.endpoint}
        onBack={
          connection
            ? () => router.back(homeRoute(connection, connection.session.snapshot().accounts))
            : undefined
        }
      />
    );
  return (
    <Client
      key={connection.endpoint}
      connection={connection}
      theme={theme}
      setTheme={setTheme}
      onConnect={connected}
      disconnect={() => {
        router.push({ name: "servers" });
        if (desktop) setConnection(null);
      }}
    />
  );
}
function ConnectionForm({
  onConnect,
  error: initialError,
  onBack,
  initialEndpoint,
}: {
  onConnect: (c: Connection) => void;
  error: string;
  onBack?: () => void;
  initialEndpoint?: string;
}) {
  useLocale();
  const [endpoint, setEndpoint] = useState(initialEndpoint ?? location.origin),
    [token, setToken] = useState("");
  const [error, setError] = useState(initialError),
    [busy, setBusy] = useState(false),
    [show, setShow] = useState(false);
  const [hubs, setHubs] = useState<PreviewHub[]>([]);
  useEffect(() => {
    let cancelled = false;
    void previewHubs().then(value => { if (!cancelled) setHubs(value); }).catch(() => {});
    return () => { cancelled = true; };
  }, []);
  async function submit(event: FormEvent) {
    event.preventDefault();
    if (busy) return;
    setBusy(true);
    setError("");
    try {
      const remembered =
        import.meta.env.DEV && new URL(endpoint).origin === location.origin
          ? await bootstrap(token)
          : null;
      const connection = remembered ?? (await connect(endpoint, token));
      setToken("");
      onConnect(connection);
    } catch (e) {
      setError(readableError(e));
    } finally {
      setBusy(false);
    }
  }
  return (
    <Welcome simulator={false}>
      {onBack && (
        <button
          className="step-back"
          type="button"
          disabled={busy}
          onClick={onBack}
        >
          <Icon name="back" /> {t("Back", "返回")}
        </button>
      )}
      <div className="welcome-heading">
        <h1>
          {t("Connect to an agent-wechat hub", "连接到 agent-wechat 服务")}
        </h1>
        <p>
          {t(
            "Enter the address and access key for your agent-wechat hub.",
            "请输入你的 agent-wechat 服务地址和访问密钥。",
          )}
        </p>
      </div>
      {hubs.length > 1 && <div className="preview-hubs" aria-label={t("Preview hubs", "预览服务")}>
        {hubs.map(hub => <button key={hub.id} type="button" className="secondary" disabled={busy}
          onClick={() => {
            setBusy(true); setError("");
            void bootstrap(undefined, hub.id).then(value => {
              if (!value) throw Error(t("Reopen the private preview link to choose a hub.", "请重新打开预览链接以选择服务。"));
              onConnect(value);
            }).catch(e => setError(readableError(e))).finally(() => setBusy(false));
          }}>{hub.name}</button>)}
      </div>}
      <form onSubmit={submit}>
        <div className="remote-fields">
          <label htmlFor="hub-address">
            {t("agent-wechat hub address", "agent-wechat 服务地址")}
          </label>
          <div className="connection-input">
            <Icon name="server" />
            <input
              id="hub-address"
              type="url"
              required
              value={endpoint}
              onChange={(e) => setEndpoint(e.target.value)}
              autoComplete="off"
            />
          </div>
          <label htmlFor="hub-key">
            {t("agent-wechat hub access key", "agent-wechat 服务访问密钥")}
          </label>
          <div className="connection-input">
            <input
              id="hub-key"
              type={show ? "text" : "password"}
              required
              value={token}
              onChange={(e) => setToken(e.target.value)}
              autoComplete="off"
            />
            <button type="button" onClick={() => setShow(!show)}>
              {show ? t("Hide", "隐藏") : t("Show", "显示")}
            </button>
          </div>
        </div>
        {error && <ErrorNotice>{error}</ErrorNotice>}
        <button className="continue-button" disabled={busy}>
          {busy ? t("Connecting…", "正在连接…") : t("Connect", "连接")}{" "}
          <Icon name="chevron" />
        </button>
      </form>
      <details className="hub-help">
        <summary>
          {t(
            "Use this device as an agent-wechat hub",
            "将此设备作为 agent-wechat 服务使用",
          )}
        </summary>
        <p>
          {t(
            "The browser connects to a running agent-wechat hub. For local development, start ",
            "浏览器需要连接正在运行的 agent-wechat 服务。本地开发时，请在此设备上运行 ",
          )}
          <code>pnpm client:dev</code>
          {t(
            " on this device and open its printed URL.",
            " 并打开终端输出的链接。",
          )}
        </p>
      </details>
    </Welcome>
  );
}
function Client({
  connection,
  theme,
  setTheme,
  disconnect,
  onConnect,
}: {
  connection: Connection;
  theme: string;
  setTheme: (t: string) => void;
  disconnect: () => void;
  onConnect: (connection: Connection) => void;
}) {
  const { session, simulator, serverId } = connection;
  const state = useSyncExternalStore(session.subscribe, session.snapshot);
  // The address says what the user wants to see; account state decides what can
  // be shown. A link for another hub, or no usable link, opens this hub's home.
  const requested = useRoute();
  const address = useHash();
  const own = requested && requested.name !== "servers" && requested.server === serverId ? requested : null;
  const intent = own ?? homeRoute(connection, state.accounts);
  const hash = formatRoute(intent);
  const [error, setError] = useState("");
  const [notice, setNotice] = useState<{ text: string; hash: string } | null>(null);
  const [busy, setBusy] = useState(false);
  const [signingIn, setSigningIn] = useState<string | null>(null);
  const adding = useRef(false);
  const [managed, setManaged] = useState<string | null>(null);
  const known = useRef(new Set<string>());
  for (const item of state.accounts) known.current.add(item.id);
  const accountsRoute: Route = { name: "accounts", server: serverId };
  // Leaves an address that can't be shown for one that can, and says why.
  function fallBack(text: string | null, to: Route = accountsRoute) {
    if (text) setNotice({ text, hash: formatRoute(to) });
    router.replace(to);
  }
  const account =
    intent.name !== "accounts" ? state.accounts.find((item) => item.id === intent.account) : undefined;
  const screen: "accounts" | "login" | "chats" | "opening" =
    intent.name === "accounts"
      ? "accounts"
      : !account
      ? "opening"
      : !account.authenticated || signingIn === account.id
      ? "login"
      : "chats";
  useEffect(() => {
    if (own) return;
    const home = homeRoute(connection, state.accounts);
    fallBack(requested && requested.name !== "servers" ? "That link is for a different hub." : null, home);
  }, [own, address]);
  useEffect(() => {
    if (!desktop) return;
    return desktopNotifications(connection, (route) => router.push(route));
  }, [connection]);
  // The list can be older than the link: check the hub before calling an account unknown.
  const unknownAccount = intent.name !== "accounts" && !account ? intent.account : null;
  useEffect(() => {
    if (!unknownAccount) return;
    let cancelled = false;
    void session
      .accounts()
      .then((items) => {
        if (cancelled || items.some((item) => item.id === unknownAccount)) return;
        // An account removed while open is not news.
        fallBack(known.current.has(unknownAccount) ? null : "That account isn't on this hub.");
      })
      .catch((e) => {
        if (cancelled) return;
        setError(readableError(e));
        fallBack(null);
      });
    return () => {
      cancelled = true;
    };
  }, [unknownAccount]);
  // Keep showing sign-in for an account until it finishes, even once the hub says it is signed in.
  useEffect(() => {
    setSigningIn(screen === "login" && account ? account.id : null);
  }, [screen, account?.id]);
  // The sign-in address of an account that is signed in leads to its chats.
  useEffect(() => {
    if (intent.name === "login" && screen === "chats")
      router.replace({ name: "chats", server: serverId, account: intent.account });
  }, [hash, screen]);
  const wanted = screen === "chats" && account ? account.id : null;
  const activation = useRef<string | null>(null);
  useEffect(() => {
    if (!wanted) {
      activation.current = null;
      return;
    }
    if (state.account === wanted) {
      if (connection.cache.lastAccount() !== wanted) connection.cache.rememberAccount(wanted);
      return;
    }
    if (activation.current === wanted) return;
    activation.current = wanted;
    void session.activate(wanted, "thread" in intent ? intent.thread : undefined).catch((e) => {
      setError(readableError(e));
      fallBack(null);
    });
  }, [wanted, state.account]);
  const showing = screen === "chats" && !!state.account;
  useEffect(() => {
    document.body.classList.toggle("onboarding-active", !showing);
  }, [showing]);
  function open(item: Account) {
    setError("");
    router.push({ name: item.authenticated ? "chats" : "login", server: serverId, account: item.id });
  }
  async function add() {
    if (adding.current) return;
    adding.current = true;
    setBusy(true);
    setError("");
    try {
      const id = simulator
        ? `account-${crypto.randomUUID().slice(0, 8)}`
        : undefined;
      let number = 1;
      while (
        state.accounts.some(
          (item) =>
            item.alias === `account-${number}` ||
            item.id === `account-${number}`,
        )
      )
        number++;
      const result = await session.client.createAccount({
        id,
        alias: `account-${number}`,
      });
      await session.accounts();
      known.current.add((result as Account).id);
      router.push({ name: "login", server: serverId, account: (result as Account).id });
    } catch (e) {
      setError(readableError(e));
      router.push(accountsRoute);
    } finally {
      adding.current = false;
      setBusy(false);
    }
  }
  function switchAccount(replace = false) {
    connection.cache.rememberAccount(null);
    if (replace) router.replace(accountsRoute);
    else router.push(accountsRoute);
    void session.accounts();
  }
  if (screen === "opening" || (screen === "chats" && !state.account))
    return (
      <Welcome simulator={simulator}>
        <h1>{t("Opening AgentWechat…", "正在打开 AgentWechat…")}</h1>
      </Welcome>
    );
  if (screen === "login" && account)
    return (
      <Welcome simulator={simulator}>
        {state.signInRequired === account.id && (
          <ErrorNotice>WeChat signed out. Sign in again.</ErrorNotice>
        )}
        <Login
          connection={connection}
          account={account.id}
          cancel={() => {
            setSigningIn(null);
            router.up(accountsRoute);
            void session.accounts();
          }}
          signedIn={async () => {
            const accounts = await session.accounts();
            if (!accounts.some((item) => item.id === account.id && item.authenticated === true))
              throw new NativeApiError(409, "auth_required");
            setSigningIn(null);
          }}
        />
      </Welcome>
    );
  if (screen === "accounts") {
    const shown = notice?.hash === hash ? notice.text : "";
    return (
      <Welcome simulator={simulator}>
        <div className="welcome-heading">
          <h1>
            {state.accounts.length
              ? t("Choose an account", "选择账号")
              : t("Sign in to WeChat", "登录微信")}
          </h1>
          {!state.accounts.length && (
            <p>
              {simulator
                ? t(
                    "Create a synthetic account to try the connected client.",
                    "创建模拟账号以体验已连接的客户端。",
                  )
                : t(
                    "Add your WeChat account to get started.",
                    "添加你的微信账号以开始使用。",
                  )}
            </p>
          )}
        </div>
        {state.signInRequired && (
          <ErrorNotice>
            {t("WeChat signed out. Sign in again.", "微信已退出登录，请重新登录。")}
          </ErrorNotice>
        )}
        {shown && <ErrorNotice>{shown}</ErrorNotice>}
        <div className="login-accounts">
          {state.accounts.map((a) => (
            <div className="profile-row" key={a.id}>
              <button
                className="login-account"
                disabled={busy}
                onClick={() => void open(a)}
              >
                <HubAvatar
                  connection={connection}
                  account={a.id}
                  username={a.ownUsername}
                  name={a.alias || a.id}
                />
                <span>
                  <strong>{a.alias || a.id}</strong>
                  <small>
                    {a.authenticated
                      ? t("Open chats", "打开聊天")
                      : t("Sign in to WeChat", "登录微信")}
                  </small>
                </span>
                <Icon name="chevron" />
              </button>
              <button
                className="profile-manage"
                aria-label={t(
                  `Manage ${a.alias || a.id}`,
                  `管理 ${a.alias || a.id}`,
                )}
                title={t("Manage profile", "管理资料")}
                disabled={busy}
                onClick={() => setManaged(a.id)}
              >
                <Icon name="settings" />
              </button>
            </div>
          ))}
        </div>
        {managed && state.accounts.some((a) => a.id === managed) && (
          <ProfileDialog
            connection={connection}
            account={state.accounts.find((a) => a.id === managed)!}
            close={() => setManaged(null)}
            signedOut={() => {
              session.deactivate(managed);
              setManaged(null);
            }}
            removed={() => {
              session.deactivate(managed);
              void connection.clearCache(managed);
              setManaged(null);
            }}
            signedIn={() => {
              const a = state.accounts.find((a) => a.id === managed);
              setManaged(null);
              if (a) void open(a);
            }}
          />
        )}
        {error && <ErrorNotice>{error}</ErrorNotice>}
        <button
          className="continue-button"
          disabled={busy}
          onClick={() => void add()}
        >
          {busy
            ? t("Opening…", "正在打开…")
            : t("Add WeChat account", "添加微信账号")}{" "}
          <Icon name="plus" />
        </button>
        <button className="quiet-button" onClick={disconnect}>
          {t(
            "Connect to a different agent-wechat hub",
            "连接到其他 agent-wechat 服务",
          )}
        </button>
      </Welcome>
    );
  }
  return (
    <Chats
      key={state.account}
      connection={connection}
      route={isChatsRoute(intent) ? intent : { name: "chats", server: serverId, account: account!.id }}
      notice={notice?.hash === hash ? notice.text : ""}
      dismissNotice={() => setNotice(null)}
      fallBack={fallBack}
      theme={theme}
      setTheme={setTheme}
      switchAccount={switchAccount}
      addAccount={() => void add()}
      addingAccount={busy}
      onConnect={onConnect}
      disconnect={disconnect}
    />
  );
}
function Login({
  connection,
  account,
  signedIn,
  cancel,
}: {
  connection: Connection;
  account: string;
  signedIn: () => Promise<void>;
  cancel: () => void;
}) {
  const [operation, setOperation] = useState<NativeOperation | null>(null),
    [qr, setQr] = useState(""),
    [error, setError] = useState(""),
    [attempt, setAttempt] = useState(0),
    [clock, setClock] = useState<number | null>(null);
  const [busy, setBusy] = useState(false);
  const operationId = useRef<string | null>(null);
  useEffect(() => {
    const controller = new AbortController();
    let url = "";
    const login = connection.loginAttempt(account);
    void (async () => {
      try {
        setError("");
        setOperation(null);
        setQr("");
        const op = await login.open();
        if (controller.signal.aborted) {
          await connection.session.client.cancelOperation(account, op.id);
          return;
        }
        operationId.current = op.id;
        let generation = 0;
        while (!controller.signal.aborted) {
          const current = await connection.session.client.operation(
            account,
            op.id,
            controller.signal,
          );
          if (controller.signal.aborted) return;
          setOperation(current);
          if (connection.simulator) {
            const now = await simulatorClock().catch(() => null);
            if (controller.signal.aborted) return;
            setClock(now);
          }
          const result = current.result as {
            qrAvailable?: boolean;
            qrGeneration?: number;
          } | null;
          if (current.state === "succeeded") {
            login.complete(op.id);
            connection.clearLogout(account);
            operationId.current = null;
            await signedIn();
            return;
          }
          if (
            [
              "failed",
              "expired",
              "rejected",
              "cancelled",
              "needs_login",
            ].includes(current.state)
          ) {
            login.complete(op.id);
            operationId.current = null;
            return;
          }
          if (result?.qrAvailable && result.qrGeneration !== generation) {
            const bytes = await connection.session.client.qr(
              account,
              op.id,
              result.qrGeneration,
            );
            if (controller.signal.aborted) return;
            if (url) URL.revokeObjectURL(url);
            url = URL.createObjectURL(
              new Blob([new Uint8Array(bytes).buffer], { type: "image/png" }),
            );
            setQr(url);
            generation = result.qrGeneration ?? 1;
          }
          await pause(650, controller.signal);
        }
      } catch (e) {
        if (!controller.signal.aborted) setError(readableError(e));
      }
    })();
    return () => {
      controller.abort();
      if (url) URL.revokeObjectURL(url);
      const id = operationId.current;
      operationId.current = null;
      if (id)
        void connection.session.client
          .cancelOperation(account, id)
          .catch(() => {});
    };
  }, [account, attempt]);
  const state = operation?.state;
  const terminal = [
    "failed",
    "expired",
    "rejected",
    "cancelled",
    "needs_login",
  ].includes(state ?? "");
  const expiresAt = (operation?.result as { expiresAt?: number } | null)
    ?.expiresAt;
  const remaining =
    typeof expiresAt === "number"
      ? Math.max(0, expiresAt - (clock ?? Math.floor(Date.now() / 1000)))
      : null;
  async function simulate(status: string) {
    setBusy(true);
    try {
      await control({ op: "login", account, status });
      if (status === "expired") setQr("");
    } catch {
      setError(t("Simulator control failed.", "模拟器控制失败。"));
    } finally {
      setBusy(false);
    }
  }
  return (
    <>
      <button className="step-back" onClick={cancel}>
        <Icon name="back" /> {t("Back", "返回")}
      </button>
      <div className="welcome-heading">
        <h1>
          {state === "awaiting_approval"
            ? t("Confirm sign-in in WeChat", "在微信中确认登录")
            : state === "expired"
            ? t("QR code expired", "二维码已过期")
            : terminal
            ? t("Sign-in could not finish", "无法完成登录")
            : !qr
            ? t("Signing in…", "正在登录…")
            : t("Sign in to WeChat", "登录微信")}
        </h1>
        <p>
          {connection.simulator
            ? t(
                "Simulator QR only. It cannot sign in to WeChat. Use the phone controls below.",
                "仅模拟器二维码，无法用于登录真实微信。请使用下方的模拟手机控件。",
              )
            : state === "awaiting_approval"
            ? t(
                "Open WeChat on your phone and confirm sign-in.",
                "请在手机上打开微信并确认登录。",
              )
            : qr
            ? t(
                "In WeChat on your phone, tap +, then Scan.",
                "在手机微信中点击“+”，然后选择“扫一扫”。",
              )
            : terminal
            ? t("Try signing in again.", "请重新尝试登录。")
            : t("Trying your saved WeChat sign-in.", "正在尝试使用已保存的微信登录状态。")}
        </p>
      </div>
      <div className="account-link-card">
        {state === "awaiting_approval" ? (
          <div className="approval-symbol">
            <Icon name="check" />
          </div>
        ) : qr && !terminal ? (
          <img
            className="login-qr"
            src={qr}
            alt={
              connection.simulator
                ? t("Simulator sign-in QR code", "模拟器登录二维码")
                : t("WeChat sign-in QR code", "微信登录二维码")
            }
            data-qr-generation={
              (operation?.result as { qrGeneration?: number } | null)
                ?.qrGeneration
            }
            data-expires-at={expiresAt}
          />
        ) : (
          <p>
            {terminal
              ? t("Try again to continue.", "请重试以继续。")
              : t("Connecting…", "正在连接…")}
          </p>
        )}
      </div>
      {qr && !terminal && state !== "awaiting_approval" && (
        <p className="qr-expiry" role="status">
          {remaining === 0
            ? t("Refreshing code…", "正在刷新二维码…")
            : t("Code refresh in", "二维码刷新倒计时")}
          {remaining !== null && (!connection.simulator || clock !== null)
            ? remaining > 0
              ? ` ${Math.floor(remaining / 60)}:${String(
                  remaining % 60,
                ).padStart(2, "0")}`
              : ""
            : t(" a moment", " 即将刷新")}
        </p>
      )}
      {error && <ErrorNotice>{error}</ErrorNotice>}
      {terminal || error ? (
        <button
          className="continue-button"
          onClick={() => setAttempt((a) => a + 1)}
        >
          {t("Try again", "重试")}
        </button>
      ) : connection.simulator ? (
        <div className="sim-phone">
          <span>{t("Simulated phone", "模拟手机")}</span>
          <button
            className="continue-button"
            disabled={busy || !qr}
            onClick={() =>
              void simulate(
                state === "awaiting_approval" ? "approved" : "scanned",
              )
            }
          >
            {state === "awaiting_approval"
              ? t("Simulate phone approval", "模拟手机确认")
              : t("Simulate scan", "模拟扫码")}{" "}
            <Icon name="chevron" />
          </button>
          {state !== "awaiting_approval" && (
            <details className="sim-login-tools">
              <summary>
                {t("Simulator test controls", "模拟器测试控件")}
              </summary>
              <button
                className="quiet-button"
                disabled={busy || !qr}
                onClick={() => void simulate("expired")}
              >
                {t("Simulate code expiry", "模拟二维码过期")}
              </button>
            </details>
          )}
        </div>
      ) : (
        <p className="field-help">
          {t("Chats open automatically after sign-in.", "登录后将自动打开会话列表。")}
        </p>
      )}
    </>
  );
}
function Chats({
  connection,
  route,
  notice: shownNotice,
  dismissNotice,
  fallBack,
  theme,
  setTheme,
  switchAccount,
  addAccount,
  addingAccount,
  onConnect,
  disconnect,
}: {
  connection: Connection;
  route: ChatsRoute;
  /** Why the address was changed, shown while it is still the address. */
  notice: string;
  dismissNotice: () => void;
  fallBack: (text: string | null, to: Route) => void;
  theme: string;
  setTheme: (t: string) => void;
  switchAccount: (replace?: boolean) => void;
  addAccount: () => void;
  addingAccount: boolean;
  onConnect: (connection: Connection) => void;
  disconnect: () => void;
}) {
  const { session, simulator } = connection,
    state = useSyncExternalStore(session.subscribe, session.snapshot);
  const [locale] = useLocale();
  const savedView = state.account ? connection.cache.view(state.account) : {};
  const [query, setQuery] = useState(""),
    [filter, setFilter] = useState(
      ["all", "unread", "groups"].includes(savedView.filter ?? "")
        ? savedView.filter!
        : "all",
    );
  const [accountMenu, setAccountMenu] = useState(false);
  // What is on screen follows the address. A dialog (settings, new chat) shows
  // over the view that was open before it.
  const here = { server: connection.serverId, account: state.account! };
  const listRoute: Route = { name: "chats", ...here };
  const chatRoute = (thread: string): Route => ({ name: "chat", ...here, thread });
  const lastView = useRef<ViewRoute>({ name: "chats", ...here });
  if (isViewRoute(route)) lastView.current = route;
  const view = isViewRoute(route) ? route : lastView.current;
  const section = view.name === "contacts" ? "contacts" : "chats";
  const routeThread = "thread" in view ? view.thread : null;
  const mobile = routeThread !== null;
  const details = view.name === "info";
  const settings = route.name === "settings";
  const newChat = route.name === "new-chat";
  const synced = route.account === state.account;
  // Back, Forward and links change the conversation; clicking one opens it
  // directly. Wait for the account to finish opening so a link does not race
  // the conversation it picks itself.
  useEffect(() => {
    if (!routeThread || !synced || state.selected === routeThread || state.loading || state.opening) return;
    const account = state.account!;
    let cancelled = false;
    void conversationAvailable(session.client, account, routeThread, state.threads).then((available) => {
      if (cancelled) return;
      if (available) void session.select(routeThread);
      else fallBack("That chat isn't available.", listRoute);
    });
    return () => {
      cancelled = true;
    };
  }, [routeThread, synced, state.account, state.selected, state.loading, state.opening]);
  useEffect(() => {
    if (!desktop || !connection.desktopId) return;
    const narrow = window.matchMedia("(max-width: 720px)");
    const report = () => { void desktopInvoke("notifications_context", {
      focus: { connectionId: connection.desktopId, accountId: state.account, threadId: section === "chats" && (!narrow.matches || mobile) && !settings && !newChat && !accountMenu ? state.selected : null },
    }); };
    report();
    narrow.addEventListener("change", report);
    return () => { narrow.removeEventListener("change", report); void desktopInvoke("notifications_context", { focus: null }); };
  }, [connection, state.account, state.selected, section, mobile, settings, newChat, accountMenu]);
  useEffect(() => {
    if (state.account)
      connection.cache.saveView(state.account, { section, filter });
  }, [state.account, section, filter]);
  const [drafts, setDrafts] = useState<Record<string, string>>({});
  const draftsNow = useRef(drafts);
  draftsNow.current = drafts;
  const [mentionDrafts, setMentionDrafts] = useState<Record<string, MentionToken[]>>({});
  const [mentionProblems, setMentionProblems] = useState<Record<string, boolean>>({});
  const [mentionOpen, setMentionOpen] = useState(0);
  const [replyDrafts, setReplyDrafts] = useState<Record<string, NativeMessage>>(
    {},
  );
  const [draftReady, setDraftReady] = useState("");
  const [quoteRestoring, setQuoteRestoring] = useState(false);
  const pendingQuote = useRef<
    { messageId: string; serverId: string } | undefined
  >(undefined);

  const messageInput = useRef<HTMLTextAreaElement>(null);
  const composerInput = useRef<HTMLDivElement>(null);
  const mentionInput = useRef<HTMLDivElement>(null);
  function focusMessage() { (mentionInput.current ?? messageInput.current)?.focus({ preventScroll: true }); }
  const composing = useRef(false);
  const [simBody, setSimBody] = useState("Hello from the simulated phone."),
    [simStatus, setSimStatus] = useState("");
  const [find, setFind] = useState(false),
    [inChat, setInChat] = useState("");
  useEffect(() => {
    setInChat("");
    setFind(false);
  }, [state.account, state.selected]);
  const account = state.accounts.find((a) => a.id === state.account);
  const name = (id: string) =>
    state.contacts.find((c) => c.id === id)?.displayName ||
    state.threads.find((t) => t.id === id)?.displayName ||
    (id === "filehelper" ? t("File Transfer", "文件传输助手") : id);
  const selectedName = state.selected ? name(state.selected) : "";
  const draftKey = `${state.account}:${state.selected}`,
    draft = drafts[draftKey] ?? "";
  useSyncExternalStore(connection.attachmentDrafts.subscribe, connection.attachmentDrafts.snapshot);
  const attachments = connection.attachmentDrafts.get(state.account ?? "", state.selected ?? "");
  const [attachmentSendError, setAttachmentSendError] = useState<{ target: string; message: string } | null>(null);
  const draftMentions = mentionDrafts[draftKey] ?? EMPTY_MENTIONS;
  const allowAllMention = canMentionAll(state.selectedGroup, state.selected);
  const allMentionBlocked = draftMentions.some(token => token.userId === null) && !allowAllMention;
  const inlineCapable = state.capabilities.inlineTextMentions === true && !!state.selected?.endsWith("@chatroom");
  const inlineEditor = inlineCapable || draftMentions.length > 0;
  function changeDraft(text: string, tokens: MentionToken[] = []) {
    draftsNow.current = { ...draftsNow.current, [draftKey]: text };
    setDrafts(values => ({ ...values, [draftKey]: text }));
    setMentionDrafts(values => ({ ...values, [draftKey]: tokens }));
  }
  useEffect(() => {
    if (!state.account || !state.selected) return;
    const account = state.account,
      thread = state.selected;
    const saved = connection.cache.readState<{
      text: string;
      quote?: { messageId: string; serverId: string };
      mentionTokens?: MentionToken[];
    }>(account, thread, "drafts");
    let cancelled = false;
    setDraftReady("");
    const validMentions = saved?.mentionTokens === undefined || (typeof saved.text === "string" && validMentionTokens(saved.text, saved.mentionTokens));
    setMentionDrafts(values => ({ ...values, [draftKey]: validMentions ? saved ? saved.mentionTokens ?? [] : values[draftKey] ?? [] : [] }));
    setMentionProblems(values => ({ ...values, [draftKey]: !validMentions }));
    setReplyDrafts((values) => {
      const next = { ...values };
      delete next[draftKey];
      return next;
    });
    pendingQuote.current = saved?.quote;
    setQuoteRestoring(!!saved?.quote);
    setDrafts((values) => ({
      ...values,
      [draftKey]:
        typeof saved?.text === "string" ? saved.text : values[draftKey] ?? "",
    }));
    setDraftReady(draftKey);
    if (saved?.quote) {
      const quote = saved.quote;
      let resolved = false;
      const existing = state.messages.find(
        (message) => message.id === quote.messageId,
      );
      void (
        existing
          ? Promise.resolve(existing)
          : session.client.message(account, quote.messageId)
      )
        .then((message) => {
          if (
            !cancelled &&
            pendingQuote.current === quote &&
            message.accountId === account &&
            message.threadId === thread &&
            message.serverId === quote.serverId &&
            canReply(message)
          ) {
            resolved = true;
            setReplyDrafts((values) => ({ ...values, [draftKey]: message }));
          }
        })
        .catch(() => {})
        .finally(() => {
          if (!cancelled && pendingQuote.current === quote) {
            if (resolved) pendingQuote.current = undefined;
            setQuoteRestoring(false);
          }
        });
    }
    return () => {
      cancelled = true;
    };
  }, [state.account, state.selected, connection]);
  const savedReply = replyDrafts[draftKey];
  const reply =
    savedReply &&
    (state.messages.find((message) => message.id === savedReply.id) ??
      savedReply);
  const replyInvalid =
    !!reply &&
    (!canReply(reply) ||
      reply.serverId !== savedReply?.serverId ||
      state.capabilities.quotedTextSend !== true);
  useEffect(() => {
    if (draftReady !== draftKey || !state.account || !state.selected) return;
    if (mentionProblems[draftKey]) return;
    const quote = savedReply?.serverId
      ? { messageId: savedReply.id, serverId: savedReply.serverId }
      : pendingQuote.current;
    connection.cache.writeState(
      state.account,
      state.selected,
      draft || quote ? { text: draft, ...(quote && { quote }), ...(draftMentions.length > 0 && { mentionTokens: draftMentions }) } : null,
      "drafts",
    );
  }, [
    connection,
    draftReady,
    draftKey,
    draft,
    savedReply,
    quoteRestoring,
    state.account,
    state.selected,
    draftMentions,
    mentionProblems,
  ]);
  const leftGroup = state.selectedGroup?.isInGroup === false;
  const active = account?.authenticated !== false && !leftGroup;
  async function simulate(op: Record<string, unknown>) {
    try {
      await control({ account: state.account, ...op });
      setSimStatus(t("Scenario applied.", "已应用模拟场景。"));
    } catch {
      setSimStatus(t("Scenario control failed.", "模拟场景控制失败。"));
    }
  }
  function send() {
    const text = draftsNow.current[draftKey] ?? draft;
    if (
      (!text.trim() && !attachments.files.length) ||
      (!text.trim() && !!reply) ||
      !!attachments.submission ||
      !state.account ||
      !state.selected ||
      !active ||
      replyInvalid ||
      pendingQuote.current
      || mentionProblems[draftKey]
      || allMentionBlocked
    )
      return;
    const batch = attachments.files.length ? connection.attachmentDrafts.claim(state.account, state.selected) : null;
    if (attachments.files.length && !batch) return;
    const outbox = batch ? connection.mediaOutbox(batch.account) : null;
    setAttachmentSendError(null);
    if (text.trim()) {
      connection.cache.writeState(
        state.account,
        state.selected,
        null,
        "drafts",
      );
      changeDraft("");
      clearReply();
    }
    void (text.trim() ? session.send(text, reply, draftMentions) : Promise.resolve(true)).then(async (queued) => {
      const current = batch ? connection.attachmentDrafts.finish(batch, queued) : true;
      if (queued) {
        if (current && batch && outbox) {
          void session.readOnInteraction(batch.account, batch.thread);
          await sendAttachmentBatch(outbox, batch);
        }
        return;
      }
      if (!current) return;
      // Keep new input, including its recipient identities, if typing resumed.
      if (!draftsNow.current[draftKey]) {
        // Restore to the original conversation even if navigation changed.
        draftsNow.current = { ...draftsNow.current, [draftKey]: text };
        setDrafts(values => ({ ...values, [draftKey]: text }));
        setMentionDrafts(values => ({ ...values, [draftKey]: draftMentions }));
        if (reply)
          setReplyDrafts((replies) =>
            replies[draftKey] ? replies : { ...replies, [draftKey]: reply },
          );
      }
    }).catch(error => {
      if (batch) connection.attachmentDrafts.finish(batch, false);
      setAttachmentSendError({ target: draftKey, message: readableError(error) });
    });
  }
  function acknowledgeComposer() {
    if (state.account && state.selected)
      void session.readOnInteraction(state.account, state.selected);
  }
  function clearReply() {
    pendingQuote.current = undefined;
    setQuoteRestoring(false);
    setReplyDrafts((replies) => {
      const updated = { ...replies };
      delete updated[draftKey];
      return updated;
    });
    focusMessage();
  }
  function select(id: string, mode: "push" | "replace" = "push") {
    // Commit the selected composer within the click gesture, including on mobile.
    // History continues loading without delaying focus or stealing it later.
    flushSync(() => {
      setInChat("");
      void session.select(id);
      // The details panel stays open while switching conversations, as it did before routes.
      router[mode](view.name === "info" ? { name: "info", ...here, thread: id } : chatRoute(id));
    });
    focusMessage();
  }
  const threads = orderThreads(useThreadPinPreview(connection, state.account!, state.threads)).filter(
    (t) =>
      name(t.id).toLowerCase().includes(query.toLowerCase()) &&
      (filter !== "unread" || !!t.unreadCount) &&
      (filter !== "groups" || t.kind === "group"),
  );
  const firstPinned = threads.find((thread) => thread.pinned === true)?.id;
  return (
    <div className={`shell ${mobile ? "mobile-chat" : ""}`}>
      <nav className="rail" aria-label={t("Main navigation", "主导航")}>
        <div className="brand" title="AgentWechat">
          <Logo />
        </div>
        <button
          className={`nav-btn ${section === "chats" ? "active" : ""}`}
          aria-label={t("Chats", "聊天")}
          onClick={() => router.push(listRoute)}
        >
          <Icon name="chat" />
        </button>
        <button
          className={`nav-btn ${section === "contacts" ? "active" : ""}`}
          aria-label={t("Contacts", "通讯录")}
          onClick={() => router.push({ name: "contacts", ...here })}
        >
          <Icon name="people" />
        </button>
        <div className="rail-spacer" />
        <button
          className="nav-btn"
          aria-label={t("Settings", "设置")}
          onClick={() => router.push({ name: "settings", ...here })}
        >
          <Icon name="settings" />
        </button>
        <div className="rail-rule" />
        <button
          className="nav-btn account-btn"
          aria-label={t("Switch account", "切换账号")}
          onClick={() => setAccountMenu(!accountMenu)}
        >
          <HubAvatar
            connection={connection}
            account={state.account!}
            username={account?.ownUsername}
            name={account?.alias || state.account || "?"}
          />
        </button>
      </nav>
      <aside className="sidebar">
        <div className="side-head">
          <div className="eyebrow">{account?.alias || "AgentWechat"}</div>
          <div className="title-row">
            <h1>
              {section === "chats"
                ? t("Messages", "消息")
                : t("Contacts", "通讯录")}
            </h1>
            <Button
              icon="plus"
              label={t("New conversation", "新建聊天")}
              onClick={() => router.push({ name: "new-chat", ...here })}
            />
          </div>
          <label className="search">
            <Icon name="search" />
            <input
              aria-label={
                section === "chats"
                  ? t("Search conversations", "搜索会话")
                  : t("Search contacts", "搜索联系人")
              }
              placeholder={
                section === "chats"
                  ? t("Search conversations", "搜索会话")
                  : t("Find a person", "查找联系人")
              }
              value={query}
              onChange={(e) => setQuery(e.target.value)}
            />
          </label>
          {section === "chats" && (
            <div className="filters">
              {(["all", "unread", "groups"] as const).map((f) => (
                <button
                  key={f}
                  className={`pill ${filter === f ? "active" : ""}`}
                  onClick={() => setFilter(f)}
                >
                  {f === "all"
                    ? t("All", "全部")
                    : f === "unread"
                    ? t("Unread", "未读")
                    : t("Groups", "群聊")}
                </button>
              ))}
            </div>
          )}
        </div>
        {shownNotice && !routeThread && (
          <div className="banner route-notice" role="status">
            {shownNotice}
            <button onClick={dismissNotice}>Dismiss</button>
          </div>
        )}
        <div className="chat-list">
          {section === "chats"
            ? threads.map((t, index) => (
                <ConversationRow
                  key={t.id}
                  connection={connection}
                  account={state.account!}
                  thread={t}
                  name={name(t.id)}
                  selected={state.selected === t.id}
                  select={() => select(t.id)}
                  heading={t.id === firstPinned}
                  divider={index > 0 && threads[index - 1].pinned === true && t.pinned !== true}
                />
              ))
            : state.contacts
                .filter((c) =>
                  (c.displayName || c.id)
                    .toLowerCase()
                    .includes(query.toLowerCase()),
                )
                .map((c) => (
                  <button
                    key={c.id}
                    className="chat-row"
                    onClick={() => select(c.id)}
                  >
                    <HubAvatar
                      connection={connection}
                      account={state.account!}
                      username={c.id}
                      name={c.displayName || c.id}
                    />
                    <div className="chat-copy">
                      <strong>{c.displayName || c.id}</strong>
                      <p className="preview">{c.username}</p>
                    </div>
                  </button>
                ))}
          {!state.loading &&
            (section === "chats" ? threads.length : state.contacts.length) ===
              0 && (
              <p className="no-results">
                {query
                  ? t("No matches.", "无匹配结果。")
                  : t("No conversations yet.", "暂无会话。")}
              </p>
            )}
        </div>
        <div className="side-footer">
          <span>
            <i className={`status-dot ${!state.connected ? "off" : ""}`} />
            {state.connected
              ? t("agent-wechat hub connected", "已连接 agent-wechat 服务")
              : state.reconnecting
              ? t("Reconnecting live updates…", "正在重新连接实时更新…")
              : t(
                  "Connecting to agent-wechat hub…",
                  "正在连接 agent-wechat 服务…",
                )}
          </span>
          {simulator && (
            <span className="sim-label">{t("SIMULATOR", "模拟器")}</span>
          )}
        </div>
      </aside>
      <main className="workspace">
        <header className="chat-header">
          <Button
            icon="back"
            label={t("Back to conversations", "返回会话列表")}
            className="back-btn"
            onClick={() => router.up(listRoute)}
          />
          {state.selected && state.account && (
            <ConversationAvatar
              connection={connection}
              account={state.account}
              username={state.selected}
              name={selectedName}
              members={
                state.threads.find((thread) => thread.id === state.selected)
                  ?.avatarMembers
              }
            />
          )}
          <div className="chat-heading">
            <h2>{selectedName || "AgentWechat"}</h2>
            <p>
              {state.selected === "filehelper"
                ? t("Notes to yourself", "发送给自己")
                : state.selected
                ? state.selected.endsWith("@chatroom")
                  ? t("Group conversation", "群聊")
                  : t("WeChat conversation", "微信会话")
                : t("Choose a conversation", "选择一个会话")}
            </p>
          </div>
          <div className="header-actions">
            <Button
              icon="search"
              label={t("Find in conversation", "在聊天中查找")}
              onClick={() => {
                setFind(!find);
                setInChat("");
              }}
            />
            <Button
              icon="panel"
              label={t("Conversation details", "聊天详情")}
              onClick={() =>
                details
                  ? router.up(chatRoute(view.name === "info" ? view.thread : state.selected!))
                  : state.selected && router.push({ name: "info", ...here, thread: state.selected })
              }
            />
          </div>
          {find && (
            <label className="search conversation-search">
              <input
                aria-label={t("Find in this chat", "在此聊天中查找")}
                placeholder={t("Find in this chat…", "在此聊天中查找…")}
                autoFocus
                maxLength={64}
                value={inChat}
                onChange={(e) => setInChat(e.target.value)}
                onKeyDown={(e) => {
                  if (e.key === "Escape") {
                    setInChat("");
                    setFind(false);
                  }
                }}
              />
            </label>
          )}
        </header>
        {state.error && (
          <div className="banner" role="alert">
            {state.error}
            <button
              onClick={() => {
                session.clearError();
                void session.refresh().catch(() => {});
              }}
            >
              {t("Refresh", "刷新")}
            </button>
          </div>
        )}
        {shownNotice && routeThread && (
          <div className="banner route-notice" role="status">
            {shownNotice}
            <button onClick={dismissNotice}>Dismiss</button>
          </div>
        )}
        {leftGroup && (
          <div className="banner">
            {t(
              "You are no longer in this group. Cached history is still available.",
              "你已不在该群聊中，仍可查看本地缓存的历史记录。",
            )}
          </div>
        )}
        {account?.authenticated === false && (
          <div className="banner">
            {t(
              "Sign in to WeChat again to send messages.",
              "请重新登录微信以发送消息。",
            )}
            <button onClick={() => switchAccount}>{t("Sign in", "登录")}</button>
          </div>
        )}
        {simulator && (
          <details className="simulator-tools">
            <summary>{t("Simulator controls", "模拟器控件")}</summary>
            <div className="sim-controls">
              <input
                aria-label={t("Synthetic incoming text", "模拟收到的消息文本")}
                value={simBody}
                onChange={(e) => setSimBody(e.target.value)}
              />
              <button
                onClick={() =>
                  void simulate({
                    op: "inbound",
                    peer: state.selected || "sim_mei",
                    body: simBody,
                  })
                }
              >
                {t("Receive message", "接收消息")}
              </button>
              <button
                onClick={() =>
                  void simulate({
                    op: "inbound",
                    peer: state.selected || "sim_mei",
                    body: simBody,
                    self_sent: true,
                  })
                }
              >
                {t("Send from phone", "从手机发送")}
              </button>
              <button
                onClick={() =>
                  void simulate({
                    op: "advertise_image",
                    peer: state.selected || "sim_mei",
                  })
                }
              >
                {t("Unavailable image", "不可用图片")}
              </button>
              {["image", "file", "video", "voice", "sticker"].map((kind) => (
                <button
                  key={kind}
                  disabled={
                    kind !== "sticker" &&
                    (!Array.isArray(state.capabilities.mediaKinds) ||
                      !state.capabilities.mediaKinds.includes(kind))
                  }
                  onClick={() =>
                    void simulate({
                      op: "receive_media",
                      peer: state.selected || "sim_mei",
                      kind,
                    })
                  }
                >
                  {t(`Receive ${kind}`, `接收 ${kind}`)}
                </button>
              ))}
              <button
                onClick={() =>
                  void simulate({ op: "send_fault", fault: "reject" })
                }
              >
                {t("Reject next send", "拒绝下一次发送")}
              </button>
              <button
                onClick={() =>
                  void simulate({
                    op: "send_fault",
                    fault: "lost_acknowledgment",
                  })
                }
              >
                {t("Lose next acknowledgment", "丢失下一次确认")}
              </button>
            </div>
            <p role="status">
              {simStatus ||
                t(
                  "Synthetic upstream only. No official WeChat connection.",
                  "仅模拟上游，未连接官方微信。",
                )}
            </p>
          </details>
        )}
        <ConversationNavigation
          key={`${state.account}:${state.selected}`}
          session={session}
          closeSearch={() => setInChat("")}
        >
          <ReplyContext.Provider
            value={{
              enabled: active && state.capabilities.quotedTextSend === true,
              reply(message) {
                if (
                  message.accountId !== state.account ||
                  message.threadId !== state.selected ||
                  !canReply(message)
                )
                  return;
                pendingQuote.current = undefined;
                setQuoteRestoring(false);
                setReplyDrafts((replies) => ({
                  ...replies,
                  [draftKey]: message,
                }));
                focusMessage();
              },
            }}
          >
            <PhotoViewer
              connection={connection}
              route={{
                id: route.name === "media" ? route.message : null,
                open: (message) =>
                  state.selected &&
                  router.push({ name: "media", ...here, thread: state.selected, message }),
                move: (message) =>
                  state.selected &&
                  router.replace({ name: "media", ...here, thread: state.selected, message }),
                close: () => state.selected && router.up(chatRoute(state.selected)),
                showMessage: (message) =>
                  state.selected &&
                  router.replace({ name: "message", ...here, thread: state.selected, message }),
                missing: () =>
                  state.selected &&
                  fallBack("That photo or video isn't available.", chatRoute(state.selected)),
              }}
            >
              {view.name === "message" && state.account && state.selected === view.thread && (
                <RouteMessage
                  session={session}
                  account={state.account}
                  thread={view.thread}
                  message={view.message}
                  missing={() => fallBack("That message isn't in this chat.", chatRoute(view.thread))}
                />
              )}
              <div className="body-row">
                <section
                  className="conversation"
                  aria-label={t("Conversation", "会话")}
                >
                  <ConversationScroll
                    key={`${state.account}:${state.selected}:${!!(
                      find && inChat.trim()
                    )}`}
                    searching={!!(find && inChat.trim())}
                    position={
                      state.account && state.selected && view.name !== "message"
                        ? {
                            cache: connection.cache,
                            account: state.account,
                            thread: state.selected,
                          }
                        : undefined
                    }
                    history={{
                      hasMore: !!state.nextMessages,
                      loading: state.loading,
                      load: () => session.older(),
                    }}
                    leading={
                      state.messages[0] ? (
                        <MessageDate value={state.messages[0].createdAt} />
                      ) : null
                    }
                  >
                    {find &&
                    inChat.trim() &&
                    state.account &&
                    state.selected ? (
                      <SearchResults
                        key={`${state.account}:${state.selected}`}
                        connection={connection}
                        account={state.account}
                        thread={state.selected}
                        query={inChat}
                        name={name}
                        close={() => setInChat("")}
                      />
                    ) : (
                      <>
                        {state.messages.map((m, index) => (
                          <ConversationItem key={m.id} messageId={m.id}>
                            <Message
                              message={m}
                              connection={connection}
                              name={name}
                            />
                            {state.messages[index + 1] &&
                              date(m.createdAt).toDateString() !==
                                date(
                                  state.messages[index + 1].createdAt,
                                ).toDateString() && (
                                <MessageDate
                                  value={state.messages[index + 1].createdAt}
                                />
                              )}
                          </ConversationItem>
                        ))}
                        {state.sends
                          .filter(
                            (s) =>
                              s.threadId === state.selected && !s.dismissed,
                          )
                          .map((s) => (
                            <ConversationItem
                              key={s.key}
                              messageId={`pending:${s.key}`}
                            >
                              <Pending
                                send={s}
                                check={() => session.checkSend(s.key)}
                                cancel={() => session.cancelSend(s.key)}
                                dismiss={() => session.dismissSend(s.key)}
                              />
                            </ConversationItem>
                          ))}
                        <ConversationItem messageId="pending-media">
                          {state.account && (
                            <PendingAttachments
                              key={state.account}
                              connection={connection}
                              account={state.account}
                              threadId={state.selected}
                              messages={state.messages}
                            />
                          )}
                          {!state.loading &&
                            state.selected &&
                            state.messages.length === 0 && (
                              <p className="empty-chat">
                                {t("Start the conversation.", "开始聊天吧。")}
                              </p>
                            )}
                        </ConversationItem>
                      </>
                    )}
                  </ConversationScroll>
                  <div className="composer-wrap">
                    <div className="composer" ref={composerInput}>
                      {!reply && pendingQuote.current && (
                        <div className="composer-reply">
                          <span className="reply-source">
                            {quoteRestoring
                              ? t("Loading reply…", "正在加载回复…")
                              : t(
                                  "Quoted message unavailable",
                                  "引用的消息不可用",
                                )}
                          </span>
                          <Button
                            icon="close"
                            label={t("Remove reply", "移除回复")}
                            onClick={clearReply}
                          />
                        </div>
                      )}
                      {reply && (
                        <ReplyPreview
                          message={reply}
                          name={name}
                          invalid={replyInvalid}
                          cancel={clearReply}
                        />
                      )}
                      <AttachmentTray draft={attachments}
                        remove={id => { connection.attachmentDrafts.remove(state.account!, state.selected!, id); focusMessage(); }}
                        quality={original => connection.attachmentDrafts.quality(state.account!, state.selected!, original ? "original" : "normal")} />
                      {attachmentSendError?.target === draftKey && <p className="attachment-error" role="alert">{attachmentSendError.message}</p>}
                      {attachments.files.length > 0 && reply && !draft.trim() && <p className="field-help">{t("Write text for the quoted reply. Files send as separate messages.", "请为引用回复输入文字。文件将作为独立消息发送。")}</p>}
                      {inlineEditor && state.account && state.selected ? <MentionEditor
                        key={draftKey} connection={connection} account={state.account} thread={state.selected} inputRef={mentionInput}
                        value={draft} tokens={draftMentions} disabled={!active} allowMentions={inlineCapable && active}
                        canMentionAll={allowAllMention}
                        openRequest={mentionOpen} placeholder={t(`Message ${selectedName}…`, `发送给 ${selectedName}…`)} onChange={changeDraft} onSend={send} onFocus={acknowledgeComposer} onEscape={clearReply}
                      /> : <textarea
                        ref={messageInput}
                        aria-label={t("Message", "消息")}
                        onFocus={acknowledgeComposer}
                        onClick={acknowledgeComposer}
                        onCompositionStart={() => {
                          composing.current = true;
                        }}
                        onCompositionEnd={() => {
                          composing.current = false;
                        }}
                        onBlur={() => {
                          composing.current = false;
                        }}
                        rows={2}
                        placeholder={
                          state.selected
                            ? t(
                                `Message ${selectedName}…`,
                                `发送给 ${selectedName}…`,
                              )
                            : t("Choose a conversation", "选择一个会话")
                        }
                        value={draft}
                        disabled={!state.selected || !active}
                        onChange={(e) => changeDraft(e.target.value)}
                        onKeyDown={(e) => {
                          // WebKit can end composition before its confirming Enter.
                          // Leave IME keys untouched so candidate selection keeps working.
                          if (
                            e.defaultPrevented ||
                            composing.current ||
                            e.nativeEvent.isComposing ||
                            e.nativeEvent.keyCode === 229
                          ) {
                            return;
                          }
                          if (e.key === "Escape" && reply) {
                            e.preventDefault();
                            clearReply();
                            return;
                          }
                          if (e.key === "Enter" && !e.shiftKey) {
                            e.preventDefault();
                            if (!e.repeat) send();
                          }
                        }}
                      />}
                      {mentionProblems[draftKey] && <p className="mention-error" role="alert">{t("Saved mention recipients could not be restored. Choose recipients again, or ", "无法恢复已保存的 @ 提醒对象。请重新选择提醒对象，或")}<button onClick={() => { setMentionProblems(values => ({ ...values, [draftKey]: false })); changeDraft(draft); }}>{t("keep only the text", "仅保留纯文本")}</button>{t(".", "。")}</p>}
                      {allMentionBlocked && <p className="mention-error" role="alert">{allMentionProblem(state.selectedGroup, state.selected)} <button type="button" onClick={() => { const next = removeAllMentions(draft, draftMentions); changeDraft(next.text, next.tokens); }}>{t("Remove Everyone", "移除所有人")}</button> <button type="button" disabled={!active || state.opening} onClick={() => { void session.refreshConversations(state.account, false, state.selected ?? undefined).catch(() => {}); }}>{t("Refresh", "刷新")}</button></p>}
                      <div className="composer-bottom">
                        <div className="composer-tools" role="group" aria-label={t("Message actions", "消息操作")}>
                          {state.account && <AttachmentPicker key={JSON.stringify([state.account, state.selected])} connection={connection} account={state.account} threadId={state.selected}
                            composerRef={composerInput} mediaKinds={Array.isArray(state.capabilities.mediaKinds) ? state.capabilities.mediaKinds : []}
                            capabilities={state.capabilities}
                            focusMessage={focusMessage}
                            enabled={active && !attachments.submission && Array.isArray(state.capabilities.mediaKinds) && state.capabilities.mediaKinds.length > 0} />}
                          <Button icon="mention" label={t("Mention someone", "提醒某人")}
                            title={!state.selected?.endsWith("@chatroom") ? t("Mentions are available in group chats", "仅群聊支持 @ 提醒") : !inlineCapable ? t("Update the hub to use inline mentions", "请更新服务以使用群内 @ 提醒") : t("Mention someone", "提醒某人")}
                            disabled={!inlineCapable || !active} onClick={() => setMentionOpen(value => value + 1)} />
                        </div>
                        {simulator && <span className="composer-hint">{t("Messages stay in the simulator.", "消息仅保留在模拟器中。")}</span>}
                        <ComposerHelp label={t("Keyboard shortcuts", "键盘快捷键")}><span><kbd>Enter</kbd> {t("to send", "发送")}</span><span><kbd>Shift</kbd> + <kbd>Enter</kbd> {t("for a new line", "换行")}</span></ComposerHelp>
                        <button
                          className="send-btn"
                          disabled={
                            !state.selected ||
                            (!draft.trim() && !attachments.files.length) ||
                            (!draft.trim() && !!reply) ||
                            !!attachments.submission ||
                            !active ||
                            !!pendingQuote.current ||
                            replyInvalid || mentionProblems[draftKey] || allMentionBlocked
                          }
                          onClick={send}
                        >
                          {t("Send", "发送")} <Icon name="send" />
                        </button>
                      </div>
                    </div>
                  </div>
                </section>
                {details && state.selected && (
                  <Details
                    key={`${state.account}:${state.selected}`}
                    connection={connection}
                    threadId={state.selected}
                    name={selectedName}
                  />
                )}
              </div>
            </PhotoViewer>
          </ReplyContext.Provider>
        </ConversationNavigation>
      </main>
      {accountMenu && (
        <div
          className="account-menu"
          role="dialog"
          aria-label={t("Switch account", "切换账号")}
        >
          <div className="eyebrow">{t("WeChat accounts", "微信账号")}</div>
          {state.accounts.map((a) => (
            <button
              key={a.id}
              className={`account-option ${
                a.id === state.account ? "selected" : ""
              }`}
              onClick={() => {
                setAccountMenu(false);
                if (a.authenticated) router.push({ name: "chats", server: here.server, account: a.id });
                else switchAccount();
              }}
            >
              <HubAvatar
                connection={connection}
                account={a.id}
                username={a.ownUsername}
                name={a.alias || a.id}
              />
              <span>
                <strong>{a.alias || a.id}</strong>
                <small>
                  {a.authenticated
                    ? t("Signed in", "已登录")
                    : t("Sign-in needed", "需要登录")}
                </small>
              </span>
            </button>
          ))}
          <button
            className="account-option account-action"
            disabled={addingAccount}
            onClick={() => {
              setAccountMenu(false);
              addAccount();
            }}
          >
            <Icon name="plus" />{" "}
            <span>
              {addingAccount
                ? t("Opening…", "正在打开…")
                : t("Add WeChat account", "添加微信账号")}
            </span>
          </button>
          <button
            className="account-option account-action"
            onClick={() => switchAccount()}
          >
            <Icon name="people" />{" "}
            <span>{t("Choose an account", "选择账号")}</span>
          </button>
        </div>
      )}
      {newChat && state.account && (
        <NewChat
          key={state.account}
          connection={connection}
          account={state.account}
          close={() => router.up(view)}
          open={(id) => select(id, "replace")}
        />
      )}
      {settings && (
        <div className="overlay-backdrop" onClick={() => router.up(view)}>
          <section
            className="modal settings-modal"
            role="dialog"
            aria-modal="true"
            aria-label={t("Settings", "设置")}
            onClick={(e) => e.stopPropagation()}
          >
            <div className="modal-head">
              <h2>{t("Settings", "设置")}</h2>
              <Button
                icon="close"
                label={t("Close settings", "关闭设置")}
                onClick={() => router.up(view)}
              />
            </div>
            <div className="modal-body">
              <label className="setting">
                {t("Appearance", "外观")}
                <select
                  value={theme}
                  onChange={(e) => setTheme(e.target.value)}
                >
                  <option value="system">{t("System", "跟随系统")}</option>
                  <option value="light">{t("Light", "浅色")}</option>
                  <option value="dark">{t("Dark", "深色")}</option>
                </select>
              </label>
              <label className="setting">
                {t("Language", "语言")}
                <select
                  aria-label={t("Language", "语言")}
                  value={locale}
                  onChange={(e) => setLocale(e.target.value as Locale)}
                >
                  <option value="en">English</option>
                  <option value="zh-CN">简体中文</option>
                </select>
              </label>
              <div className="setting">
                <span>
                  agent-wechat hub<small>{connection.endpoint}</small>
                </span>
                {simulator
                  ? t("Simulator", "模拟器")
                  : t("Connected", "已连接")}
              </div>
              {desktop && (
                <>
                  <DesktopPreferences onConnection={onConnect} onDisconnect={disconnect} />
                  <button className="secondary" onClick={disconnect}>{t("Switch hub", "切换服务")}</button>
                </>
              )}
              {state.account && (
                <LocalCacheSettings
                  connection={connection}
                  account={state.account}
                />
              )}
              <div className="setting settings-accounts">
                <span>{t("Accounts", "账号")}</span>
                <button className="secondary" onClick={() => switchAccount()}>
                  {t("Manage", "管理")}
                </button>
              </div>
              {account && (
                <AccountSettings
                  key={account.id}
                  connection={connection}
                  account={account}
                  signedOut={() => {
                    session.deactivate(account.id);
                    switchAccount(true);
                  }}
                  removed={() => {
                    session.deactivate(account.id);
                    void connection.clearCache(account.id);
                    switchAccount(true);
                  }}
                />
              )}
            </div>
          </section>
        </div>
      )}
    </div>
  );
}
function SearchResults({
  connection,
  account,
  thread,
  query,
  name,
  close,
}: {
  connection: Connection;
  account: string;
  thread: string;
  query: string;
  name: (id: string) => string;
  close: () => void;
}) {
  useLocale();
  const [search] = useState(
    () => new HistorySearch(connection.session.client, account, thread),
  );
  const state = useSyncExternalStore(search.subscribe, search.snapshot);
  useEffect(() => {
    const timer = setTimeout(() => void search.search(query), 250);
    return () => clearTimeout(timer);
  }, [search, query]);
  useEffect(() => () => search.close(), [search]);
  const current = state.query === query.trim();
  return (
    <section
      aria-label={t("Conversation search results", "会话搜索结果")}
      className="history-search"
    >
      <div className="history-search-status">
        <span role="status">
          {!current || state.loading
            ? t(
                "Searching history on this agent-wechat hub…",
                "正在此 agent-wechat 服务上搜索历史记录…",
              )
            : t(
                `${state.items.length}${state.nextCursor ? "+" : ""} ${
                  state.items.length === 1 && !state.nextCursor
                    ? "match"
                    : "matches"
                } in this conversation`,
                `此会话中找到 ${state.items.length}${
                  state.nextCursor ? "+" : ""
                } 条匹配结果`,
              )}
        </span>
        <button onClick={close}>{t("Back to chat", "返回聊天")}</button>
      </div>
      {current && state.error && (
        <ErrorNotice>
          {state.error}{" "}
          <button onClick={() => void search.search(query)}>
            {t("Retry search", "重新搜索")}
          </button>
        </ErrorNotice>
      )}
      {current &&
        state.items.map((message) => (
          <Message
            key={message.id}
            message={message}
            connection={connection}
            name={name}
          />
        ))}
      {current && state.nextCursor && (
        <button
          className="load-history"
          disabled={state.loading}
          onClick={() => void search.more()}
        >
          {t("Load more matches", "加载更多匹配结果")}
        </button>
      )}
      {current && !state.loading && !state.error && !state.items.length && (
        <p className="empty-chat">
          {t(
            "No matches in the history available on this agent-wechat hub.",
            "此 agent-wechat 服务上的可用历史记录中没有匹配项。",
          )}
        </p>
      )}
    </section>
  );
}
function Pending({
  send,
  check,
  cancel,
  dismiss,
}: {
  send: PendingSend;
  check: () => Promise<void>;
  cancel: () => Promise<void>;
  dismiss: () => void;
}) {
  useLocale();
  const [busy, setBusy] = useState(false);
  const run = async (action: () => Promise<void>) => {
    setBusy(true);
    try {
      await action();
    } finally {
      setBusy(false);
    }
  };
  const state = send.intent?.state;
  const uncertain = send.uncertain || state === "needs_review";
  const ended = ["rejected", "failed", "cancelled"].includes(state ?? "");
  const label = uncertain
    ? t("Couldn't confirm send", "无法确认发送状态")
    : state === "rejected" || state === "failed"
    ? t("Send rejected", "发送被拒绝")
    : state === "cancelled"
    ? t("Send cancelled", "已取消发送")
    : state === "accepted"
    ? t("Accepted · loading message…", "已接收 · 正在加载消息…")
    : t("Queued…", "排队中…");
  return (
    <article
      className="message own"
      data-send-state={state || (send.uncertain ? "uncertain" : "submitting")}
      aria-busy={busy}
    >
      <Avatar name={t("You", "我")} />
      <div className="message-body">
        <div className="bubble">
          {send.quote && (
            <div className="pending-quote">
              <strong>
                {send.quotePreview?.senderName ||
                  t("Quoted message", "引用的消息")}
              </strong>
              <span>
                {emojiText(send.quotePreview?.text || t("Reply", "回复"))}
              </span>
            </div>
          )}
          <p>{emojiText(send.text)}</p>
        </div>
        <div
          className={`delivery ${
            uncertain || state === "rejected" || state === "failed"
              ? "warn"
              : ""
          }`}
          role="status"
        >
          {label}
          {!ended && (
            <button disabled={busy} onClick={() => void run(check)}>
              {t("Check status", "检查状态")}
            </button>
          )}
          {state === "queued" && (
            <button disabled={busy} onClick={() => void run(cancel)}>
              {t("Cancel", "取消")}
            </button>
          )}
          {(uncertain || ended) && (
            <button disabled={busy} onClick={dismiss}>
              {t("Dismiss", "忽略")}
            </button>
          )}
        </div>
        {uncertain && (
          <small className="send-explanation">
            {t(
              "Dismiss hides this notice; it doesn’t cancel delivery.",
              "忽略仅隐藏此提示，不会取消消息投递。",
            )}
          </small>
        )}
      </div>
    </article>
  );
}
function ReplyPreview({
  message,
  name,
  invalid,
  cancel,
}: {
  message: NativeMessage;
  name: (id: string) => string;
  invalid: boolean;
  cancel: () => void;
}) {
  useLocale();
  const navigation = useConversationNavigation();
  const sender =
    message.direction === "outgoing"
      ? t("You", "我")
      : name(message.senderId || message.threadId);
  return (
    <div
      className="composer-reply"
      role="region"
      aria-label={t("Replying to message", "正在回复消息")}
    >
      <Icon name="reply" />
      <button
        className="reply-source"
        onClick={() => void navigation?.jump(message)}
      >
        <strong>{sender}</strong>
        <span>
          {invalid
            ? message.recall
              ? t("Message recalled", "已撤回一条消息")
              : t("Message unavailable", "消息不可用")
            : emojiText(quotedText(message).replace(/\s+/g, " ").slice(0, 240))}
        </span>
      </button>
      <Button
        icon="close"
        label={t("Cancel reply", "取消回复")}
        onClick={cancel}
      />
    </div>
  );
}
function MessageDate({ value }: { value: string | null }) {
  useLocale();
  return (
    <div className="date-label">
      {date(value).toLocaleDateString(dateLocale(), {
        month: "long",
        day: "numeric",
      })}
    </div>
  );
}
function Message({
  message: m,
  connection,
  name,
}: {
  message: NativeMessage;
  connection: Connection;
  name: (id: string) => string;
}) {
  useLocale();
  const navigation = useConversationNavigation();
  const own = m.direction === "outgoing";
  const presentation = messagePresentation(m);
  const textLink = textMessageLink(m);
  if (presentation === "system") {
    return <SystemNotice messageId={m.id} text={m.body?.trim() || t("System message", "系统消息")}
      timestamp={time(m.createdAt)} dateTime={date(m.createdAt).toISOString()} />;
  }
  if (
    presentation === "recalled" &&
    (!m.senderId || !["incoming", "outgoing"].includes(m.direction ?? ""))
  )
    return (
      <article className="message recall-notice" data-message-id={m.id}>
        <div className="message-line">
          <span className="recalled">{recallText(m)}</span>
          <time
            className="message-time"
            dateTime={date(m.createdAt).toISOString()}
          >
            {time(m.createdAt)}
          </time>
        </div>
      </article>
    );
  return (
    <article
      className={`message ${own ? "own" : ""}${
        navigation?.target?.id === m.id ? " quote-target" : ""
      }`}
      data-message-id={m.id}
      data-direction={m.direction}
    >
      <HubAvatar
        connection={connection}
        account={m.accountId}
        username={m.senderId || (!own ? m.threadId : null)}
        name={own ? t("You", "我") : name(m.senderId || m.threadId)}
      />
      <div className="message-body">
        {!own && (
          <div className="message-meta">
            <b>{name(m.senderId || m.threadId)}</b>
          </div>
        )}
        <div className="message-line">
          <div className="message-content">
            <MessageActions message={m} connection={connection} />
            {presentation === "recalled" ? (
              <div className="bubble recalled">{recallText(m)}</div>
            ) : presentation === "text" ? (
              <div className="bubble">
                <p>
                  {textLink ? (
                    <a href={textLink.href} target="_blank" rel="noopener noreferrer">
                      {m.body}
                    </a>
                  ) : emojiText(m.body || (m.type >= 10000 ? t("System message", "系统消息") : ""))}
                </p>
              </div>
            ) : presentation === "reply" || presentation === "forwarded" ? (
              <div className={`bubble ${presentation}-message`}>
                {presentation === "reply" ? (
                  <QuotedReply
                    message={m}
                    connection={connection}
                    name={name}
                  />
                ) : (
                  <div className="message-context">
                    <Icon name="chat" />
                    {t("Forwarded messages", "聊天记录")}
                  </div>
                )}
                <p>
                  {emojiText(m.body ||
                    (presentation === "reply"
                      ? t("Reply text is unavailable.", "回复内容不可用。")
                      : t("Forwarded content is unavailable.", "转发内容不可用。")))}
                </p>
              </div>
            ) : presentation === "link" && m.link ? (
              <SharedLink message={m} connection={connection} />
            ) : presentation === "location" && m.location ? (
              <LocationMessage location={m.location} />
            ) : presentation === "media" ? (
              <MediaCard message={m} connection={connection} />
            ) : (
              <div className="bubble unsupported-message">
                <p>
                  {t(
                    "This WeChat message type isn’t supported yet.",
                    "暂不支持此类型的微信消息。",
                  )}
                </p>
              </div>
            )}
          </div>
          <time
            className="message-time"
            dateTime={date(m.createdAt).toISOString()}
          >
            {time(m.createdAt)}
          </time>
        </div>
      </div>
    </article>
  );
}
function Details({
  connection,
  threadId,
  name,
}: {
  connection: Connection;
  threadId: string;
  name: string;
}) {
  useLocale();
  const state = connection.session.snapshot();
  return (
    <aside
      className="detail-panel"
      aria-label={t("Conversation details", "聊天详情")}
    >
      <div className="detail-profile">
        <ConversationAvatar
          connection={connection}
          account={state.account!}
          username={threadId}
          name={name}
          members={
            state.threads.find((thread) => thread.id === threadId)
              ?.avatarMembers
          }
        />
        <h3>{name}</h3>
        <p>
          {threadId === "filehelper"
            ? t("Notes to yourself", "发送给自己")
            : threadId.endsWith("@chatroom")
            ? t("Group conversation", "群聊")
            : t("WeChat contact", "微信联系人")}
        </p>
      </div>
      <div className="detail-section">
        <div className="section-head">{t("Conversation", "会话")}</div>
        <p className="peer-id" title={t("Internal ID", "内部 ID")}>
          {threadId}
        </p>
      </div>
      {state.account && (
        <ThreadPinSwitch
          key={`pin:${state.account}:${threadId}`}
          connection={connection}
          account={state.account}
          threadId={threadId}
        />
      )}
      {state.account && <ThreadNotifications key={state.account+threadId} connection={connection} account={state.account} threadId={threadId} />}
      {state.account && threadId.endsWith("@chatroom") && (
        <GroupSettings
          connection={connection}
          account={state.account}
          groupId={threadId}
        />
      )}
    </aside>
  );
}

createRoot(document.getElementById("root")!).render(<><App />{desktop && <DesktopUpdates notice />}</>);
