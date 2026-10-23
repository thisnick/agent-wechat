import {
  Settings,
  ImageOff,
  Undo2,
  ZoomIn,
  ZoomOut,
  Focus,
} from "lucide-react";
import type { ReactNode } from "react";
const icons: Record<string, string> = {
  chat: "M21 11.5a8.4 8.4 0 0 1-9 8.5 10 10 0 0 1-4-.8L3 21l1.6-4.5A8.3 8.3 0 0 1 3 11.5 8.4 8.4 0 0 1 12 3a8.4 8.4 0 0 1 9 8.5ZM8 11h.01M12 11h.01M16 11h.01",
  people:
    "M16 21v-2a4 4 0 0 0-4-4H6a4 4 0 0 0-4 4v2M16 4a4 4 0 0 1 0 8M22 21v-2a4 4 0 0 0-3-3.87M13 7a4 4 0 1 1-8 0 4 4 0 0 1 8 0Z",
  search: "m21 21-5-5M18 10a8 8 0 1 1-16 0 8 8 0 0 1 16 0Z",
  plus: "M12 5v14M5 12h14",
  more: "M5 12h.01M12 12h.01M19 12h.01",
  panel: "M3 4h18v16H3zM15 4v16",
  bell: "M18 8a6 6 0 0 0-12 0c0 7-3 7-3 9h18c0-2-3-2-3-9M10 21h4",
  check: "m5 12 4 4L19 6",
  chevron: "m9 5 7 7-7 7",
  down: "m6 9 6 6 6-6",
  back: "m14 6-6 6 6 6",
  close: "m6 6 12 12M6 18 18 6",
  attach: "m8 12 6-6a3 3 0 0 1 4 4l-8 8a5 5 0 0 1-7-7l9-9",
  smile:
    "M21 12a9 9 0 1 1-18 0 9 9 0 0 1 18 0ZM8 9h.01M16 9h.01M8 14c2 3 6 3 8 0",
  image: "M3 3h18v18H3zM3 16l5-5 5 5 3-3 5 5M15 7h.01",
  send: "m22 2-7 20-4-9-9-4 20-7ZM11 13 22 2",
  file: "M14 2H4v20h16V8l-6-6ZM14 2v6h6M8 13h8M8 17h5",
  download: "M12 3v12m-5-5 5 5 5-5M4 17v4h16v-4",
  reply: "m9 5-6 6 6 6M3 11h10a8 8 0 0 1 8 8",
  play: "m8 4 12 8-12 8V4Z",
  pause: "M8 4v16M16 4v16",
  logout: "M9 4H4v16h5M9 12h12m-5-5 5 5-5 5",
  leaf: "M20 3C7 1 2 8 6 15c7 5 15-1 14-12ZM4 21 15 9",
  pin: "m16 3 5 5-4 2-3 6-2-2-6 6M9 3l12 12M3 9l6-6",
  sun: "M12 3V1M12 23v-2M3 12H1M23 12h-2M5 5 3 3M21 21l-2-2M3 21l2-2M19 5l2-2M17 12a5 5 0 1 1-10 0 5 5 0 0 1 10 0Z",
  server: "M3 3h18v7H3zM3 14h18v7H3zM7 6h.01M7 17h.01",
  help: "M12 17h.01M9 9a3 3 0 1 1 4 3v2M21 12a9 9 0 1 1-18 0 9 9 0 0 1 18 0Z",
};

