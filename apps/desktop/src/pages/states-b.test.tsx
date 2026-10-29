// @vitest-environment jsdom
/**
 * Empty, error, loading and daemon-stopped states for Agent, Apps (+ connect wizard), Double Tap,
 * Plugins, Privacy and Safety: each says what is happening and what to do next.
 */
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import type { DoubleTapSettings, PolicyState } from "../api";
import { App } from "../App";
import { byText, cleanup, click, flush, installDomShims, mockDaemon, render } from "../test/render";
import { AgentPage } from "./Agent";
import { AppsPage } from "./Apps";
import { DoubleTapPage } from "./DoubleTap";
import { PluginsPage } from "./Plugins";
import { PrivacyPage } from "./Privacy";
import { SafetyPage } from "./Safety";

beforeEach(installDomShims);
afterEach(cleanup);

/** A daemon call that fails with a JSON-RPC error (not "method not found"). */
const fails = (message: string) => () => {
  throw { kind: "rpc", message, code: -32000 };
};
/** A daemon call that never answers. */
const hangs = () => new Promise(() => {});

const alerts = (root: ParentNode) => Array.from(root.querySelectorAll(".note-error, [role=alert]"));
const statusText = (root: ParentNode) => Array.from(root.querySelectorAll("[role=status]")).map((s) => s.textContent ?? "");

const policy: PolicyState = {
  config: { version: 1, rules: [], disabled_defaults: [], rate_limits: { send: 25, destroy: 25, spend: 3, security: 3 }, confirm_timeout_secs: 120 },
  defaults: [{ rule: { id: "default:spend", description: "Always ask before spending", risks: ["spend"], effect: "confirm" }, enabled: true }],
  halted: false,
  log: { entries: 0, head: null, chain_ok: true },
};

const dtSettings: DoubleTapSettings = {
  enabled: true,
  trigger: { kind: "double_tap", modifier: "right_control", window_ms: 350 },
  app_providers: {},
  excluded_apps: ["1Password", "Bitwarden", "chase.com"],
  allow_terminals: false,
};

/** A fresh install: nothing configured, no tasks, no plugins, no apps, no providers. */
const fresh = {
  providers_list: [],
  sources_list: [],
  mcp_clients_list: [],
  plugins_list: { plugins: [] },
  egress_recent: [],
  agent_list: [],
  policy_get: policy,
  policy_approvals: { pending: [], task: [] },
  browser_status: { connected: false, origin: null, host_version: null, tools: [] },
  doubletap_settings_get: dtSettings,
  doubletap_default_excluded: ["1Password", "Bitwarden", "chase.com"],
  voice_status: { samples: 0, channels: [], last_built: null },
};

describe("Agent page states", () => {
  it("fresh install: no tasks, and says a model is needed", async () => {
    mockDaemon(fresh);
    const { container } = await render(<AgentPage />);
    expect(container.textContent).toContain("No tasks yet");
    expect(container.textContent).toContain("No AI model set up.");
    expect(container.textContent).toContain("Add a model on the Models page");
    expect(alerts(container)).toEqual([]);
  });

  it("a failing history call shows an error with Retry", async () => {
    mockDaemon({ ...fresh, agent_list: fails("task log is locked") });
    const { container } = await render(<AgentPage />);
    const err = container.querySelector(".note-error")!;
    expect(err.textContent).toContain("task log is locked");
    expect(byText(err, "button", "Retry")).toBeDefined();
  });

  it("announces loading", async () => {
    mockDaemon({ ...fresh, agent_list: hangs });
    const { container } = await render(<AgentPage />);
    expect(statusText(container).join(" ")).toContain("Loading recent tasks");
  });
});

