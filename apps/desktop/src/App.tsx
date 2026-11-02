import { useCallback, useEffect, useMemo, useRef, useState, type ComponentType, type KeyboardEvent } from "react";
import { api, chatHistory, events, errorMessage, proactiveApi, proactiveEvents, type ConnectionInfo, type PageId, type StoredConversation } from "./api";
import { useEvent } from "./hooks";
import { requestChatOpen, requestNewChat, requestSearch, requestSearchState, usePendingPage } from "./hooks.nav";
import { comboAria, comboLabel, digitOf, hasMod, isComposing, isTypingTarget, letterOf, useKeydown, type Combo } from "./hooks.shortcuts";
import { useT, type MessageKey } from "./i18n";
import { Logo } from "./components/Logo";
import { Icon, type IconName } from "./components/Icon";
import { cloudApi } from "./cloud";
import { Offline } from "./components/Offline";
import { RecoveryBanner } from "./components/BackupSettings";
import { CommandPalette, type PaletteItem } from "./components/CommandPalette";
import { ShortcutsHelp, type ShortcutRow } from "./components/ShortcutsHelp";
import { SavedSearchNav } from "./components/search/SavedSearches";
import { useSavedSearches, type SavedSearch } from "./savedSearches";
import { ChatPage } from "./pages/Chat";
import { SearchPage } from "./pages/Search";
import { KnowledgePage } from "./pages/Knowledge";
import { AppsPage } from "./pages/Apps";
import { PluginsPage } from "./pages/Plugins";
import { AgentPage } from "./pages/Agent";
import { SafetyPage } from "./pages/Safety";
import { SourcesPage } from "./pages/Sources";
import { ModelsPage } from "./pages/Models";
import { PrivacyPage } from "./pages/Privacy";
import { StatusPage } from "./pages/Status";
import { HomePage, NeedsPage } from "./pages/Today";
import { startProactiveNotifications } from "./proactiveNotifications";
import { DoubleTapPage } from "./pages/DoubleTap";
import { CloudPage } from "./pages/Cloud";
import { Onboarding } from "./onboarding/Onboarding";
import { SidebarSettings } from "./onboarding/SidebarSettings";
import { restartOnboarding, shouldShowOnboarding } from "./onboarding/machine";
import { migrateTriggerOnce } from "./onboarding/trigger";
import { DeepLinks } from "./platform/DeepLinks";
import { QuitPrompt } from "./platform/QuitPrompt";
import { useShellLabels } from "./platform/shellLabels";
import { useQuickAskHandover } from "./quickask/handover";

export interface PageDef {
  id: PageId;
  /** Nav label (also the page's landmark name and its palette entry). */
  label: MessageKey;
  Component: ComponentType;
  /** Extra words the command palette matches, e.g. synonyms ("ask, conversation"). */
  keywords?: MessageKey;
  /** A ⌘/Ctrl+<key> shortcut besides the automatic ⌘/Ctrl+1…9 by position. */
  shortcut?: string;
  /** Works without the local background service (Heed Cloud). */
  standalone?: boolean;
  /** Where the sidebar lists it: the four main surfaces, Settings, or nowhere (Chat). */
  place: "main" | "settings" | "hidden";
  icon?: IconName;
}

/**
 * Every page of the app. The nav, ⌘/Ctrl+1…9 (by position), the command palette and page restore
 * all come from this list. The sidebar shows the four main surfaces and recent chats; the
 * Settings pages show in its place when one of them is open (⌘, or the account button).
 */
