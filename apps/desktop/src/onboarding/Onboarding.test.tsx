// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { emit } from "@tauri-apps/api/event";
import { act } from "react";
import { byText, cleanup, click, flush, installDomShims, mockDaemon, render } from "../test/render";
import { Onboarding } from "./Onboarding";
import { INITIAL, loadState } from "./machine";
import { TRIGGER_KEY } from "./trigger";
import type { DoubleTapSettings, SourceInfo } from "../api";
import type { CloudConnections, CloudSession } from "../cloud";
import { usePendingChatAsk } from "../hooks.nav";

beforeEach(() => {
  installDomShims();
});
afterEach(cleanup);

const heading = (c: HTMLElement) => c.querySelector("h1")?.textContent;
const button = (c: HTMLElement, text: string | RegExp) => byText(c, "button", text);

describe("Onboarding", () => {
  it("walks every step, saving progress, and exits when finished", async () => {
    let dt: DoubleTapSettings = {
      enabled: true,
      trigger: { kind: "double_tap", modifier: "right_control", window_ms: 350 },
      app_providers: {},
      excluded_apps: [],
      allow_terminals: false,
    };
    mockDaemon({
      doubletap_settings_get: () => dt,
      doubletap_settings_set: (args: unknown) => (dt = (args as { settings: DoubleTapSettings }).settings),
    });
    const onExit = vi.fn();
    const { container } = await render(<Onboarding connected onExit={onExit} />);
    expect(heading(container)).toBe("Welcome to Heed");
    // No account on offer in this build: the local-first welcome, and three stages of progress.
    expect(container.querySelectorAll(".ob-progress li").length).toBe(3);
    expect(container.querySelector('.ob-progress [aria-current="step"]')?.closest("li")?.className).toContain("ob-seg-current");

    await click(button(container, "Continue"));
    expect(heading(container)).toBe("Add your first source");
    expect(document.activeElement).toBe(container.querySelector("h1"));
    expect(loadState().step).toBe("source");
    // The sources the daemon already has are listed with their progress (GG6-12), so the folder
    // button offers another one; it opens the folder form directly, skipping the kind picker.
    await flush();
    expect(container.querySelectorAll(".ob-index li").length).toBeGreaterThan(0);
    await click(button(container, "Add another source"));
    expect(document.querySelector("dialog")).not.toBeNull();
    expect(byText(document.body, "dialog button", "Choose folders…")).toBeDefined();
    await click(document.querySelector<HTMLElement>('dialog button[aria-label="Close"]'));
    expect(document.querySelector("dialog")).toBeNull();
    // The sources the daemon already has count: wait until the step knows about them.
    await flush();
    const hint = () => container.querySelector("#ob-continue-hint");
    expect(hint()).toBeNull();

    await click(button(container, "Continue"));
    expect(heading(container)).toBe("Choose your AI");
    // Local: recommended models with sizes, install with progress.
    const local = container.querySelector<HTMLInputElement>('input[type="radio"][value="local"]')!;
    await click(local);
    await flush();
    expect(container.textContent).toContain("qwen3-4b-q4");
    expect(container.textContent).toContain("2.3 GB download");
    expect(container.textContent).not.toContain("llama-8b-q4");
    await click(button(container, /Show all models/));
    expect(container.textContent).toContain("llama-8b-q4");
    await click(byText(container, "li.card button", "Install"));
    await act(() => emit("nugget:models-progress", { id: "qwen3-4b-q4", downloaded: 1_250_000_000, total: 2_500_000_000, phase: "downloading", error: null }));
    await flush();
    expect(container.querySelector("progress")?.getAttribute("value")).toBe("50");
    expect(container.textContent).toContain("Downloading · 50% · 1.2 GB of 2.3 GB");
    // A download in progress doesn't set the step up yet (it can still fail): Continue would skip it.
    expect(hint()?.textContent).toBe("Nothing set up here yet: Continue skips this step.");
    expect(button(container, "Continue")!.getAttribute("aria-describedby")).toBe("ob-continue-hint");
    await act(() => emit("nugget:models-progress", { id: "qwen3-4b-q4", downloaded: 2_500_000_000, total: 2_500_000_000, phase: "installed", error: null }));
    await flush();
    expect(hint()).toBeNull();
    expect(button(container, "Continue")!.hasAttribute("aria-describedby")).toBe(false);

    await click(button(container, "Continue"));
    expect(loadState().ai).toBe("local");
    expect(heading(container)).toBe("What Double Tap needs");
    await click(button(container, "Continue"));
    expect(heading(container)).toBe("Choose your Double Tap trigger");
    await click(container.querySelectorAll<HTMLInputElement>('input[type="radio"]')[1]!);
    // Saved where Double Tap reads it (and cached locally).
    expect(dt.trigger).toMatchObject({ kind: "chord" });
    expect(JSON.parse(localStorage.getItem(TRIGGER_KEY)!)).toMatchObject({ kind: "chord" });

    await click(button(container, "Continue"));
    // The last step asks the first question; what setup did sits below it.
    expect(container.querySelector("textarea#ob-question")).not.toBeNull();
    expect(container.querySelector(".ob-setup h2")?.textContent).toBe("You're all set");
    expect(container.textContent).toContain("Every step is done.");
    // The summary says what was set up.
    const summary = [...container.querySelectorAll(".ob-summary > div")].map((d) => d.textContent);
    expect(summary[0]).toBe("AI modelOn this device (qwen3-4b-q4)");
    expect(summary[1]).toBe("SourcesDocuments and Work mail");
    expect(container.querySelectorAll(".ob-summary kbd").length).toBeGreaterThan(0);
    // "Skip setup" keeps its slot on the last step but is hidden.
    expect(button(container, "Skip setup")!.classList.contains("ob-invisible")).toBe(true);
    expect(onExit).not.toHaveBeenCalled();
    // The mocked daemon has an indexed source, so the last step points at Search.
    expect(container.textContent).toContain("1,234 items are indexed and searchable.");
    await click(button(container, "Search your sources"));
    expect(onExit).toHaveBeenCalledWith("search");
    expect(loadState().finished).toBe(true);
  });

  it("GG6-2: a folder with nothing indexable says so and why, and doesn't finish the step", async () => {
    const empty: SourceInfo = {
      id: "files-3",
      kind: "files",
      name: "Pictures",
      enabled: true,
      state: "watching",
      items: 0,
      chunks: 0,
      scan: {
        finished: true,
        found: 0,
        unsupported: 3,
        too_large: 1,
        unreadable: 0,
        roots_missing: 0,
        roots_denied: 0,
        max_file_bytes: 50 * 1024 * 1024,
        skipped: [
          { path: "holiday.jpg", reason: "unsupported" },
          { path: "film.mov", reason: "too_large", bytes: 200_000_000 },
        ],
      },
    };
    mockDaemon({ sources_list: () => [empty] });
    const { container } = await render(<Onboarding connected onExit={() => {}} initial={{ ...INITIAL, step: "source", completed: ["welcome"] }} />);
    await flush();
    const row = container.querySelector<HTMLElement>(".ob-index li")!;
    expect(row.dataset.phase).toBe("empty");
    expect(row.querySelector(".spinner")).toBeNull();
    expect(container.querySelector(".ob-index")?.getAttribute("aria-busy")).toBe("false");
    expect(row.textContent).toContain("Pictures: nothing in this folder can be indexed.");
    expect(row.textContent).toContain("3 files of a type Heed doesn't read");
    // MM1-8: the same limit and unit the daemon states (50 MiB reads "50 MB", not "52 MB").
    expect(row.textContent).toContain("1 file larger than the 50 MB limit");
    expect(row.textContent).toContain("holiday.jpg");
    expect(row.textContent).toContain("film.mov");
    expect(container.textContent).toContain("Add a different folder, or continue without one.");
    // Continue here is continuing without a source: said next to the button, and the last step lists it.
    expect(container.querySelector("#ob-continue-hint")).not.toBeNull();
    await click(button(container, "Continue"));
    expect(loadState().skipped).toContain("source");
  });

  it("MM1-6: the last step doesn't claim search works when nothing was indexed", async () => {
    const empty: SourceInfo = { id: "files-4", kind: "files", name: "Empty", enabled: true, state: "watching", items: 0, chunks: 0 };
    mockDaemon({ sources_list: () => [empty] });
    const all = ["welcome", "ai", "source", "permissions", "trigger"] as const;
    const { container } = await render(
      <Onboarding connected onExit={() => {}} initial={{ ...INITIAL, step: "done", ai: "later", completed: [...all], sources: ["Empty"] }} />,
    );
    await flush();
    expect(container.querySelector(".ob-setup h2")?.textContent).toBe("Almost there");
    expect(container.textContent).toContain("Nothing has been indexed from your sources yet");
    expect(container.textContent).not.toContain("Search works straight away");
    expect(container.textContent).not.toContain("Every step is done.");
    const row = container.querySelectorAll(".ob-summary > div")[1]!;
    expect(row.className).toBe("ob-summary-unset");
    expect(row.textContent).toContain("Nothing indexed yet");
    await click(button(container, "Add another source"));
    expect(heading(container)).toBe("Add your first source");
  });

  it("GG6-2: a folder Heed can't open says so", async () => {
    const denied: SourceInfo = {
      id: "files-4",
      kind: "files",
      name: "Locked",
      enabled: true,
      state: "watching",
      items: 0,
      chunks: 0,
      scan: { finished: true, found: 0, unsupported: 0, too_large: 0, unreadable: 1, roots_missing: 0, roots_denied: 1, max_file_bytes: 50_000_000 },
    };
    mockDaemon({ sources_list: () => [denied] });
    const { container } = await render(<Onboarding connected onExit={() => {}} initial={{ ...INITIAL, step: "source" }} />);
    await flush();
    const row = container.querySelector<HTMLElement>(".ob-index li")!;
    expect(row.dataset.phase).toBe("denied");
    expect(row.textContent).toContain("Heed can't open this folder");
  });

  it("GG6-26: indexing progress shows the total once the scan knows it", async () => {
    const folder: SourceInfo = {
      id: "files-5",
      kind: "files",
      name: "Docs",
      enabled: true,
      state: "syncing",
      items: 5,
      chunks: 5,
      scan: { finished: true, found: 24, unsupported: 0, too_large: 0, unreadable: 0, roots_missing: 0, roots_denied: 0, max_file_bytes: 50_000_000 },
    };
    let listed = [folder];
    mockDaemon({ sources_list: () => listed });
    const { container } = await render(<Onboarding connected onExit={() => {}} initial={{ ...INITIAL, step: "source" }} />);
    await flush();
    expect(container.querySelector(".ob-index li")?.textContent).toContain("Docs: indexing, 5 of 24 items…");
    listed = [{ ...folder, state: "watching", items: 10 }];
    await act(() => emit("nugget:sources-changed", {}));
    await flush();
    expect(container.querySelector(".ob-index li")?.textContent).toContain("Docs: 10 items of 24 indexed so far. Search works now");
    // Something is searchable: the step is done.
    expect(container.querySelector("#ob-continue-hint")).toBeNull();
    expect(loadState().indexed).toBe(true);
  });

  it("GG6-12: the source step remembers a folder across Back and setup again", async () => {
    const folder: SourceInfo = { id: "files-7", kind: "files", name: "back-small", enabled: true, state: "watching", items: 1, chunks: 1 };
    mockDaemon({ sources_list: () => [folder] });
    const { container } = await render(<Onboarding connected onExit={() => {}} initial={{ ...INITIAL, step: "source", ai: "later", completed: ["welcome"] }} />);
    await flush();
    expect(container.querySelector(".ob-index li")?.textContent).toContain("back-small: 1 item indexed. Search works now.");
    await click(button(container, "Back"));
    expect(heading(container)).toBe("Welcome to Heed");
    await click(button(container, "Continue"));
    await flush();
    expect(heading(container)).toBe("Add your first source");
    expect(container.querySelector(".ob-index li")?.textContent).toContain("back-small: 1 item indexed. Search works now.");
    expect(button(container, "Add another source")).toBeDefined();
    expect(button(container, "Add a folder")).toBeUndefined();
  });

  it("GG6-31, GG6-21: the recommended AI option starts selected, and Decide later is not a skipped step", async () => {
    mockDaemon({ sources_list: [], providers_list: [] });
    const { container } = await render(<Onboarding connected onExit={() => {}} initial={{ ...INITIAL, step: "ai", completed: ["welcome", "source"] }} />);
    await flush();
    expect(container.querySelector<HTMLInputElement>('input[type="radio"][value="local"]')!.checked).toBe(true);
    await click(container.querySelector<HTMLInputElement>('input[type="radio"][value="later"]'));
    expect(container.querySelector("#ob-continue-hint")).toBeNull();
    await click(button(container, "Continue"));
    expect(loadState().skipped).not.toContain("ai");
    expect(loadState().completed).toContain("ai");
  });

  it("GG6-16: says how long the download takes, before and during it", async () => {
    mockDaemon();
    const { container } = await render(<Onboarding connected onExit={() => {}} initial={{ ...INITIAL, step: "ai", ai: "local", completed: ["welcome", "source"] }} />);
    await flush();
    // 2.5 GB: under 4 minutes at 100 Mbit/s, about 34 at 10 Mbit/s.
    expect(container.textContent).toContain("about 4 minutes on a fast one (100 Mbit/s), about 34 minutes on a slower one (10 Mbit/s)");
    expect(container.textContent).not.toContain("two minutes");
  });

  it("takes a key in a masked field only, and still shows the manual setup", async () => {
    mockDaemon();
    const { container } = await render(<Onboarding connected onExit={() => {}} initial={{ ...INITIAL, step: "ai", completed: ["welcome", "source"] }} />);
    await click(container.querySelector<HTMLInputElement>('input[value="byok"]'));
    // The key goes in a password field that the browser won't remember.
    const key = container.querySelectorAll<HTMLInputElement>('input[type="password"]');
    expect(key).toHaveLength(1);
    expect(key[0]!.autocomplete).toBe("off");
    expect(container.querySelector("textarea")).toBeNull();
    expect(container.textContent).toContain('api_key = "keychain:anthropic"');
    expect(container.textContent).toContain("dev.nugget.Nugget");
  });
});