describe("Apps page states", () => {
  it("fresh install: no apps, with a connect button; the wizard explains there are no sources yet", async () => {
    mockDaemon(fresh);
    const { container } = await render(<AppsPage />);
    expect(container.textContent).toContain("No apps connected");
    const connect = container.querySelectorAll(".pg-empty button");
    expect(connect[0]?.textContent).toBe("Connect an app");
    await click(connect[0]);
    expect(document.querySelector("dialog")?.textContent).toContain("No sources added yet.");
    expect(alerts(container)).toEqual([]);
  });

  it("a failing list shows an error with Retry", async () => {
    mockDaemon({ ...fresh, mcp_clients_list: fails("token store unreadable") });
    const { container } = await render(<AppsPage />);
    const err = container.querySelector(".note-error")!;
    expect(err.textContent).toContain("token store unreadable");
    expect(byText(err, "button", "Retry")).toBeDefined();
  });

  it("announces loading", async () => {
    mockDaemon({ ...fresh, mcp_clients_list: hangs });
    const { container } = await render(<AppsPage />);
    expect(statusText(container).join(" ")).toContain("Loading connected apps");
  });
});

describe("Plugins page states", () => {
  it("fresh install: no plugins, says how to add one", async () => {
    mockDaemon(fresh);
    const { container } = await render(<PluginsPage />);
    expect(container.textContent).toContain("No plugins installed");
    expect(container.textContent).toContain("Install a .nuggetplugin file");
    expect(alerts(container)).toEqual([]);
  });

  it("a failing list shows an error with Retry", async () => {
    mockDaemon({ ...fresh, plugins_list: fails("plugin host crashed") });
    const { container } = await render(<PluginsPage />);
    const err = container.querySelector(".note-error")!;
    expect(err.textContent).toContain("plugin host crashed");
    expect(byText(err, "button", "Retry")).toBeDefined();
  });

  it("announces loading", async () => {
    mockDaemon({ ...fresh, plugins_list: hangs });
    const { container } = await render(<PluginsPage />);
    expect(statusText(container).join(" ")).toContain("Loading plugins");
  });
});

describe("Privacy page states", () => {
  it("fresh install: nothing has left this device", async () => {
    mockDaemon(fresh);
    const { container } = await render(<PrivacyPage />);
    expect(container.textContent).toContain("Nothing has left this device");
    expect(container.textContent).toContain("Requests to cloud providers and model downloads will appear here.");
    expect(alerts(container)).toEqual([]);
  });

  it("a failing ledger shows an error with Retry", async () => {
    mockDaemon({ ...fresh, egress_recent: fails("ledger unavailable") });
    const { container } = await render(<PrivacyPage />);
    const err = container.querySelector(".note-error")!;
    expect(err.textContent).toContain("ledger unavailable");
    expect(byText(err, "button", "Retry")).toBeDefined();
  });
});

describe("Safety page states", () => {
  it("fresh install: no rules of your own, nothing waiting, browser not connected", async () => {
    mockDaemon(fresh);
    const { container } = await render(<SafetyPage />);
    expect(container.textContent).toContain("No rules of your own");
    expect(container.textContent).toContain("The built-in rules below apply.");
    expect(alerts(container)).toEqual([]);
  });

  it("a failing policy call shows an error with Retry", async () => {
    mockDaemon({ ...fresh, policy_get: fails("policy file is invalid") });
    const { container } = await render(<SafetyPage />);
    const err = container.querySelector(".note-error")!;
    expect(err.textContent).toContain("policy file is invalid");
    expect(byText(err, "button", "Retry")).toBeDefined();
  });

  it("announces loading", async () => {
    mockDaemon({ ...fresh, policy_get: hangs });
    const { container } = await render(<SafetyPage />);
    expect(statusText(container).join(" ")).toContain("Loading safety settings");
  });
});