export const PAGES: readonly PageDef[] = [
  { id: "today", label: "app.nav.today", Component: HomePage, place: "main", icon: "home", keywords: "app.keywords.today" },
  { id: "needs", label: "app.nav.needs", Component: NeedsPage, place: "main", icon: "inbox", keywords: "app.keywords.needs" },
  { id: "search", label: "app.nav.search", Component: SearchPage, keywords: "app.keywords.search", place: "main", icon: "search" },
  { id: "connections", label: "app.nav.connections", Component: CloudPage, standalone: true, place: "main", icon: "plug" },
  { id: "chat", label: "app.nav.chat", Component: ChatPage, keywords: "app.keywords.chat", place: "hidden", icon: "chat" },
  { id: "knowledge", label: "app.nav.knowledge", Component: KnowledgePage, keywords: "app.keywords.knowledge", place: "settings" },
  { id: "sources", label: "app.nav.sources", Component: SourcesPage, keywords: "app.keywords.sources", place: "settings" },
  { id: "apps", label: "app.nav.apps", Component: AppsPage, keywords: "app.keywords.apps", place: "settings" },
  { id: "plugins", label: "app.nav.plugins", Component: PluginsPage, place: "settings" },
  { id: "agent", label: "app.nav.agent", Component: AgentPage, place: "settings" },
  { id: "safety", label: "app.nav.safety", Component: SafetyPage, place: "settings" },
  { id: "models", label: "app.nav.models", Component: ModelsPage, keywords: "app.keywords.models", place: "settings" },
  { id: "doubletap", label: "app.nav.doubletap", Component: DoubleTapPage, place: "settings" },
  { id: "privacy", label: "app.nav.privacy", Component: PrivacyPage, keywords: "app.keywords.privacy", place: "settings" },
  { id: "status", label: "app.nav.status", Component: StatusPage, keywords: "app.keywords.status", shortcut: ",", place: "settings" },
];

const PAGE_IDS: readonly PageId[] = PAGES.map((p) => p.id);

const PAGE_KEY = "nugget.page";
const RECENT_CHATS = 5;
const PALETTE: Combo = { mod: true, key: "K" };

function initialPage(): PageId {
  try {
    const saved = localStorage.getItem(PAGE_KEY);
    if (PAGES.some((p) => p.id === saved)) return saved as PageId;
  } catch {
    /* storage unavailable */
  }
  return "today";
}

/** The shortcut for a page: ⌘/Ctrl+<n> for the first nine, else its own, if any. */
function pageCombo(p: PageDef, i: number): Combo | null {
  if (i < 9) return { mod: true, key: String(i + 1) };
  return p.shortcut ? { mod: true, key: p.shortcut } : null;
}

/** `null` while the first connection attempt is in flight. */
type Connection = ConnectionInfo | null;