describe("Onboarding with a Heed account", () => {
  const signedOut: CloudSession = {
    configured: true,
    signed_in: false,
    email: null,
  };
  const signedIn: CloudSession = {
    configured: true,
    signed_in: true,
    email: "ana@example.com",
  };
  const catalogue: CloudConnections = {
    connections: [
      {
        id: "c1",
        toolkit: "gmail",
        status: "ACTIVE",
        last_synced_at: null,
        last_error: null,
      },
      {
        id: "c2",
        toolkit: "notion",
        status: "INITIATED",
        last_synced_at: null,
        last_error: null,
      },
    ],
    available: [
      { slug: "gmail", name: "Gmail", logo: null, connectable: true },
      { slug: "notion", name: "Notion", logo: null, connectable: true },
      {
        slug: "linear",
        name: "Linear",
        logo: "https://logos.example/linear.svg",
        connectable: true,
      },
      { slug: "figma", name: "Figma", logo: null, connectable: false },
    ],
  };

  it("signs in first, with a local-first way on without an account", async () => {
    const calls = mockDaemon({
      cloud_session: () => signedOut,
      cloud_sign_in: () => null,
    });
    const { container } = await render(<Onboarding connected onExit={() => {}} />);
    expect(heading(container)).toBe("Sign in to Heed Cloud");
    // One primary action: Google. The footer's way on is the local-first one, not a primary.
    const google = button(container, "Continue with Google")!;
    expect(google.classList.contains("btn-primary")).toBe(true);
    expect(container.querySelectorAll(".btn-primary").length).toBe(1);
    expect(container.querySelector<HTMLLabelElement>('label[for="ob-email"]')?.textContent).toBe("Email address");
    await click(google);
    expect(calls).toContain("cloud_sign_in");
    expect(container.textContent).toContain("Finish signing in in your browser.");
    const footer = container.querySelector("footer.ob-actions")!;
    const without = footer.querySelector<HTMLButtonElement>("button:last-child")!;
    expect(without.classList.contains("btn-primary")).toBe(false);
    await click(without);
    // Without an account: the local sources, and a way back to sign in for one-click apps.
    expect(heading(container)).toBe("Add your first source");
    expect(loadState().completed).toContain("welcome");
    expect(container.querySelector(".ob-signin-first")).not.toBeNull();
    await click(button(container, "Sign in"));
    expect(heading(container)).toBe("Sign in to Heed Cloud");
  });

  it("moves on to connecting apps when the sign-in finishes", async () => {
    let session = signedOut;
    mockDaemon({
      cloud_session: () => session,
      cloud_connections_list: () => catalogue,
    });
    const { container } = await render(<Onboarding connected onExit={() => {}} />);
    expect(heading(container)).toBe("Sign in to Heed Cloud");
    session = signedIn;
    await act(() => emit("nugget:cloud-session", { signed_in: true, error: null }));
    await flush();
    expect(loadState().step).toBe("source");
    expect(container.querySelectorAll(".ob-app").length).toBe(4);
  });

  it("connects apps in one click, with their states, and keeps the local folder below", async () => {
    let list = catalogue;
    const calls = mockDaemon({
      cloud_session: () => signedIn,
      cloud_connections_list: () => list,
      cloud_connect: () => null,
      cloud_sync: () => ({ synced: [] }),
      sources_list: [],
    });
    const { container } = await render(<Onboarding connected onExit={() => {}} initial={{ ...INITIAL, step: "source", completed: ["welcome"] }} />);
    await flush();
    const states = () =>
      [...container.querySelectorAll<HTMLElement>(".ob-app")].map((li) => `${li.querySelector(".ob-app-name")?.textContent}:${li.dataset.state}`);
    expect(states()).toEqual(["Gmail:connected", "Notion:authorising", "Linear:idle", "Figma:soon"]);
    expect(container.querySelector(".ob-app-connected .ob-app-check")).not.toBeNull();
    // A remote logo can't load under the app's CSP: the tile shows the app's initial instead.
    expect(container.querySelector(".ob-app-idle .ob-app-logo img")).toBeNull();
    expect(container.querySelector(".ob-app-idle .ob-app-logo")?.textContent).toBe("L");
    // A connected app finishes the step.
    expect(loadState().cloudApps).toEqual(["Gmail"]);
    expect(container.querySelector("#ob-continue-hint")).toBeNull();

    await click(container.querySelector('button[aria-label="Connect Linear"]'));
    expect(calls).toContain("cloud_connect");
    expect(states()[2]).toBe("Linear:authorising");
    expect(container.querySelector(".ob-connect-status")?.textContent).toContain("Finish connecting Linear in your browser");

    // The browser comes back: Linear is connected, announced, and the first sync starts.
    list = {
      ...catalogue,
      connections: [
        ...catalogue.connections,
        {
          id: "c3",
          toolkit: "linear",
          status: "ACTIVE",
          last_synced_at: null,
          last_error: null,
        },
      ],
    };
    await act(() => emit("nugget:cloud-connections", null));
    await flush();
    expect(states()[2]).toBe("Linear:connected");
    expect(calls).toContain("cloud_sync");
    expect(loadState().cloudApps).toEqual(["Gmail", "Linear"]);

    // The local folder and other sources are still here, below the apps.
    expect(container.querySelector(".ob-local-heading")).not.toBeNull();
    expect(button(container, "Add a folder")).toBeDefined();
    expect(button(container, "Mail, calendar and more…")).toBeDefined();
  });

  it("hands the first question to Chat", async () => {
    mockDaemon({ sources_list: [] });
    const onExit = vi.fn();
    const all = ["welcome", "source", "ai", "permissions", "trigger"] as const;
    const { container } = await render(<Onboarding connected onExit={onExit} initial={{ ...INITIAL, step: "done", ai: "later", completed: [...all] }} />);
    const ask = container.querySelector<HTMLButtonElement>(".ob-ask button[type=submit]")!;
    expect(ask.disabled).toBe(true);
    expect(ask.classList.contains("btn-primary")).toBe(true);
    // The footer's way out is not a second primary.
    expect(container.querySelector("footer.ob-actions .btn-primary")).toBeNull();
    const field = container.querySelector<HTMLTextAreaElement>("#ob-question")!;
    expect(container.querySelector('label[for="ob-question"]')?.textContent).toBe("Ask a question about your data");
    // A suggestion fills the question in.
    await click(container.querySelector(".ob-suggestion"));
    expect(field.value).not.toBe("");
    expect(document.activeElement).toBe(field);
    const setValue = Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, "value")!.set!;
    await act(async () => {
      setValue.call(field, "Who reviewed the Tidewater contract?");
      field.dispatchEvent(new Event("input", { bubbles: true }));
    });
    await click(ask);
    expect(onExit).toHaveBeenCalledWith("chat");
    expect(loadState().finished).toBe(true);

    // Chat picks the question up when it mounts, and sends it.
    const got: string[] = [];
    function ChatProbe() {
      usePendingChatAsk((q) => got.push(q));
      return null;
    }
    await render(<ChatProbe />);
    expect(got).toEqual(["Who reviewed the Tidewater contract?"]);
  });
});
