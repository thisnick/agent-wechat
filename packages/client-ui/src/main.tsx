import { GroupSettings } from "./group-settings.tsx";
import { createRoot } from "react-dom/client";
import {
  useEffect,
  useRef,
  useState,
  useSyncExternalStore,
  type FormEvent,
  type ReactNode,
} from "react";
import {
  type NativeMessage,
  type NativeOperation,
  isNativeTombstone,
} from "@agent-wechat/shared";
import {
  bootstrap,
  connect,
  control,
  simulatorClock,
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
import { AttachmentPicker, MediaCard, PendingAttachments } from "./media.tsx";
import { ConversationAvatar, HubAvatar } from "./avatars.tsx";
import { threadPreview } from "./thread-preview.ts";
import { messagePresentation } from "./message-presentation.ts";
import { AccountSettings } from "./account-settings.tsx";
import { NewChat } from "./new-chat.tsx";
import {
  ConversationScroll,
  ConversationItem,
} from "./conversation-scroll.tsx";
import "./style.css";
import "./onboarding.css";
import "./client.css";

function date(value: string | null): Date {
  return new Date(
    value && /^\d+$/.test(value) ? Number(value) * 1000 : (value ?? 0),
  );
}
function time(value: string | null): string {
  return value
    ? date(value).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" })
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
function ErrorNotice({ children }: { children: ReactNode }) {
  return (
    <div className="connection-error" role="alert">
      <Icon name="help" />
      <span>{children}</span>
    </div>
  );
}
function App() {
  const [connection, setConnection] = useState<Connection | null>(null);
  const [starting, setStarting] = useState(true);
  const [error, setError] = useState("");
  const [theme, setTheme] = useTheme();
  useEffect(() => {
    let cancelled = false;
    void bootstrap()
      .then((value) => {
        if (cancelled) value?.close();
        else setConnection(value);
      })
      .catch((e) => {
        if (!cancelled) setError(readableError(e));
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
    document.body.classList.toggle("onboarding-active", !connection);
  }, [connection]);
  if (starting)
    return (
      <Welcome simulator={false}>
        <h1>Opening AgentWechat…</h1>
      </Welcome>
    );
  if (!connection)
    return <ConnectionForm error={error} onConnect={setConnection} />;
  return (
    <Client
      key={connection.endpoint}
      connection={connection}
      theme={theme}
      setTheme={setTheme}
      disconnect={() => setConnection(null)}
    />
  );
}
function ConnectionForm({
  onConnect,
  error: initialError,
}: {
  onConnect: (c: Connection) => void;
  error: string;
}) {
  const [endpoint, setEndpoint] = useState(location.origin),
    [token, setToken] = useState("");
  const [error, setError] = useState(initialError),
    [busy, setBusy] = useState(false),
    [show, setShow] = useState(false);
  async function submit(event: FormEvent) {
    event.preventDefault();
    if (busy) return;
    setBusy(true);
    setError("");
    try {
      const connection = await connect(endpoint, token);
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
      <div className="welcome-heading">
        <h1>Connect to an agent-wechat hub</h1>
        <p>Enter the address and access key for your agent-wechat hub.</p>
      </div>
      <form onSubmit={submit}>
        <div className="remote-fields">
          <label htmlFor="hub-address">agent-wechat hub address</label>
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
          <label htmlFor="hub-key">agent-wechat hub access key</label>
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
              {show ? "Hide" : "Show"}
            </button>
          </div>
        </div>
        {error && <ErrorNotice>{error}</ErrorNotice>}
        <button className="continue-button" disabled={busy}>
          {busy ? "Connecting…" : "Connect"} <Icon name="chevron" />
        </button>
      </form>
      <details className="hub-help">
        <summary>Use this device as an agent-wechat hub</summary>
        <p>
          The browser connects to a running agent-wechat hub. For local
          development, start <code>pnpm client:dev</code> on this device and
          open its printed URL.
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
}: {
  connection: Connection;
  theme: string;
  setTheme: (t: string) => void;
  disconnect: () => void;
}) {
  const { session, simulator } = connection;
  const state = useSyncExternalStore(session.subscribe, session.snapshot);
  const [screen, setScreen] = useState<"accounts" | "login" | "chats">(
    "accounts",
  );
  const [loginAccount, setLoginAccount] = useState<Account | null>(null);
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);
  useEffect(() => {
    document.body.classList.toggle("onboarding-active", screen !== "chats");
  }, [screen]);
  async function open(account: Account) {
    setBusy(true);
    setError("");
    try {
      if (!account.authenticated) {
        setLoginAccount(account);
        setScreen("login");
        return;
      }
      await session.activate(account.id);
      setScreen("chats");
    } catch (e) {
      setError(readableError(e));
    } finally {
      setBusy(false);
    }
  }
  async function add() {
    setBusy(true);
    setError("");
    try {
      const id = simulator
        ? `account-${crypto.randomUUID().slice(0, 8)}`
        : undefined;
      const result = await session.client.createAccount({
        id,
        alias: `account-${state.accounts.length + 1}`,
      });
      await session.accounts();
      setLoginAccount(result as Account);
      setScreen("login");
    } catch (e) {
      setError(readableError(e));
    } finally {
      setBusy(false);
    }
  }
  if (screen === "login" && loginAccount)
    return (
      <Welcome simulator={simulator}>
        <Login
          connection={connection}
          account={loginAccount.id}
          cancel={() => {
            setScreen("accounts");
            void session.accounts();
          }}
          signedIn={async () => {
            await session.accounts();
            await session.activate(loginAccount.id);
            setScreen("chats");
          }}
        />
      </Welcome>
    );
  if (screen !== "chats")
    return (
      <Welcome simulator={simulator}>
        <div className="welcome-heading">
          <h1>
            {state.accounts.length ? "Choose an account" : "Sign in to WeChat"}
          </h1>
          {!state.accounts.length && (
            <p>
              {simulator
                ? "Create a synthetic account to try the connected client."
                : "Add your WeChat account to get started."}
            </p>
          )}
        </div>
        <div className="login-accounts">
          {state.accounts.map((a) => (
            <button
              key={a.id}
              className="login-account"
              disabled={busy}
              onClick={() => void open(a)}
            >
              <Avatar name={a.alias || a.id} />
              <span>
                <strong>{a.alias || a.id}</strong>
                <small>
                  {a.authenticated ? "Open chats" : "Sign in to WeChat"}
                </small>
              </span>
              <Icon name="chevron" />
            </button>
          ))}
        </div>
        {error && <ErrorNotice>{error}</ErrorNotice>}
        <button
          className="continue-button"
          disabled={busy}
          onClick={() => void add()}
        >
          {busy ? "Opening…" : "Add WeChat account"} <Icon name="plus" />
        </button>
        <button className="quiet-button" onClick={disconnect}>
          Connect to a different agent-wechat hub
        </button>
      </Welcome>
    );
  return (
    <Chats
      connection={connection}
      theme={theme}
      setTheme={setTheme}
      switchAccount={() => {
        setScreen("accounts");
        void session.accounts();
      }}
      addAccount={() => void add()}
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
            ["failed", "expired", "rejected", "cancelled"].includes(
              current.state,
            )
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
  const terminal = ["failed", "expired", "rejected", "cancelled"].includes(
    state ?? "",
  );
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
      setError("Simulator control failed.");
    } finally {
      setBusy(false);
    }
  }
  return (
    <>
      <button className="step-back" onClick={cancel}>
        <Icon name="back" /> Back
      </button>
      <div className="welcome-heading">
        <h1>
          {state === "awaiting_approval"
            ? "Confirm sign-in in WeChat"
            : state === "expired"
              ? "QR code expired"
              : terminal
                ? "Sign-in could not finish"
                : "Sign in to WeChat"}
        </h1>
        <p>
          {connection.simulator
            ? "Simulator QR only. It cannot sign in to WeChat. Use the phone controls below."
            : "In WeChat on your phone, tap +, then Scan."}
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
                ? "Simulator sign-in QR code"
                : "WeChat sign-in QR code"
            }
            data-qr-generation={
              (operation?.result as { qrGeneration?: number } | null)
                ?.qrGeneration
            }
            data-expires-at={expiresAt}
          />
        ) : (
          <p>
            {terminal ? "Get a new code to try again." : "Preparing your code…"}
          </p>
        )}
      </div>
      {qr && !terminal && state !== "awaiting_approval" && (
        <p className="qr-expiry" role="status">
          {remaining === 0 ? "Refreshing code…" : "Code refresh in"}
          {remaining !== null && (!connection.simulator || clock !== null)
            ? remaining > 0
              ? ` ${Math.floor(remaining / 60)}:${String(
                  remaining % 60,
                ).padStart(2, "0")}`
              : ""
            : " a moment"}
        </p>
      )}
      {error && <ErrorNotice>{error}</ErrorNotice>}
      {terminal || error ? (
        <button
          className="continue-button"
          onClick={() => setAttempt((a) => a + 1)}
        >
          Get a new code
        </button>
      ) : connection.simulator ? (
        <div className="sim-phone">
          <span>Simulated phone</span>
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
              ? "Simulate phone approval"
              : "Simulate scan"}{" "}
            <Icon name="chevron" />
          </button>
          {state !== "awaiting_approval" && (
            <details className="sim-login-tools">
              <summary>Simulator test controls</summary>
              <button
                className="quiet-button"
                disabled={busy || !qr}
                onClick={() => void simulate("expired")}
              >
                Simulate code expiry
              </button>
            </details>
          )}
        </div>
      ) : (
        <p className="field-help">
          Chats open automatically after you confirm on your phone.
        </p>
      )}
    </>
  );
}
function Chats({
  connection,
  theme,
  setTheme,
  switchAccount,
  addAccount,
}: {
  connection: Connection;
  theme: string;
  setTheme: (t: string) => void;
  switchAccount: () => void;
  addAccount: () => void;
}) {
  const { session, simulator } = connection,
    state = useSyncExternalStore(session.subscribe, session.snapshot);
  const [section, setSection] = useState<"chats" | "contacts">("chats"),
    [query, setQuery] = useState(""),
    [filter, setFilter] = useState("all");
  const [mobile, setMobile] = useState(false),
    [details, setDetails] = useState(false),
    [settings, setSettings] = useState(false),
    [accountMenu, setAccountMenu] = useState(false);
  const [newChat, setNewChat] = useState(false);
  const [drafts, setDrafts] = useState<Record<string, string>>({});
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
    (id === "filehelper" ? "File Transfer" : id);
  const selectedName = state.selected ? name(state.selected) : "";
  const draftKey = `${state.account}:${state.selected}`,
    draft = drafts[draftKey] ?? "";
  const leftGroup = state.selectedGroup?.isInGroup === false;
  const active = account?.authenticated !== false && !leftGroup;
  async function simulate(op: Record<string, unknown>) {
    try {
      await control({ account: state.account, ...op });
      setSimStatus("Scenario applied.");
    } catch {
      setSimStatus("Scenario control failed.");
    }
  }
  function send() {
    if (!draft.trim() || !state.selected || !active) return;
    setDrafts((d) => ({ ...d, [draftKey]: "" }));
    void session.send(draft);
  }
  function select(id: string) {
    setMobile(true);
    setInChat("");
    void session.select(id);
  }
  const threads = state.threads.filter(
    (t) =>
      name(t.id).toLowerCase().includes(query.toLowerCase()) &&
      (filter !== "unread" || !!t.unreadCount) &&
      (filter !== "groups" || t.kind === "group"),
  );
  return (
    <div className={`shell ${mobile ? "mobile-chat" : ""}`}>
      <nav className="rail" aria-label="Main navigation">
        <div className="brand" title="AgentWechat">
          <Logo />
        </div>
        <button
          className={`nav-btn ${section === "chats" ? "active" : ""}`}
          aria-label="Chats"
          onClick={() => {
            setSection("chats");
            setMobile(false);
          }}
        >
          <Icon name="chat" />
        </button>
        <button
          className={`nav-btn ${section === "contacts" ? "active" : ""}`}
          aria-label="Contacts"
          onClick={() => {
            setSection("contacts");
            setMobile(false);
          }}
        >
          <Icon name="people" />
        </button>
        <div className="rail-spacer" />
        <button
          className="nav-btn"
          aria-label="Settings"
          onClick={() => setSettings(true)}
        >
          <Icon name="settings" />
        </button>
        <div className="rail-rule" />
        <button
          className="nav-btn account-btn"
          aria-label="Switch account"
          onClick={() => setAccountMenu(!accountMenu)}
        >
          <Avatar name={account?.alias || state.account || "?"} />
        </button>
      </nav>
      <aside className="sidebar">
        <div className="side-head">
          <div className="eyebrow">{account?.alias || "AgentWechat"}</div>
          <div className="title-row">
            <h1>{section === "chats" ? "Messages" : "Contacts"}</h1>
            <Button
              icon="plus"
              label="New conversation"
              onClick={() => {
                setNewChat(true);
              }}
            />
          </div>
          <label className="search">
            <Icon name="search" />
            <input
              aria-label={
                section === "chats" ? "Search conversations" : "Search contacts"
              }
              placeholder={
                section === "chats" ? "Search conversations" : "Find a person"
              }
              value={query}
              onChange={(e) => setQuery(e.target.value)}
            />
          </label>
          {section === "chats" && (
            <div className="filters">
              {["all", "unread", "groups"].map((f) => (
                <button
                  key={f}
                  className={`pill ${filter === f ? "active" : ""}`}
                  onClick={() => setFilter(f)}
                >
                  {f[0].toUpperCase() + f.slice(1)}
                </button>
              ))}
            </div>
          )}
        </div>
        <div className="list-label">
          {section === "chats" ? "CONVERSATIONS" : "CONTACTS"}
        </div>
        <div className="chat-list">
          {section === "chats"
            ? threads.map((t) => (
                <button
                  key={t.id}
                  className={`chat-row ${
                    state.selected === t.id ? "selected" : ""
                  }`}
                  onClick={() => select(t.id)}
                >
                  <ConversationAvatar
                    connection={connection}
                    account={state.account!}
                    username={t.id}
                    name={name(t.id)}
                    members={t.avatarMembers}
                  />
                  <div className="chat-copy">
                    <div className="chat-top">
                      <span className="chat-name">{name(t.id)}</span>
                    </div>
                    <div className="preview-row">
                      <span className="preview" title={threadPreview(t)}>
                        {threadPreview(t)}
                      </span>
                      {!!t.unreadCount && (
                        <span className="count">{t.unreadCount}</span>
                      )}
                    </div>
                  </div>
                </button>
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
                    onClick={() => {
                      select(c.id);
                      setSection("chats");
                    }}
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
                {query ? "No matches." : "No conversations yet."}
              </p>
            )}
        </div>
        <div className="side-footer">
          <span>
            <i className={`status-dot ${!state.connected ? "off" : ""}`} />
            {state.connected
              ? "agent-wechat hub connected"
              : "Connecting to agent-wechat hub…"}
          </span>
          {simulator && <span className="sim-label">SIMULATOR</span>}
        </div>
      </aside>
      <main className="workspace">
        <header className="chat-header">
          <Button
            icon="back"
            label="Back to conversations"
            className="back-btn"
            onClick={() => setMobile(false)}
          />
          {state.selected && state.account && (
            <ConversationAvatar
              connection={connection}
              account={state.account}
              username={state.selected}
              name={selectedName}
              downloadMissing
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
                ? "Notes to yourself"
                : state.selected
                  ? state.selected.endsWith("@chatroom")
                    ? "Group conversation"
                    : "WeChat conversation"
                  : "Choose a conversation"}
            </p>
          </div>
          <div className="header-actions">
            <Button
              icon="search"
              label="Find in conversation"
              onClick={() => {
                setFind(!find);
                setInChat("");
              }}
            />
            <Button
              icon="panel"
              label="Conversation details"
              onClick={() => setDetails(!details)}
            />
          </div>
          {find && (
            <label className="search conversation-search">
              <input
                aria-label="Find in this chat"
                placeholder="Find in this chat…"
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
              Refresh
            </button>
          </div>
        )}
        {leftGroup && (
          <div className="banner">
            You are no longer in this group. Cached history is still available.
          </div>
        )}
        {account?.authenticated === false && (
          <div className="banner">
            Sign in to WeChat again to send messages.
            <button onClick={switchAccount}>Sign in</button>
          </div>
        )}
        {simulator && (
          <details className="simulator-tools">
            <summary>Simulator controls</summary>
            <div className="sim-controls">
              <input
                aria-label="Synthetic incoming text"
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
                Receive message
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
                Send from phone
              </button>
              <button
                onClick={() =>
                  void simulate({
                    op: "advertise_image",
                    peer: state.selected || "sim_mei",
                  })
                }
              >
                Unavailable image
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
                  Receive {kind}
                </button>
              ))}
              <button
                onClick={() =>
                  void simulate({ op: "send_fault", fault: "reject" })
                }
              >
                Reject next send
              </button>
              <button
                onClick={() =>
                  void simulate({
                    op: "send_fault",
                    fault: "lost_acknowledgment",
                  })
                }
              >
                Lose next acknowledgment
              </button>
            </div>
            <p role="status">
              {simStatus ||
                "Synthetic upstream only. No official WeChat connection."}
            </p>
          </details>
        )}
        <div className="body-row">
          <section className="conversation" aria-label="Conversation">
            <ConversationScroll
              key={`${state.account}:${state.selected}:${!!(
                find && inChat.trim()
              )}`}
              searching={!!(find && inChat.trim())}
              history={{
                hasMore: !!state.nextMessages,
                loading: state.loading,
                load: () => void session.older(),
              }}
              leading={
                state.messages[0] ? (
                  <MessageDate value={state.messages[0].createdAt} />
                ) : null
              }
            >
              {find && inChat.trim() && state.account && state.selected ? (
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
                    .filter((s) => s.threadId === state.selected)
                    .map((s) => (
                      <ConversationItem
                        key={s.key}
                        messageId={`pending:${s.key}`}
                      >
                        <Pending
                          send={s}
                          check={() => void session.checkSend(s.key)}
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
                        <p className="empty-chat">Start the conversation.</p>
                      )}
                  </ConversationItem>
                </>
              )}
            </ConversationScroll>
            <div className="composer-wrap">
              <div className="composer">
                <div className="composer-tools">
                  {state.account && (
                    <AttachmentPicker
                      key={state.account}
                      connection={connection}
                      account={state.account}
                      threadId={state.selected}
                      recipient={selectedName}
                      enabled={
                        active &&
                        Array.isArray(state.capabilities.mediaKinds) &&
                        state.capabilities.mediaKinds.length > 0
                      }
                    />
                  )}
                  {simulator && (
                    <span className="field-help">
                      Messages and files stay in the simulator.
                    </span>
                  )}
                </div>
                <textarea
                  aria-label="Message"
                  rows={2}
                  placeholder={
                    state.selected
                      ? `Message ${selectedName}…`
                      : "Choose a conversation"
                  }
                  value={draft}
                  disabled={!state.selected || !active}
                  onChange={(e) =>
                    setDrafts((d) => ({ ...d, [draftKey]: e.target.value }))
                  }
                  onKeyDown={(e) => {
                    if (
                      e.key === "Enter" &&
                      !e.shiftKey &&
                      !e.nativeEvent.isComposing
                    ) {
                      e.preventDefault();
                      send();
                    }
                  }}
                />
                <div className="composer-bottom">
                  <span className="composer-hint">
                    Enter to send · Shift + Enter for a new line
                  </span>
                  <button
                    className="send-btn"
                    disabled={!state.selected || !draft.trim() || !active}
                    onClick={send}
                  >
                    Send <Icon name="send" />
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
      </main>
      {accountMenu && (
        <div className="account-menu" role="dialog" aria-label="Switch account">
          <div className="eyebrow">WeChat accounts</div>
          {state.accounts.map((a) => (
            <button
              key={a.id}
              className={`account-option ${
                a.id === state.account ? "selected" : ""
              }`}
              onClick={() => {
                setAccountMenu(false);
                setMobile(false);
                void (a.authenticated
                  ? session.activate(a.id)
                  : Promise.resolve(switchAccount()));
              }}
            >
              <Avatar name={a.alias || a.id} />
              <span>
                <strong>{a.alias || a.id}</strong>
                <small>
                  {a.authenticated ? "Signed in" : "Sign-in needed"}
                </small>
              </span>
            </button>
          ))}
          <button
            className="account-option"
            onClick={() => {
              setAccountMenu(false);
              addAccount();
            }}
          >
            <Icon name="plus" /> Add WeChat account
          </button>
          <button className="account-option" onClick={switchAccount}>
            Choose an account
          </button>
        </div>
      )}
      {newChat && state.account && (
        <NewChat
          key={state.account}
          connection={connection}
          account={state.account}
          close={() => setNewChat(false)}
          open={(id) => {
            setSection("chats");
            setNewChat(false);
            select(id);
          }}
        />
      )}
      {settings && (
        <div className="overlay-backdrop" onClick={() => setSettings(false)}>
          <section
            className="modal"
            role="dialog"
            aria-modal="true"
            aria-label="Settings"
            onClick={(e) => e.stopPropagation()}
          >
            <div className="modal-head">
              <h2>Settings</h2>
              <Button
                icon="close"
                label="Close settings"
                onClick={() => setSettings(false)}
              />
            </div>
            <div className="modal-body">
              <label className="setting">
                Appearance
                <select
                  value={theme}
                  onChange={(e) => setTheme(e.target.value)}
                >
                  <option value="system">System</option>
                  <option value="light">Light</option>
                  <option value="dark">Dark</option>
                </select>
              </label>
              <div className="setting">
                <span>
                  agent-wechat hub<small>{connection.endpoint}</small>
                </span>
                {simulator ? "Simulator" : "Connected"}
              </div>
              <button className="secondary" onClick={switchAccount}>
                Manage accounts
              </button>
              {account && (
                <AccountSettings
                  key={account.id}
                  connection={connection}
                  account={account}
                  signedOut={() => {
                    setSettings(false);
                    switchAccount();
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
      aria-label="Conversation search results"
      className="history-search"
    >
      <div className="history-search-status">
        <span role="status">
          {!current || state.loading
            ? "Searching history on this agent-wechat hub…"
            : `${state.items.length}${state.nextCursor ? "+" : ""} ${
                state.items.length === 1 && !state.nextCursor
                  ? "match"
                  : "matches"
              } in this conversation`}
        </span>
        <button onClick={close}>Back to chat</button>
      </div>
      {current && state.error && (
        <ErrorNotice>
          {state.error}{" "}
          <button onClick={() => void search.search(query)}>
            Retry search
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
          Load more matches
        </button>
      )}
      {current && !state.loading && !state.error && !state.items.length && (
        <p className="empty-chat">
          No matches in the history available on this agent-wechat hub.
        </p>
      )}
    </section>
  );
}
function Pending({ send, check }: { send: PendingSend; check: () => void }) {
  const state = send.intent?.state;
  const uncertain = send.uncertain || state === "needs_review";
  const label = uncertain
    ? "Couldn't confirm send"
    : state === "rejected" || state === "failed"
      ? "Send rejected"
      : state === "cancelled"
        ? "Send cancelled"
        : state === "accepted"
          ? "Accepted · loading message…"
          : "Queued…";
  return (
    <article
      className="message own"
      data-send-state={state || (send.uncertain ? "uncertain" : "submitting")}
    >
      <Avatar name="You" />
      <div className="message-body">
        <div className="bubble">
          <p>{send.text}</p>
        </div>
        <div
          className={`delivery ${
            uncertain || state === "rejected" || state === "failed"
              ? "warn"
              : ""
          }`}
          role="status"
        >
          {label} <button onClick={check}>Check status</button>
        </div>
        {uncertain && (
          <small className="send-explanation">
            Check the conversation before sending again.
          </small>
        )}
      </div>
    </article>
  );
}
function MessageDate({ value }: { value: string | null }) {
  return (
    <div className="date-label">
      {date(value).toLocaleDateString([], { month: "long", day: "numeric" })}
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
  const own = m.direction === "outgoing";
  const presentation = messagePresentation(m);
  return (
    <article
      className={`message ${own ? "own" : ""}`}
      data-message-id={m.id}
      data-direction={m.direction}
    >
      <HubAvatar
        connection={connection}
        account={m.accountId}
        username={m.senderId || (!own ? m.threadId : null)}
        name={own ? "You" : name(m.senderId || m.threadId)}
        downloadMissing={m.threadId.endsWith("@chatroom")}
      />
      <div className="message-body">
        <div className="message-meta">
          <b>{own ? "You" : name(m.senderId || m.threadId)}</b>
          <time dateTime={date(m.createdAt).toISOString()}>
            {time(m.createdAt)}
          </time>
        </div>
        {presentation === "recalled" ? (
          <div className="bubble recalled">Message recalled</div>
        ) : presentation === "text" ? (
          <div className="bubble">
            <p>{m.body || (m.type >= 10000 ? "System message" : "")}</p>
          </div>
        ) : presentation === "reply" || presentation === "forwarded" ? (
          <div className={`bubble ${presentation}-message`}>
            <div className="message-context">
              <Icon name={presentation === "reply" ? "reply" : "chat"} />
              {presentation === "reply" ? "Reply" : "Forwarded messages"}
            </div>
            <p>
              {m.body ||
                (presentation === "reply"
                  ? "Reply text is unavailable."
                  : "Forwarded content is unavailable.")}
            </p>
          </div>
        ) : presentation === "link" && m.link ? (
          <div className="bubble">
            <strong>{m.link.title || "Shared link"}</strong>
            <p>{m.link.description}</p>
            {m.link.url && /^https?:\/\//i.test(m.link.url) && (
              <a href={m.link.url} target="_blank" rel="noreferrer">
                Open link
              </a>
            )}
          </div>
        ) : presentation === "media" ? (
          <MediaCard message={m} connection={connection} />
        ) : (
          <div className="bubble unsupported-message">
            <p>This WeChat message type isn’t supported yet.</p>
          </div>
        )}
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
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false),
    [notice, setNotice] = useState("");
  const state = connection.session.snapshot();
  return (
    <aside className="detail-panel" aria-label="Conversation details">
      <div className="detail-profile">
        <ConversationAvatar
          connection={connection}
          account={state.account!}
          username={threadId}
          name={name}
          downloadMissing
          members={
            state.threads.find((thread) => thread.id === threadId)
              ?.avatarMembers
          }
        />
        <h3>{name}</h3>
        <p>
          {threadId === "filehelper"
            ? "Notes to yourself"
            : threadId.endsWith("@chatroom")
              ? "Group conversation"
              : "WeChat contact"}
        </p>
      </div>
      <div className="detail-section">
        <div className="section-head">Conversation</div>
        <p className="peer-id">{threadId}</p>
        <p className="field-help">
          History contains the messages available on this agent-wechat hub.
        </p>
        <div className="detail-actions">
          <button
            disabled={
              busy ||
              !state.threads.find((thread) => thread.id === threadId)
                ?.unreadCount
            }
            onClick={() => {
              setBusy(true);
              setError("");
              void connection.session
                .markRead(threadId)
                .then((result) => {
                  setNotice(
                    (result as { phoneReceiptSent?: boolean }).phoneReceiptSent
                      ? "Marked as read."
                      : "Marked read on this agent-wechat hub. Phone confirmation is unavailable.",
                  );
                })
                .catch((error) => setError(readableError(error)))
                .finally(() => setBusy(false));
            }}
          >
            Mark as read
          </button>
          <button
            disabled={busy}
            onClick={() => {
              if (!state.account) return;
              setBusy(true);
              setError("");
              void connection.avatars
                .refresh(state.account, threadId)
                .then(() => setNotice("Avatar refreshed."))
                .catch((error) => setError(readableError(error)))
                .finally(() => setBusy(false));
            }}
          >
            Refresh avatar
          </button>
        </div>
        {notice && (
          <p className="account-notice" role="status">
            {notice}
          </p>
        )}
      </div>
      {state.account && threadId.endsWith("@chatroom") && (
        <GroupSettings
          connection={connection}
          account={state.account}
          groupId={threadId}
        />
      )}
      {error && <p>{error}</p>}
    </aside>
  );
}

createRoot(document.getElementById("root")!).render(<App />);