export function App() {
  const t = useT();
  const [page, setPage] = useState<PageId>(initialPage);
  const [conn, setConn] = useState<Connection>(null);
  const [onboarding, setOnboarding] = useState(shouldShowOnboarding);
  const [palette, setPalette] = useState(false);
  const [help, setHelp] = useState(false);
  const endOnboarding = useCallback((to?: PageId) => {
    setOnboarding(false);
    setPage(to ?? "today");
  }, []);
  const mainRef = useRef<HTMLElement>(null);
  // Set when a shortcut or the palette navigates: focus moves to the new page once it renders.
  const focusMain = useRef(false);
  const saved = useSavedSearches();
  const recents = useRecentChats(page);
  const needsCount = useNeedsCount(conn?.connected === true, page);
  /** The main page to go back to from Settings. */
  const lastMain = useRef<PageId>("today");

  const go = useCallback((id: PageId) => {
    if (PAGES.find((p) => p.id === id)?.place !== "settings") lastMain.current = id;
    setPage(id);
    try {
      localStorage.setItem(PAGE_KEY, id);
    } catch {
      /* ignore */
    }
  }, []);

  /** Navigate and put keyboard focus at the top of the new page. */
  const jump = useCallback(
    (id: PageId) => {
      focusMain.current = true;
      go(id);
    },
    [go],
  );

  useEffect(() => {
    if (!focusMain.current || palette) return;
    focusMain.current = false;
    // A page that focused its own field on mount (Search) keeps it.
    if (!mainRef.current?.contains(document.activeElement)) mainRef.current?.focus();
  }, [page, palette]);

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

  useEffect(() => startProactiveNotifications(), []);
  useShellLabels();
  useQuickAskHandover();

  // Apply a Double Tap trigger an older onboarding only kept locally (once; never throws).
  useEffect(() => {
    if (conn?.connected) void migrateTriggerOnce();
  }, [conn?.connected]);

  usePendingPage(jump);
  useEvent(events.onConnection, setConn);
  useEvent(events.onNavigate, (p) => {
    go(p);
    mainRef.current?.focus();
  });

  // Global shortcuts. Modifier combos work even while typing; plain keys (`?`) never steal
  // text input, and nothing fires during IME composition or under another page's dialog.
  useKeydown((e) => {
    if (onboarding || e.defaultPrevented || isComposing(e)) return;
    const mod = hasMod(e) && !e.altKey;
    const foreignDialog = !palette && !help && document.querySelector("dialog[open]") !== null;
    if (foreignDialog) return;
    if (mod && !e.shiftKey && letterOf(e) === "k") {
      e.preventDefault();
      setHelp(false);
      setPalette((o) => !o);
      return;
    }
    if (palette || help) {
      if (e.key === "Escape") {
        e.preventDefault();
        setPalette(false);
        setHelp(false);
      }
      return;
    }
    if (mod) {
      const n = e.shiftKey ? null : digitOf(e);
      const target = n !== null ? PAGES[n - 1] : PAGES.find((p) => p.shortcut && p.shortcut === e.key);
      if (target) {
        e.preventDefault();
        jump(target.id);
      } else if (e.key === "/") {
        e.preventDefault();
        setHelp(true);
      }
      return;
    }
    if (e.key === "?" && !e.ctrlKey && !e.metaKey && !e.altKey && !isTypingTarget(e.target)) {
      e.preventDefault();
      setHelp(true);
    }
  });

  /** Open Search with a saved search applied (the page takes focus itself). */
  const runSaved = useCallback(
    (s: SavedSearch) => {
      requestSearchState(s.state);
      go("search");
    },
    [go],
  );

  const paletteItems = useMemo<PaletteItem[]>(() => {
    const pages = PAGES.map((p, i): PaletteItem => {
      const combo = pageCombo(p, i);
      return {
        id: `page:${p.id}`,
        label: t(p.label),
        keywords: p.keywords ? t(p.keywords) : undefined,
        hint: combo ? comboLabel(combo) : undefined,
        group: t("palette.group.page"),
        run: () => jump(p.id),
      };
    });
    const actions: PaletteItem[] = [
      {
        id: "action:new-chat",
        label: t("palette.action.newChat"),
        keywords: t("app.keywords.chat"),
        group: t("palette.group.action"),
        run: () => {
          requestNewChat();
          go("chat");
        },
      },
      {
        id: "action:shortcuts",
        label: t("palette.action.shortcuts"),
        keywords: t("palette.action.shortcutsKeywords"),
        hint: comboLabel({ key: "?" }),
        group: t("palette.group.action"),
        run: () => setHelp(true),
      },
      {
        id: "action:setup",
        label: t("settings.setupAgain"),
        keywords: t("palette.action.setupKeywords"),
        group: t("palette.group.action"),
        run: () => {
          restartOnboarding();
          setOnboarding(true);
        },
      },
    ];
    const searches = saved.map((s): PaletteItem => ({
      id: `saved:${s.id}`,
      label: t("searchui.paletteSaved", { name: s.name }),
      keywords: t("app.keywords.search"),
      group: t("palette.group.action"),
      run: () => runSaved(s),
    }));
    return [...pages, ...actions, ...searches];
  }, [t, go, jump, saved, runSaved]);

  const searchItem = useCallback(
    (q: string): PaletteItem => ({
      id: "action:search",
      label: t("palette.action.search", { query: q }),
      group: t("palette.group.action"),
      run: () => {
        // The Search page takes focus into its box, so don't move focus to <main> here.
        requestSearch(q);
        go("search");
      },
    }),
    [t, go],
  );

  const shortcutRows = useMemo<ShortcutRow[]>(
    () => [
      { combos: [PALETTE], label: t("shortcuts.palette") },
      { combos: [{ mod: true, key: `1–${Math.min(9, PAGES.length)}` }], label: t("shortcuts.pages") },
      ...PAGES.filter((p) => p.shortcut).map((p) => ({ combos: [{ mod: true, key: p.shortcut! }], label: t("shortcuts.openPage", { page: t(p.label) }) })),
      { combos: [{ key: "?" }, { mod: true, key: "/" }], label: t("shortcuts.help") },
      { combos: [{ key: "Escape" }], label: t("shortcuts.close") },
    ],
    [t],
  );

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

  const navItem = (p: PageDef) => {
    const i = PAGES.indexOf(p);
    const combo = pageCombo(p, i);
    const label = t(p.label);
    return (
      <li key={p.id}>
        <button
          type="button"
          className="nav-item"
          aria-current={p.id === page ? "page" : undefined}
          aria-keyshortcuts={combo ? comboAria(combo) : undefined}
          title={combo ? t("shortcuts.hint", { label, keys: comboLabel(combo) }) : undefined}
          onClick={() => go(p.id)}
        >
          {p.icon && <Icon name={p.icon} />}
          {label}
          {p.id === "needs" && needsCount > 0 && <span className="nav-count">{formatCount(needsCount)}</span>}
        </button>
      </li>
    );
  };
  const inSettings = PAGES.find((p) => p.id === page)?.place === "settings";

  const current = PAGES.find((p) => p.id === page) ?? PAGES[0]!;
  const Current = current.Component;

  if (onboarding)
    return (
      <>
        <Onboarding connected={conn?.connected === true} onExit={endOnboarding} />
        <QuitPrompt />
      </>
    );

  return (
    <div className="shell">
      <a className="skip-link" href="#main">
        {t("app.skipToContent")}
      </a>
      <aside className="sidebar" aria-label={t("app.sidebarLabel")}>
        <div className="titlebar" data-tauri-drag-region aria-hidden="true" />
        <div className="brand">
          <Logo pulse={needsCount > 0} />
          <span className="wordmark">{t("app.name").toLowerCase()}</span>
        </div>
        <button type="button" className="palette-trigger" onClick={() => setPalette(true)} aria-keyshortcuts={comboAria(PALETTE)} aria-haspopup="dialog">
          <Icon name="search" size={14} />
          <span className="palette-trigger-label">{t("app.openPalette")}</span>
          <kbd aria-hidden="true" dir="ltr">
            {comboLabel(PALETTE)}
          </kbd>
        </button>
        {inSettings ? (
          <nav aria-label={t("app.navGroup.settings")} onKeyDown={onNavKey} className="nav-settings">
            <button type="button" className="nav-item nav-back" onClick={() => go(lastMain.current)}>
              <span className="flip-rtl">
                <Icon name="back" />
              </span>
              {t("app.backToApp")}
            </button>
            <p className="nav-group-label" id="nav-group-settings">
              {t("app.navGroup.settings")}
            </p>
            <ul aria-labelledby="nav-group-settings">{PAGES.filter((p) => p.place === "settings").map(navItem)}</ul>
            <SidebarSettings onSetup={() => setOnboarding(true)} />
          </nav>
        ) : (
          <nav aria-label={t("app.navLabel")} onKeyDown={onNavKey}>
            <ul>{PAGES.filter((p) => p.place === "main").map(navItem)}</ul>
            <div>
              <div className="nav-group-head">
                <p className="nav-group-label" id="nav-group-recent">
                  {t("app.recentChats")}
                </p>
                <button
                  type="button"
                  className="btn btn-ghost btn-icon nav-new-chat"
                  aria-label={t("palette.action.newChat")}
                  title={t("palette.action.newChat")}
                  onClick={() => {
                    requestNewChat();
                    go("chat");
                  }}
                >
                  <Icon name="plus" size={14} />
                </button>
              </div>
              <ul aria-labelledby="nav-group-recent">
                {recents.map((c) => (
                  <li key={c.id}>
                    <button
                      type="button"
                      className="nav-item"
                      onClick={() => {
                        requestChatOpen(c.id);
                        go("chat");
                      }}
                    >
                      <bdi className="nav-recent">{conversationTitle(c)}</bdi>
                    </button>
                  </li>
                ))}
                <li>
                  <button type="button" className="nav-item" aria-current={page === "chat" ? "page" : undefined} onClick={() => go("chat")}>
                    <Icon name="chat" />
                    {t("app.nav.chat")}
                  </button>
                </li>
              </ul>
            </div>
            <SavedSearchNav onRun={runSaved} />
          </nav>
        )}
        <div className="sidebar-foot">
          <AccountButton active={inSettings} onOpen={() => go(inSettings ? page : "status")} />
          <ConnectionPill conn={conn} />
        </div>
      </aside>
      <main id="main" ref={mainRef} tabIndex={-1} aria-label={t(current.label)}>
        <div className="titlebar" data-tauri-drag-region aria-hidden="true">
          {t(current.label)}
        </div>
        <div key={page} className="page-enter">
          {current.standalone ? (
            <Current />
          ) : conn === null ? (
            <div className="center muted">{t("app.connecting")}</div>
          ) : !conn.connected ? (
            <Offline conn={conn} onConnected={setConn} onRetry={refreshConnection} />
          ) : (
            <>
              <RecoveryBanner />
              <Current />
            </>
          )}
        </div>
      </main>
      <DeepLinks pages={PAGE_IDS} go={jump} />
      <QuitPrompt />
      {palette && <CommandPalette items={paletteItems} queryItem={searchItem} onClose={() => setPalette(false)} />}
      {help && <ShortcutsHelp rows={shortcutRows} onClose={() => setHelp(false)} />}
    </div>
  );
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

function formatCount(n: number): string {
  return n > 99 ? "99+" : String(n);
}

/** A saved conversation's title: its first question, on one line. */
function conversationTitle(c: StoredConversation): string {
  const first = c.title || c.messages.find((m) => m.role === "user")?.text || "";
  return first.replace(/\s+/g, " ").trim();
}

/** The newest saved conversations, reloaded whenever the page changes (Chat saves as it goes). */
function useRecentChats(page: PageId): StoredConversation[] {
  const [list, setList] = useState<StoredConversation[]>([]);
  useEffect(() => {
    let alive = true;
    chatHistory
      .load()
      .then((l) => alive && setList(l.filter((c) => conversationTitle(c)).slice(0, RECENT_CHATS)))
      .catch(() => {});
    return () => {
      alive = false;
    };
  }, [page]);
  return list;
}

/** How many cards wait in Needs you (0 while unknown or offline). */
function useNeedsCount(connected: boolean, page: PageId): number {
  const [n, setN] = useState(0);
  const [tick, setTick] = useState(0);
  useEvent(proactiveEvents.onCardsChanged, () => setTick((x) => x + 1));
  useEffect(() => {
    if (!connected) return;
    let alive = true;
    proactiveApi
      .cardsList({})
      .then((l) => alive && setN(l.enabled ? l.cards.length : 0))
      .catch(() => {});
    return () => {
      alive = false;
    };
  }, [connected, page, tick]);
  return n;
}

/** The account and settings entry at the foot of the sidebar (⌘,). */
function AccountButton({ active, onOpen }: { active: boolean; onOpen: () => void }) {
  const t = useT();
  const [email, setEmail] = useState<string | null>(null);
  useEffect(() => {
    let alive = true;
    cloudApi
      .session()
      .then((s) => alive && setEmail(s.signed_in ? s.email : null))
      .catch(() => {});
    return () => {
      alive = false;
    };
  }, []);
  const combo: Combo = { mod: true, key: "," };
  return (
    <button
      type="button"
      className="nav-item account"
      aria-current={active ? "page" : undefined}
      aria-keyshortcuts={comboAria(combo)}
      onClick={onOpen}
    >
      <span className="avatar" aria-hidden="true">
        {email ? email[0]!.toUpperCase() : <Icon name="settings" size={14} />}
      </span>
      <span className="account-text">
        <span>{t("app.navGroup.settings")}</span>
        {email && <bdi className="account-email">{email}</bdi>}
      </span>
      <kbd aria-hidden="true" dir="ltr">
        {comboLabel(combo)}
      </kbd>
    </button>
  );
}
