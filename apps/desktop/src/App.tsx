import { useCallback, useEffect, useMemo, useRef, useState, type ComponentType, type KeyboardEvent } from "react";
import { api, events, errorMessage, type ConnectionInfo, type PageId } from "./api";
import { useEvent } from "./hooks";
import { requestNewChat, requestSearch } from "./hooks.nav";
import { comboAria, comboLabel, digitOf, hasMod, isComposing, isTypingTarget, letterOf, useKeydown, type Combo } from "./hooks.shortcuts";
import { useT, type MessageKey } from "./i18n";
import { Logo } from "./components/Logo";
import { Offline } from "./components/Offline";
import { CommandPalette, type PaletteItem } from "./components/CommandPalette";
import { ShortcutsHelp, type ShortcutRow } from "./components/ShortcutsHelp";
import { ChatPage } from "./pages/Chat";
import { SearchPage } from "./pages/Search";
import { KnowledgePage } from "./pages/Knowledge";
import { AppsPage } from "./pages/Apps";
import { SourcesPage } from "./pages/Sources";
import { ModelsPage } from "./pages/Models";
import { PrivacyPage } from "./pages/Privacy";
import { StatusPage } from "./pages/Status";
import { Onboarding } from "./onboarding/Onboarding";
import { SidebarSettings } from "./onboarding/SidebarSettings";
import { restartOnboarding, shouldShowOnboarding } from "./onboarding/machine";

export interface PageDef {
  id: PageId;
  /** Nav label (also the page's landmark name and its palette entry). */
  label: MessageKey;
  Component: ComponentType;
  /** Extra words the command palette matches, e.g. synonyms ("ask, conversation"). */
  keywords?: MessageKey;
  /** A ⌘/Ctrl+<key> shortcut besides the automatic ⌘/Ctrl+1…9 by position. */
  shortcut?: string;
}

/**
 * Every page of the app, in nav order. The nav, ⌘/Ctrl+1…9, the command palette and page
 * restore all come from this list. To add a page: add its id to `PageId` in api.ts, import the
 * component above and add ONE line here (a label key under `app.nav.*` in en.json, optionally
 * `keywords` under `app.keywords.*`).
 */
export const PAGES: readonly PageDef[] = [
  { id: "chat", label: "app.nav.chat", Component: ChatPage, keywords: "app.keywords.chat" },
  { id: "search", label: "app.nav.search", Component: SearchPage, keywords: "app.keywords.search" },
  { id: "knowledge", label: "app.nav.knowledge", Component: KnowledgePage, keywords: "app.keywords.knowledge" },
  { id: "sources", label: "app.nav.sources", Component: SourcesPage, keywords: "app.keywords.sources" },
  { id: "apps", label: "app.nav.apps", Component: AppsPage, keywords: "app.keywords.apps" },
  { id: "models", label: "app.nav.models", Component: ModelsPage, keywords: "app.keywords.models" },
  { id: "privacy", label: "app.nav.privacy", Component: PrivacyPage, keywords: "app.keywords.privacy" },
  { id: "status", label: "app.nav.status", Component: StatusPage, keywords: "app.keywords.status", shortcut: "," },
];

const PAGE_KEY = "nugget.page";
const PALETTE: Combo = { mod: true, key: "K" };

function initialPage(): PageId {
  try {
    const saved = localStorage.getItem(PAGE_KEY);
    if (PAGES.some((p) => p.id === saved)) return saved as PageId;
  } catch {
    /* storage unavailable */
  }
  return "chat";
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
  const endOnboarding = useCallback(() => setOnboarding(false), []);
  const mainRef = useRef<HTMLElement>(null);
  // Set when a shortcut or the palette navigates: focus moves to the new page once it renders.
  const focusMain = useRef(false);

  const go = useCallback((id: PageId) => {
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
    return [...pages, ...actions];
  }, [t, go, jump]);

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
      { combo: PALETTE, label: t("shortcuts.palette") },
      { combo: { mod: true, key: `1–${Math.min(9, PAGES.length)}` }, label: t("shortcuts.pages") },
      ...PAGES.filter((p) => p.shortcut).map((p) => ({ combo: { mod: true, key: p.shortcut! }, label: t("shortcuts.openPage", { page: t(p.label) }) })),
      { combo: { key: "?" }, label: t("shortcuts.help") },
      { combo: { mod: true, key: "/" }, label: t("shortcuts.help") },
      { combo: { key: "Escape" }, label: t("shortcuts.close") },
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

  const current = PAGES.find((p) => p.id === page) ?? PAGES[0]!;
  const Current = current.Component;

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
        <button type="button" className="palette-trigger" onClick={() => setPalette(true)} aria-keyshortcuts={comboAria(PALETTE)} aria-haspopup="dialog">
          <span>{t("app.openPalette")}</span>
          <kbd aria-hidden="true">{comboLabel(PALETTE)}</kbd>
        </button>
        <nav aria-label={t("app.navLabel")} onKeyDown={onNavKey}>
          <ul>
            {PAGES.map((p, i) => {
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
                    {label}
                  </button>
                </li>
              );
            })}
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
          <Current />
        )}
      </main>
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
