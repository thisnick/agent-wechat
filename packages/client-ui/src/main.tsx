import { ConversationRow } from "./conversation-row.tsx";
import { AppWindow } from "lucide-react";
import { ConversationFilterBar } from "./conversation-filter-bar.tsx";
import { conversationCategory, conversationUnreadCounts, savedConversationCategories } from "./conversation-filters.ts";
import { ThreadPinSwitch, useThreadPinPreview } from "./thread-pinning.tsx";
import { ThreadNotifications } from "./thread-notifications.tsx";
import { emojiText } from "./emoji-text.ts";
import { formatLocale, richText, t, useLanguage } from "./i18n.ts";
import { DesktopUpdates } from "./desktop-updates.tsx";
import { desktop, desktopConnect, desktopInvoke, desktopNotifications, installDesktopLinks, type DesktopSession } from "./desktop.ts";
import { DesktopWelcome, DesktopPreferences } from "./desktop-ui.tsx";
import { LocalCacheSettings } from "./local-cache-settings.tsx";
import { GroupSettings } from "./group-settings.tsx";
import { createRoot } from "react-dom/client";
import { flushSync } from "react-dom";
import {
  useEffect,
  useMemo,
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
import { Avatar, Button, Icon, LanguageSelect, Logo, Welcome } from "./ui.tsx";
import { HistorySearch } from "./history-search.ts";
import { MediaCard, PendingAttachments } from "./media.tsx";
import { AttachmentPicker, AttachmentTray } from "./attachment-picker.tsx";
import { sendAttachmentBatch } from "./attachment-drafts.ts";
import { ComposerHelp } from "./composer-help.tsx";
import { ConversationAvatar, HubAvatar } from "./avatars.tsx";
import { conversationName, orderThreads } from "./thread-preview.ts";
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
import { MiniProgramCard, MiniProgramsList, MiniProgramsProvider } from "./mini-program.tsx";
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
    ? date(value).toLocaleTimeString(formatLocale(), {
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
  useEffect(() => desktop ? installDesktopLinks() : undefined, []);
  const [connection, setConnection] = useState<Connection | null>(null);
  const [starting, setStarting] = useState(true);
  // Choosing a hub is a view of the address, so Back returns from it.
  const changingHub = useRoute()?.name === "servers";
  const [error, setError] = useState("");
  const [theme, setTheme] = useTheme();
  useEffect(() => {
    let cancelled = false;
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
        <h1>{t("main.openingAgentwechat")}</h1>
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
          <Icon name="back" /> {t("main.back")}
        </button>
      )}
      <div className="welcome-heading">
        <h1>
          {t("main.connectAgentWechatHub")}
        </h1>
        <p>
          {t("main.enterAddressAccessKeyAgent")}
        </p>
      </div>
      {hubs.length > 1 && <div className="preview-hubs" aria-label={t("main.previewHubs")}>
        {hubs.map(hub => <button key={hub.id} type="button" className="secondary" disabled={busy}
          onClick={() => {
            setBusy(true); setError("");
            void bootstrap(undefined, hub.id).then(value => {
              if (!value) throw Error(t("main.reopenPrivatePreviewLinkChoose"));
              onConnect(value);
            }).catch(e => setError(readableError(e))).finally(() => setBusy(false));
          }}>{hub.name}</button>)}
      </div>}
      <form onSubmit={submit}>
        <div className="remote-fields">
          <label htmlFor="hub-address">
            {t("main.agentWechatHubAddress")}
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
            {t("main.agentWechatHubAccessKey")}
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
              {show ? t("main.hide") : t("main.show")}
            </button>
          </div>
        </div>
        {error && <ErrorNotice>{error}</ErrorNotice>}
        <button className="continue-button" disabled={busy}>
          {busy ? t("main.connecting") : t("main.connect")}{" "}
          <Icon name="chevron" />
        </button>
      </form>
      <details className="hub-help">
        <summary>
          {t("main.useDeviceAgentWechatHub")}
        </summary>
        <p>
          {richText("main.localDevHelp", { command: <code>pnpm client:dev</code> })}
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
    fallBack(requested && requested.name !== "servers" ? t("main.linkForDifferentHub") : null, home);
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
        fallBack(known.current.has(unknownAccount) ? null : t("main.accountNotOnHub"));
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
        <h1>{t("main.openingAgentwechat")}</h1>
      </Welcome>
    );
  if (screen === "login" && account)
    return (
      <Welcome simulator={simulator}>
        {state.signInRequired === account.id && (
          <ErrorNotice>{t("main.wechatSignedOutSignAgain")}</ErrorNotice>
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
              ? t("main.chooseAccount")
              : t("main.signWechat")}
          </h1>
          {!state.accounts.length && (
            <p>
              {simulator
                ? t("main.createSyntheticAccountTryConnected")
                : t("main.addWechatAccountGetStarted")}
            </p>
          )}
        </div>
        {state.signInRequired && (
          <ErrorNotice>
            {t("main.wechatSignedOutSignAgain")}
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
                      ? t("main.openChats")
                      : t("main.signWechat")}
                  </small>
                </span>
                <Icon name="chevron" />
              </button>
              <button
                className="profile-manage"
                aria-label={t("main.manage", { value: a.alias || a.id })}
                title={t("main.manageProfile")}
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
            ? t("main.opening")
            : t("main.addWechatAccount")}{" "}
          <Icon name="plus" />
        </button>
        <button className="quiet-button" onClick={disconnect}>
          {t("main.connectDifferentAgentWechatHub")}
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
      setError(t("main.simulatorControlFailed"));
    } finally {
      setBusy(false);
    }
  }
  return (
    <>
      <button className="step-back" onClick={cancel}>
        <Icon name="back" /> {t("main.back")}
      </button>
      <div className="welcome-heading">
        <h1>
          {state === "awaiting_approval"
            ? t("main.confirmSignWechat")
            : state === "expired"
            ? t("main.qrCodeExpired")
            : terminal
            ? t("main.signCouldNotFinish")
            : !qr
            ? t("main.signing")
            : t("main.signWechat")}
        </h1>
        <p>
          {connection.simulator
            ? t("main.simulatorQrOnlyCannotSign")
            : state === "awaiting_approval"
            ? t("main.openWechatPhoneConfirmSign")
            : qr
            ? t("main.wechatPhoneTapThenScan")
            : terminal
            ? t("main.trySigningAgain")
            : t("main.tryingSavedWechatSign")}
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
                ? t("main.simulatorSignQrCode")
                : t("main.wechatSignQrCode")
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
              ? t("main.tryAgainContinue")
              : t("main.connecting")}
          </p>
        )}
      </div>
      {qr && !terminal && state !== "awaiting_approval" && (
        <p className="qr-expiry" role="status">
          {remaining === 0
            ? t("main.refreshingCode")
            : remaining !== null && (!connection.simulator || clock !== null)
              ? t("main.codeRefreshIn", {
                  time: `${Math.floor(remaining / 60)}:${String(remaining % 60).padStart(2, "0")}`,
                })
              : t("main.codeRefreshSoon")}
        </p>
      )}
      {error && <ErrorNotice>{error}</ErrorNotice>}
      {terminal || error ? (
        <button
          className="continue-button"
          onClick={() => setAttempt((a) => a + 1)}
        >
          {t("main.tryAgain")}
        </button>
      ) : connection.simulator ? (
        <div className="sim-phone">
          <span>{t("main.simulatedPhone")}</span>
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
              ? t("main.simulatePhoneApproval")
              : t("main.simulateScan")}{" "}
            <Icon name="chevron" />
          </button>
          {state !== "awaiting_approval" && (
            <details className="sim-login-tools">
              <summary>
                {t("main.simulatorTestControls")}
              </summary>
              <button
                className="quiet-button"
                disabled={busy || !qr}
                onClick={() => void simulate("expired")}
              >
                {t("main.simulateCodeExpiry")}
              </button>
            </details>
          )}
        </div>
      ) : (
        <p className="field-help">
          {t("main.chatsOpenAutomaticallyAfterSign")}
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
  const savedView = state.account ? connection.cache.view(state.account) : {};
  const [query, setQuery] = useState("");
  const [categories, setCategories] = useState(() =>
    savedConversationCategories(state.account ? connection.cache.view(state.account) : {}),
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
  const programsView = view.name === "mini-programs";
  const mobile = routeThread !== null || programsView;
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
      else fallBack(t("main.chatNotAvailable"), listRoute);
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
      connection.cache.saveView(state.account, { section, categories });
  }, [connection, state.account, section, categories]);
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
  const name = (id: string) => {
    const thread = state.threads.find((item) => item.id === id);
    return conversationName(
      id,
      state.contacts.find((item) => item.id === id)?.displayName || thread?.displayName,
      thread?.avatarMembers,
      account?.ownUsername,
    );
  };
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
      setSimStatus(t("main.scenarioApplied"));
    } catch {
      setSimStatus(t("main.scenarioControlFailed"));
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
  const unreadCounts = useMemo(() => conversationUnreadCounts(state.threads), [state.threads]);
  const search = query.toLowerCase();
  const threads = orderThreads(useThreadPinPreview(connection, state.account!, state.threads)).filter((thread) => {
    if (categories.length) {
      const category = conversationCategory(thread);
      if (!category || !categories.includes(category)) return false;
    }
    return name(thread.id).toLowerCase().includes(search) ||
      (thread.id.endsWith("@chatroom") && thread.id.toLowerCase().includes(search));
  });
  const firstPinned = threads.find((thread) => thread.pinned === true)?.id;
  return (
    <MiniProgramsProvider key={JSON.stringify([here.server, here.account])}
      connection={connection} account={here.account} accountName={account?.alias || here.account}
      active={programsView}>
    <div className={`shell ${mobile ? "mobile-chat" : ""} ${programsView ? "mini-programs-view" : ""}`}>
      <nav className="rail" aria-label={t("main.mainNavigation")}>
        <div className="brand" title="AgentWechat">
          <Logo />
        </div>
        <button
          className={`nav-btn ${!programsView && section === "chats" ? "active" : ""}`}
          aria-label={t("main.chats")}
          onClick={() => router.push(listRoute)}
        >
          <Icon name="chat" />
        </button>
        <button
          className={`nav-btn ${section === "contacts" ? "active" : ""}`}
          aria-label={t("main.contacts")}
          onClick={() => router.push({ name: "contacts", ...here })}
        >
          <Icon name="people" />
        </button>
        <button
          className={`nav-btn ${programsView ? "active" : ""}`}
          aria-label={t("miniPrograms.title")}
          aria-current={programsView ? "page" : undefined}
          title={t("miniPrograms.navigation")}
          onClick={() => router.push({ name: "mini-programs", ...here })}
        >
          <AppWindow size={20} aria-hidden="true" />
        </button>
        <div className="rail-spacer" />
        <button
          className="nav-btn"
          aria-label={t("main.settings")}
          onClick={() => router.push({ name: "settings", ...here })}
        >
          <Icon name="settings" />
        </button>
        <div className="rail-rule" />
        <button
          className="nav-btn account-btn"
          aria-label={t("main.switchAccount")}
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
                ? t("main.messages")
                : t("main.contacts")}
            </h1>
            <Button
              icon="plus"
              label={t("main.newConversation")}
              onClick={() => router.push({ name: "new-chat", ...here })}
            />
          </div>
          <label className="search">
            <Icon name="search" />
            <input
              aria-label={
                section === "chats"
                  ? t("main.searchConversations")
                  : t("main.searchContacts")
              }
              placeholder={
                section === "chats"
                  ? t("main.searchConversations")
                  : t("main.findPerson")
              }
              value={query}
              onChange={(e) => setQuery(e.target.value)}
            />
          </label>
          {section === "chats" && (
            <ConversationFilterBar
              selected={categories}
              counts={unreadCounts}
              onChange={setCategories}
            />
          )}
        </div>
        {shownNotice && !routeThread && (
          <div className="banner route-notice" role="status">
            {shownNotice}
            <button onClick={dismissNotice}>{t("main.dismiss")}</button>
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
                  ? t("main.noMatches")
                  : t("main.noConversationsYet")}
              </p>
            )}
        </div>
        <div className="side-footer">
          <span>
            <i className={`status-dot ${!state.connected ? "off" : ""}`} />
            {state.connected
              ? t("main.agentWechatHubConnected")
              : state.reconnecting
              ? t("main.reconnectingLiveUpdates")
              : t("main.connectingAgentWechatHub")}
          </span>
          {simulator && (
            <span className="sim-label">{t("main.simulator")}</span>
          )}
        </div>
      </aside>
      <main className="workspace">
        {programsView ? <MiniProgramsList /> : <>
        <header className="chat-header">
          <Button
            icon="back"
            label={t("main.backConversations")}
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
                ? t("main.notesYourself")
                : state.selected
                ? state.selected.endsWith("@chatroom")
                  ? t("main.groupConversation")
                  : t("main.wechatConversation")
                : t("main.chooseConversation")}
            </p>
          </div>
          <div className="header-actions">
            <Button
              icon="search"
              label={t("main.findConversation")}
              onClick={() => {
                setFind(!find);
                setInChat("");
              }}
            />
            <Button
              icon="panel"
              label={t("main.conversationDetails")}
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
                aria-label={t("main.findChat")}
                placeholder={t("main.findChat2")}
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
              {t("main.refresh")}
            </button>
          </div>
        )}
        {shownNotice && routeThread && (
          <div className="banner route-notice" role="status">
            {shownNotice}
            <button onClick={dismissNotice}>{t("main.dismiss")}</button>
          </div>
        )}
        {leftGroup && (
          <div className="banner">
            {t("main.noLongerGroupCachedHistory")}
          </div>
        )}
        {account?.authenticated === false && (
          <div className="banner">
            {t("main.signWechatAgainSendMessages")}
            <button onClick={() => switchAccount}>{t("main.sign")}</button>
          </div>
        )}
        {simulator && (
          <details className="simulator-tools">
            <summary>{t("main.simulatorControls")}</summary>
            <div className="sim-controls">
              <input
                aria-label={t("main.syntheticIncomingText")}
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
                {t("main.receiveMessage")}
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
                {t("main.sendPhone")}
              </button>
              <button
                onClick={() =>
                  void simulate({
                    op: "advertise_image",
                    peer: state.selected || "sim_mei",
                  })
                }
              >
                {t("main.unavailableImage")}
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
                  {t("main.receive", { kind })}
                </button>
              ))}
              <button
                onClick={() =>
                  void simulate({ op: "send_fault", fault: "reject" })
                }
              >
                {t("main.rejectNextSend")}
              </button>
              <button
                onClick={() =>
                  void simulate({
                    op: "send_fault",
                    fault: "lost_acknowledgment",
                  })
                }
              >
                {t("main.loseNextAcknowledgment")}
              </button>
            </div>
            <p role="status">
              {simStatus ||
                t("main.syntheticUpstreamOnlyNoOfficial")}
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
                  fallBack(t("main.photoOrVideoNotAvailable"), chatRoute(state.selected)),
              }}
            >
              {view.name === "message" && state.account && state.selected === view.thread && (
                <RouteMessage
                  session={session}
                  account={state.account}
                  thread={view.thread}
                  message={view.message}
                  missing={() => fallBack(t("main.messageNotInChat"), chatRoute(view.thread))}
                />
              )}
              <div className="body-row">
                <section
                  className="conversation"
                  aria-label={t("main.conversation")}
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
                                {t("main.startConversation")}
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
                              ? t("main.loadingReply")
                              : t("main.quotedMessageUnavailable")}
                          </span>
                          <Button
                            icon="close"
                            label={t("main.removeReply")}
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
                      {attachments.files.length > 0 && reply && !draft.trim() && <p className="field-help">{t("main.writeTextQuotedReplyFiles")}</p>}
                      {inlineEditor && state.account && state.selected ? <MentionEditor
                        key={draftKey} connection={connection} account={state.account} thread={state.selected} inputRef={mentionInput}
                        value={draft} tokens={draftMentions} disabled={!active} allowMentions={inlineCapable && active}
                        canMentionAll={allowAllMention}
                        openRequest={mentionOpen} placeholder={t("main.message", { selectedName })} onChange={changeDraft} onSend={send} onFocus={acknowledgeComposer} onEscape={clearReply}
                      /> : <textarea
                        ref={messageInput}
                        aria-label={t("main.message2")}
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
                            ? t("main.message", { selectedName })
                            : t("main.chooseConversation")
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
                      {mentionProblems[draftKey] && <p className="mention-error" role="alert">{richText("main.mentionRestoreFailed", { action: <button onClick={() => { setMentionProblems(values => ({ ...values, [draftKey]: false })); changeDraft(draft); }}>{t("main.keepOnlyText")}</button> })}</p>}
                      {allMentionBlocked && <p className="mention-error" role="alert">{allMentionProblem(state.selectedGroup, state.selected)} <button type="button" onClick={() => { const next = removeAllMentions(draft, draftMentions); changeDraft(next.text, next.tokens); }}>{t("main.removeEveryone")}</button> <button type="button" disabled={!active || state.opening} onClick={() => { void session.refreshConversations(state.account, false, state.selected ?? undefined).catch(() => {}); }}>{t("main.refresh")}</button></p>}
                      <div className="composer-bottom">
                        <div className="composer-tools" role="group" aria-label={t("main.messageActions")}>
                          {state.account && <AttachmentPicker key={JSON.stringify([state.account, state.selected])} connection={connection} account={state.account} threadId={state.selected}
                            composerRef={composerInput} mediaKinds={Array.isArray(state.capabilities.mediaKinds) ? state.capabilities.mediaKinds : []}
                            capabilities={state.capabilities}
                            focusMessage={focusMessage}
                            enabled={active && !attachments.submission && Array.isArray(state.capabilities.mediaKinds) && state.capabilities.mediaKinds.length > 0} />}
                          <Button icon="mention" label={t("main.mentionSomeone")}
                            title={!state.selected?.endsWith("@chatroom") ? t("main.mentionsAvailableGroupChats") : !inlineCapable ? t("main.updateHubUseInlineMentions") : t("main.mentionSomeone")}
                            disabled={!inlineCapable || !active} onClick={() => setMentionOpen(value => value + 1)} />
                        </div>
                        {simulator && <span className="composer-hint">{t("main.messagesStaySimulator")}</span>}
                        <ComposerHelp label={t("main.keyboardShortcuts")}><span><kbd>Enter</kbd> {t("main.send")}</span><span><kbd>Shift</kbd> + <kbd>Enter</kbd> {t("main.newLine")}</span></ComposerHelp>
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
                          {t("main.send2")} <Icon name="send" />
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
        </>}
      </main>
      {accountMenu && (
        <div
          className="account-menu"
          role="dialog"
          aria-label={t("main.switchAccount")}
        >
          <div className="eyebrow">{t("main.wechatAccounts")}</div>
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
                    ? t("main.signed")
                    : t("main.signNeeded")}
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
                ? t("main.opening")
                : t("main.addWechatAccount")}
            </span>
          </button>
          <button
            className="account-option account-action"
            onClick={() => switchAccount()}
          >
            <Icon name="people" />{" "}
            <span>{t("main.chooseAccount")}</span>
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
            aria-label={t("main.settings")}
            onClick={(e) => e.stopPropagation()}
          >
            <div className="modal-head">
              <h2>{t("main.settings")}</h2>
              <Button
                icon="close"
                label={t("main.closeSettings")}
                onClick={() => router.up(view)}
              />
            </div>
            <div className="modal-body">
              <label className="setting">
                {t("main.appearance")}
                <select
                  value={theme}
                  onChange={(e) => setTheme(e.target.value)}
                >
                  <option value="system">{t("main.system")}</option>
                  <option value="light">{t("main.light")}</option>
                  <option value="dark">{t("main.dark")}</option>
                </select>
              </label>
              <label className="setting">
                {t("main.language")}
                <LanguageSelect />
              </label>
              <div className="setting">
                <span>
                  agent-wechat hub<small>{connection.endpoint}</small>
                </span>
                {simulator
                  ? t("main.simulator2")
                  : t("main.connected")}
              </div>
              {desktop && (
                <>
                  <DesktopPreferences onConnection={onConnect} onDisconnect={disconnect} />
                  <button className="secondary" onClick={disconnect}>{t("main.switchHub")}</button>
                </>
              )}
              {state.account && (
                <LocalCacheSettings
                  connection={connection}
                  account={state.account}
                />
              )}
              <div className="setting settings-accounts">
                <span>{t("main.accounts")}</span>
                <button className="secondary" onClick={() => switchAccount()}>
                  {t("main.manage2")}
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
    </MiniProgramsProvider>
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
      aria-label={t("main.conversationSearchResults")}
      className="history-search"
    >
      <div className="history-search-status">
        <span role="status">
          {!current || state.loading
            ? t("main.searchingHistoryAgentWechatHub")
            : t(state.nextCursor ? "main.matchesInConversationMore" : "main.matchesInConversation", { count: state.items.length })}
        </span>
        <button onClick={close}>{t("main.backChat")}</button>
      </div>
      {current && state.error && (
        <ErrorNotice>
          {state.error}{" "}
          <button onClick={() => void search.search(query)}>
            {t("main.retrySearch")}
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
          {t("main.loadMoreMatches")}
        </button>
      )}
      {current && !state.loading && !state.error && !state.items.length && (
        <p className="empty-chat">
          {t("main.noMatchesHistoryAvailableAgent")}
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
    ? t("main.couldnTConfirmSend")
    : state === "rejected" || state === "failed"
    ? t("main.sendRejected")
    : state === "cancelled"
    ? t("main.sendCancelled")
    : state === "accepted"
    ? t("main.acceptedLoadingMessage")
    : t("main.queued");
  return (
    <article
      className="message own"
      data-send-state={state || (send.uncertain ? "uncertain" : "submitting")}
      aria-busy={busy}
    >
      <Avatar name={t("main.you")} />
      <div className="message-body">
        <div className="bubble">
          {send.quote && (
            <div className="pending-quote">
              <strong>
                {send.quotePreview?.senderName ||
                  t("main.quotedMessage")}
              </strong>
              <span>
                {emojiText(send.quotePreview?.text || t("main.reply"))}
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
              {t("main.checkStatus")}
            </button>
          )}
          {state === "queued" && (
            <button disabled={busy} onClick={() => void run(cancel)}>
              {t("main.cancel")}
            </button>
          )}
          {(uncertain || ended) && (
            <button disabled={busy} onClick={dismiss}>
              {t("main.dismiss")}
            </button>
          )}
        </div>
        {uncertain && (
          <small className="send-explanation">
            {t("main.dismissHidesNoticeDoesnT")}
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
  const navigation = useConversationNavigation();
  const sender =
    message.direction === "outgoing"
      ? t("main.you")
      : name(message.senderId || message.threadId);
  return (
    <div
      className="composer-reply"
      role="region"
      aria-label={t("main.replyingMessage")}
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
              ? t("main.messageRecalled")
              : t("main.messageUnavailable")
            : emojiText(quotedText(message).replace(/\s+/g, " ").slice(0, 240))}
        </span>
      </button>
      <Button
        icon="close"
        label={t("main.cancelReply")}
        onClick={cancel}
      />
    </div>
  );
}
function MessageDate({ value }: { value: string | null }) {
  return (
    <div className="date-label">
      {date(value).toLocaleDateString(formatLocale(), {
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
  const navigation = useConversationNavigation();
  const own = m.direction === "outgoing";
  const presentation = messagePresentation(m);
  const textLink = textMessageLink(m);
  if (presentation === "system") {
    return <SystemNotice messageId={m.id} text={m.body?.trim() || t("main.systemMessage")}
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
        name={own ? t("main.you") : name(m.senderId || m.threadId)}
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
                  ) : emojiText(m.body || (m.type >= 10000 ? t("main.systemMessage") : ""))}
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
                    {t("main.forwardedMessages")}
                  </div>
                )}
                <p>
                  {emojiText(m.body ||
                    (presentation === "reply"
                      ? t("main.replyTextUnavailable")
                      : t("main.forwardedContentUnavailable")))}
                </p>
              </div>
            ) : presentation === "mini-program" && m.miniProgram ? (
              <MiniProgramCard card={m.miniProgram} message={m} connection={connection} />
            ) : presentation === "link" && m.link ? (
              <SharedLink message={m} connection={connection} />
            ) : presentation === "location" && m.location ? (
              <LocationMessage location={m.location} />
            ) : presentation === "media" ? (
              <MediaCard message={m} connection={connection} />
            ) : (
              <div className="bubble unsupported-message">
                <p>
                  {t("main.wechatMessageTypeIsnT")}
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
  const state = connection.session.snapshot();
  return (
    <aside
      className="detail-panel"
      aria-label={t("main.conversationDetails")}
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
            ? t("main.notesYourself")
            : threadId.endsWith("@chatroom")
            ? t("main.groupConversation")
            : t("main.wechatContact")}
        </p>
      </div>
      <div className="detail-section">
        <div className="section-head">{t("main.conversation")}</div>
        <p className="peer-id" title={t("main.internalId")}>
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
          title={name}
        />
      )}
    </aside>
  );
}

function Root() {
  useLanguage();
  return <><App />{desktop && <DesktopUpdates notice />}</>;
}
createRoot(document.getElementById("root")!).render(<Root />);