describe("Double Tap page states", () => {
  it("fresh install: built-in exclusions listed apart, own list empty, no models hint", async () => {
    mockDaemon({ ...fresh, doubletap_status: hangs });
    const { container } = await render(<DoubleTapPage />);
    const section = container.querySelector('section[aria-labelledby="dt-excluded"]')!;
    expect(section.textContent).toContain("Built in");
    expect(section.textContent).toContain("Show the list (3)");
    const builtIn = section.querySelector("ul")!;
    expect(builtIn.textContent).toContain("chase.com");
    // Read-only: no remove buttons on built-in entries.
    expect(builtIn.querySelector("button")).toBeNull();
    expect(section.textContent).toContain("No apps of your own. The built-in exclusions above still apply.");
    expect(section.textContent).not.toContain("No apps are excluded.");
    expect(container.textContent).toContain("No AI models are set up yet. Add one on the Models page");
    expect(container.textContent).toContain("Nugget learns your style");
    expect(alerts(container)).toEqual([]);
  });

  it("lists the user's own apps apart from the built-ins, and offers to restore removed built-ins", async () => {
    mockDaemon({ ...fresh, doubletap_settings_get: { ...dtSettings, excluded_apps: ["1Password", "com.example.secret"] }, doubletap_status: hangs });
    const { container } = await render(<DoubleTapPage />);
    const section = container.querySelector('section[aria-labelledby="dt-excluded"]')!;
    expect(section.querySelector('button[aria-label="Remove com.example.secret"]')).not.toBeNull();
    expect(section.querySelector('button[aria-label="Remove 1Password"]')).toBeNull();
    expect(section.textContent).toContain("Some built-in exclusions were removed from this list (2).");
    await click(byText(section, "button", "Restore defaults"));
    expect(section.textContent).not.toContain("were removed");
    expect(section.textContent).toContain("Show the list (3)");
  });

  it("a failing settings call shows an error with Retry, and loading is announced", async () => {
    mockDaemon({ ...fresh, doubletap_settings_get: fails("settings file unreadable"), doubletap_status: hangs });
    const { container } = await render(<DoubleTapPage />);
    const err = container.querySelector(".note-error")!;
    expect(err.textContent).toContain("settings file unreadable");
    expect(byText(err, "button", "Retry")).toBeDefined();
    // The permissions card is still loading.
    expect(statusText(container).join(" ")).toContain("Checking permissions");
  });
});

describe("daemon stopped", () => {
  function offline(extra: Record<string, unknown> = {}) {
    const calls = mockDaemon({ connection_info: { connected: false, socket: "/tmp/nuggetd.sock", error: "connection refused" }, ...extra });
    localStorage.setItem("nugget.onboarding.v1", JSON.stringify({ version: 1, step: "done", completed: [], skipped: [], ai: null, finished: true }));
    return calls;
  }

  it.each(["agent", "apps", "plugins", "privacy", "safety", "doubletap"])("%s shows the not-running screen with start and retry", async (page) => {
    const calls = offline();
    localStorage.setItem("nugget.page", page);
    await render(<App />);
    await flush(5);
    const main = document.querySelector("main")!;
    expect(main.querySelector("h1")?.textContent).toBe("Nugget isn't running");
    expect(byText(main, "button", "Start Nugget")).toBeDefined();
    expect(byText(main, "button", "Try again")).toBeDefined();
    expect(main.textContent).toContain("Start it yourself");
    expect(main.textContent).toContain("nuggetd");
    // The page itself doesn't call the daemon while it's down.
    expect(calls.filter((c) => ["agent_list", "mcp_clients_list", "plugins_list", "egress_recent", "policy_get"].includes(c))).toEqual([]);
  });

  it("says when a retry finds it still down", async () => {
    offline();
    await render(<App />);
    await flush(5);
    const main = document.querySelector("main")!;
    await click(byText(main, "button", "Try again"));
    await flush(3);
    expect(statusText(main).join(" ")).toContain("Still not running.");
  });

  it("explains a failed start with the next step", async () => {
    offline({ start_daemon: fails("nuggetd not found on PATH") });
    await render(<App />);
    await flush(5);
    const main = document.querySelector("main")!;
    await click(byText(main, "button", "Start Nugget"));
    await flush(3);
    const alert = main.querySelector("[role=alert]")!;
    expect(alert.textContent).toContain("Couldn't start the background service: nuggetd not found on PATH");
    expect(alert.textContent).toContain("Try again, or start it yourself");
  });
});