export function Icon({ name }: { name: string }) {
  const component = (
    {
      recall: Undo2,
      "zoom-in": ZoomIn,
      "zoom-out": ZoomOut,
      "reset-zoom": Focus,
    } as Record<string, typeof Undo2>
  )[name];
  if (component) {
    const Component = component;
    return <Component className="icon" aria-hidden="true" strokeWidth={1.7} />;
  }
  if (name === "image-off")
    return <ImageOff className="icon" aria-hidden="true" strokeWidth={1.7} />;
  if (name === "settings")
    return <Settings className="icon" aria-hidden="true" strokeWidth={1.7} />;
  return (
    <svg className="icon" aria-hidden="true" viewBox="0 0 24 24">
      <path d={icons[name] ?? icons.chat} />
    </svg>
  );
}
export function Logo() {
  return (
    <svg viewBox="0 0 32 32" aria-hidden="true">
      <path
        d="M6 7h17a4 4 0 0 1 4 4v9a4 4 0 0 1-4 4h-9l-6 4v-4H6a4 4 0 0 1-4-4v-9a4 4 0 0 1 4-4Z"
        fill="none"
        stroke="currentColor"
        strokeWidth="2.3"
        strokeLinejoin="round"
      />
      <path
        d="m10 13-3 3 3 3m8-6 3 3-3 3m-3-7-2 9"
        fill="none"
        stroke="currentColor"
        strokeWidth="1.8"
        strokeLinecap="round"
        strokeLinejoin="round"
      />
    </svg>
  );
}
export function Avatar({
  name,
  src,
  ref,
}: {
  name: string;
  src?: string;
  ref?: import("react").Ref<HTMLSpanElement>;
}) {
  return (
    <span className="avatar" ref={ref}>
      {src ? (
        <img src={src} alt="" />
      ) : (
        name
          .replace(/^sim_/, "")
          .split(/[ _-]+/)
          .map((n) => n[0])
          .join("")
          .slice(0, 2)
          .toUpperCase()
      )}
    </span>
  );
}
export function Button({
  icon,
  label,
  onClick,
  className = "",
  disabled = false,
}: {
  icon: string;
  label: string;
  onClick: () => void;
  className?: string;
  disabled?: boolean;
}) {
  return (
    <button
      type="button"
      className={`icon-btn ${className}`}
      aria-label={label}
      title={label}
      onClick={onClick}
      disabled={disabled}
    >
      <Icon name={icon} />
    </button>
  );
}
function AppIllustration() {
  return (
    <svg
      className="app-illustration"
      viewBox="0 0 460 282"
      width="460"
      height="282"
      aria-hidden="true"
    >
      <defs>
        <clipPath id="illustration-frame">
          <rect width="460" height="282" rx="12" />
        </clipPath>
      </defs>
      <g clipPath="url(#illustration-frame)">
        <rect width="460" height="282" fill="#343537" />
        <path d="M0 30h460" stroke="#ffffff10" />
        {[15, 26, 37].map((x) => (
          <circle key={x} cx={x} cy="15" r="3" fill="#666865" />
        ))}
        <rect y="31" width="40" height="251" fill="#2c2d2f" />
        <g
          fill="none"
          stroke="#a5a7a4"
          strokeWidth="1.5"
          strokeLinecap="round"
          strokeLinejoin="round"
        >
          <path d="M12 46h16v11H20l-5 4v-4h-3z" stroke="#e8e8e3" />
          <path d="m17 49-2 2 2 2m5-4 2 2-2 2" />
          <path d="M13 85a7 7 0 1 1 3 6l-4 2 1-5z" />
          <circle cx="20" cy="119" r="4" />
          <path d="M13 134v-4a7 7 0 0 1 14 0v4" />
        </g>
        <rect x="9" y="248" width="22" height="22" rx="7" fill="#666861" />
        <text x="20" y="262" textAnchor="middle" fill="#e4e5df" fontSize="8">
          AW
        </text>
        <text x="53" y="52" fill="#e6e7e1" fontSize="10" fontWeight="550">
          Chats
        </text>
        <path d="M145 45v9m-4-4h8" stroke="#a3a5a0" />
        <path d="M164 31v251" stroke="#ffffff0c" />
        {["MC", "DP", "SL", "LZ"].map((label, i) => (
          <g key={label} transform={`translate(47 ${66 + i * 44})`}>
            {i === 0 && <rect width="110" height="41" rx="6" fill="#484a47" />}
            <rect x="6" y="9" width="24" height="24" rx="7" fill="#62645e" />
            <text x="18" y="24" textAnchor="middle" fontSize="8" fill="#e3e4de">
              {label}
            </text>
            <rect x="38" y="15" width="38" height="4" rx="2" fill="#a2a49d" />
            <rect x="38" y="25" width="46" height="3" rx="1.5" fill="#666861" />
          </g>
        ))}
        <rect x="165" y="31" width="295" height="251" fill="#3b3c3e" />
        <rect x="175" y="42" width="20" height="20" rx="6" fill="#62645e" />
        <text x="185" y="55" textAnchor="middle" fontSize="7" fill="#e3e4de">
          MC
        </text>
        <rect x="203" y="49" width="48" height="4" rx="2" fill="#c4c6bd" />
        {[429, 435, 441].map((x) => (
          <circle key={x} cx={x} cy="51" r="1.2" fill="#a4a69d" />
        ))}
        <path d="M165 72h295" stroke="#ffffff0c" />
        <rect x="177" y="86" width="84" height="37" rx="7" fill="#52544e" />
        <path
          d="M188 98h61m-61 9h40"
          stroke="#a4a79a"
          strokeWidth="3"
          strokeLinecap="round"
        />
        <rect
          x="364"
          y="133"
          width="84"
          height="27"
          rx="7"
          fill="var(--accent)"
        />
        <path
          d="M375 146h61"
          stroke="#f7dfcc"
          strokeWidth="3"
          strokeLinecap="round"
        />
        <rect x="177" y="170" width="65" height="45" rx="7" fill="#55574f" />
        <g
          transform="translate(199 182)"
          fill="none"
          stroke="#b4b7a8"
          strokeWidth="1.5"
        >
          <rect width="20" height="17" rx="2" />
          <circle cx="6" cy="5" r="1.5" />
          <path d="m2 15 5-5 3 3 4-6 4 8" />
        </g>
        <rect
          x="398"
          y="225"
          width="50"
          height="21"
          rx="7"
          fill="var(--accent)"
        />
        <path
          d="M409 236h28"
          stroke="#f7dfcc"
          strokeWidth="3"
          strokeLinecap="round"
        />
        <path d="M175 253h274" stroke="#ffffff10" />
        <path
          d="m184 261-4 4a3 3 0 0 0 4 4l5-5a2 2 0 0 0-3-3l-4 4"
          fill="none"
          stroke="#a8ab9e"
          strokeWidth="1.2"
        />
        <rect x="196" y="264" width="50" height="4" rx="2" fill="#666960" />
        <rect
          x="424"
          y="258"
          width="24"
          height="18"
          rx="4"
          fill="var(--accent)"
        />
        <path
          d="m430 266 11-4-3 10-3-5z"
          fill="none"
          stroke="#fff"
          strokeWidth="1"
        />
      </g>
      <rect
        x=".5"
        y=".5"
        width="459"
        height="281"
        rx="11.5"
        fill="none"
        stroke="#ffffff1c"
      />
    </svg>
  );
}
export function Story() {
  return (
    <aside className="onboarding-story">
      <a
        className="wordmark"
        href="https://github.com/thisnick/agent-wechat"
        target="_blank"
        rel="noreferrer"
      >
        <span className="brand-symbol">
          <Logo />
        </span>
        <span>AgentWechat</span>
      </a>
      <div className="story-content">
        <p className="product-description">
          Chat, share files and manage your WeChat accounts alongside your AI
          agents.
        </p>
        <div className="illustration-viewport">
          <AppIllustration />
        </div>
      </div>
      <nav className="project-links" aria-label="Project links">
        {[
          ["GitHub", "https://github.com/thisnick/agent-wechat"],
          ["Docs", "https://thisnick.github.io/agent-wechat/"],
        ].map(([label, url]) => (
          <a key={label} href={url} target="_blank" rel="noreferrer">
            {label}
            <svg viewBox="0 0 16 16" aria-hidden="true">
              <path d="M5 11 11 5M5 5h6v6" />
            </svg>
          </a>
        ))}
      </nav>
    </aside>
  );
}
export function Welcome({
  children,
  simulator,
}: {
  children: ReactNode;
  simulator: boolean;
}) {
  return (
    <main className="onboarding-shell">
      <Story />
      <section className="onboarding-main">
        <header className="onboarding-top">
          {simulator && <span className="demo-tag">Simulator</span>}
        </header>
        <div className="onboarding-center">{children}</div>
        <footer className="onboarding-footer">
          <span className="connection-location">
            <i />
            {simulator
              ? "agent-wechat hub: this device · synthetic accounts"
              : "agent-wechat hub"}
          </span>
        </footer>
      </section>
    </main>
  );
}
