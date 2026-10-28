import { useCallback, useEffect, useRef, useState, type KeyboardEvent } from "react";
import { api, events, errorMessage, type ConnectionInfo, type PageId } from "./api";
import { useEvent } from "./hooks";
import { useT, type MessageKey } from "./i18n";
import { Logo } from "./components/Logo";
import { Offline } from "./components/Offline";
import { ChatPage } from "./pages/Chat";
import { SearchPage } from "./pages/Search";
import { KnowledgePage } from "./pages/Knowledge";
import { AppsPage } from "./pages/Apps";
import { SourcesPage } from "./pages/Sources";
import { ModelsPage } from "./pages/Models";
import { PrivacyPage } from "./pages/Privacy";
import { StatusPage } from "./pages/Status";
import { DoubleTapPage } from "./pages/DoubleTap";
import { Onboarding } from "./onboarding/Onboarding";
import { SidebarSettings } from "./onboarding/SidebarSettings";
import { shouldShowOnboarding } from "./onboarding/machine";

const PAGES: { id: PageId; label: MessageKey }[] = [
  { id: "chat", label: "app.nav.chat" },
  { id: "search", label: "app.nav.search" },
  { id: "knowledge", label: "app.nav.knowledge" },
  { id: "sources", label: "app.nav.sources" },
  { id: "apps", label: "app.nav.apps" },
  { id: "models", label: "app.nav.models" },
  { id: "doubletap", label: "app.nav.doubletap" },
  { id: "privacy", label: "app.nav.privacy" },
  { id: "status", label: "app.nav.status" },
];

const PAGE_KEY = "nugget.page";

function initialPage(): PageId {
  try {
    const saved = localStorage.getItem(PAGE_KEY);
    if (PAGES.some((p) => p.id === saved)) return saved as PageId;
  } catch {
    /* storage unavailable */
  }
  return "chat";
}

/** `null` while the first connection attempt is in flight. */
type Connection = ConnectionInfo | null;

export function App() {
  const t = useT();
  const [page, setPage] = useState<PageId>(initialPage);
  const [conn, setConn] = useState<Connection>(null);
  const [onboarding, setOnboarding] = useState(shouldShowOnboarding);
  const endOnboarding = useCallback(() => setOnboarding(false), []);
  const mainRef = useRef<HTMLElement>(null);

  const go = useCallback((id: PageId) => {
    setPage(id);
    try {
      localStorage.setItem(PAGE_KEY, id);
    } catch {
      /* ignore */
    }
  }, []);

  const refreshConnection = useCallback(async () => {
    try {
      setConn(await api.connection());
    } catch (e) {
      setConn({ connected: false, socket: "", error: errorMessage(e) });
    }
  }, []);

  useEffect(() => {
    void refreshConnection();
  }, [refreshConnection]);

  // While offline, poll quietly so a daemon started elsewhere (service, terminal) is picked up.
  useEffect(() => {
    if (conn?.connected !== false) return;
    const t = window.setInterval(() => void refreshConnection(), 5000);
    return () => window.clearInterval(t);
  }, [conn?.connected, refreshConnection]);

  useEvent(events.onConnection, setConn);
  useEvent(events.onNavigate, (p) => {
    go(p);
    mainRef.current?.focus();
  });

  // Arrow keys move between nav items (roving focus), as in a tab list.
  const onNavKey = (e: KeyboardEvent<HTMLElement>) => {
    if (e.key !== "ArrowDown" && e.key !== "ArrowUp") return;
    const buttons = Array.from(e.currentTarget.querySelectorAll<HTMLButtonElement>("button"));
    const i = buttons.indexOf(document.activeElement as HTMLButtonElement);
    if (i < 0) return;
    e.preventDefault();
    const next = buttons[(i + (e.key === "ArrowDown" ? 1 : buttons.length - 1)) % buttons.length];
    next?.focus();
  };

  const current = PAGES.find((p) => p.id === page) ?? PAGES[0]!;

  if (onboarding) return <Onboarding connected={conn?.connected === true} onExit={endOnboarding} />;

  return (
    <div className="shell">
      <a className="skip-link" href="#main">
        {t("app.skipToContent")}
      </a>
      <aside className="sidebar">
        <div className="brand">
          <Logo />
          <span>{t("app.name")}</span>
        </div>
        <nav aria-label={t("app.navLabel")} onKeyDown={onNavKey}>
          <ul>
            {PAGES.map((p) => (
              <li key={p.id}>
                <button
                  type="button"
                  className="nav-item"
                  aria-current={p.id === page ? "page" : undefined}
                  onClick={() => go(p.id)}
                >
                  {t(p.label)}
                </button>
              </li>
            ))}
          </ul>
        </nav>
        <ConnectionPill conn={conn} />
        <SidebarSettings onSetup={() => setOnboarding(true)} />
      </aside>
      <main id="main" ref={mainRef} tabIndex={-1} aria-label={t(current.label)}>
        {conn === null ? (
          <div className="center muted">{t("app.connecting")}</div>
        ) : !conn.connected ? (
          <Offline conn={conn} onConnected={setConn} onRetry={refreshConnection} />
        ) : (
          <Page id={page} />
        )}
      </main>
    </div>
  );
}

function Page({ id }: { id: PageId }) {
  switch (id) {
    case "chat":
      return <ChatPage />;
    case "search":
      return <SearchPage />;
    case "knowledge":
      return <KnowledgePage />;
    case "apps":
      return <AppsPage />;
    case "sources":
      return <SourcesPage />;
    case "models":
      return <ModelsPage />;
    case "privacy":
      return <PrivacyPage />;
    case "status":
      return <StatusPage />;
    case "doubletap":
      return <DoubleTapPage />;
  }
}

function ConnectionPill({ conn }: { conn: Connection }) {
  const t = useT();
  const state = conn === null ? "pending" : conn.connected ? "up" : "down";
  const label = t(`app.conn.${state}`);
  return (
    <div className={`conn conn-${state}`} role="status" title={conn?.socket || undefined}>
      <span className="dot" aria-hidden="true" />
      {label}
    </div>
  );
}
