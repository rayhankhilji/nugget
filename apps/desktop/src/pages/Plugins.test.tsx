// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import type { PluginCatalogEntry, PluginInstallResult } from "../api";
import { plugins } from "../test/fixtures";
import { byText, cleanup, click, installDomShims, mockDaemon, render } from "../test/render";
import { PluginsPage } from "./Plugins";

beforeEach(installDomShims);
afterEach(cleanup);

const staged = (over: Partial<PluginInstallResult> = {}): PluginInstallResult => ({
  token: "tok-1",
  id: "weather",
  name: "Weather",
  version: "2.0.0",
  description: "Forecasts for the assistant.",
  kinds: ["tool"],
  signature: "trusted",
  signer: "cd".repeat(32),
  requested: { hosts: ["api.weather.example", "*.tiles.example"], secrets: ["api_key"], kv_max_bytes: 10_000 },
  previously_approved: null,
  installed_version: null,
  needs_approval: true,
  installed: false,
  ...over,
});

const dialog = () => document.querySelector("dialog");

const entry = (over: Partial<PluginCatalogEntry> = {}): PluginCatalogEntry => ({
  path: "/opt/nugget/plugin-catalog/obsidian.nuggetplugin",
  file_name: "obsidian.nuggetplugin",
  id: "dev.nugget.obsidian",
  name: "Obsidian vault",
  version: "1.0.0",
  description: "Imports notes from an Obsidian vault.",
  kinds: ["connector"],
  requested: { hosts: [], secrets: [], kv_max_bytes: 0, file_roots: ["vault"] },
  connector_display_name: "Obsidian vault",
  signature: "trusted",
  signer: "ab".repeat(32),
  installed_version: null,
  update_available: false,
  needs_approval: true,
  error: null,
  ...over,
});

const availableList = (container: HTMLElement) => container.querySelector<HTMLElement>('ul[aria-label="Plugins available to install"]');

describe("PluginsPage", () => {
  it("lists installed plugins with signature, state and approved access", async () => {
    mockDaemon();
    const { container } = await render(<PluginsPage />);
    const row = container.querySelector("li.source")!;
    expect(row.textContent).toContain("RSS feeds");
    expect(row.textContent).toContain("Trusted signer");
    expect(row.textContent).toContain("Running");
    expect(row.textContent).toContain("Connect to feeds.example.com");
    expect(row.textContent).toContain("Connect to any subdomain of example.org");
    expect(row.querySelector<HTMLInputElement>('input[role="switch"]')?.checked).toBe(true);
  });

  it("explains requested access in plain language and approves the staged package", async () => {
    const approvals: unknown[] = [];
    mockDaemon({
      pick_plugin_file: "/home/me/weather.nuggetplugin",
      plugins_install: staged(),
      plugins_approve: (args: unknown) => {
        approvals.push(args);
        return { plugin: { ...plugins[0]!, id: "weather", name: "Weather", kinds: ["tool"] } };
      },
    });
    const { container } = await render(<PluginsPage />);
    await click(byText(container, "button", "Install from file"));

    const d = dialog()!;
    expect(d.querySelector("h2")?.textContent).toBe("Install Weather?");
    expect(d.textContent).toContain("Connect to api.weather.example");
    expect(d.textContent).toContain("Connect to any subdomain of tiles.example");
    expect(d.textContent).toContain("Read a secret you set, named api_key");
    expect(d.textContent).toMatch(/Store up to 10 kB of its own data/i);
    // Focus starts on the safe choice.
    expect(document.activeElement?.textContent).toBe("Cancel");

    await click(byText(d, "button", "Allow and install"));
    expect(approvals).toEqual([{ request: { token: "tok-1", approve: true } }]);
    expect(dialog()).toBeNull();
    expect(container.textContent).toContain("Weather");
  });

  it("says when nothing is requested, and cancelling discards the package", async () => {
    const approvals: unknown[] = [];
    mockDaemon({
      pick_plugin_file: "/home/me/x.nuggetplugin",
      plugins_install: staged({ requested: { hosts: [], secrets: [], kv_max_bytes: 0 } }),
      plugins_approve: (args: unknown) => {
        approvals.push(args);
        return { plugin: null };
      },
    });
    const { container } = await render(<PluginsPage />);
    await click(byText(container, "button", "Install from file"));
    expect(dialog()?.textContent).toContain("This plugin asks for no access.");
    await click(byText(dialog()!, "button", "Cancel"));
    expect(approvals).toEqual([{ request: { token: "tok-1", approve: false } }]);
    expect(dialog()).toBeNull();
  });

  it("marks access that is new in an update", async () => {
    mockDaemon({
      pick_plugin_file: "/home/me/rss.nuggetplugin",
      plugins_install: staged({
        id: "rss",
        name: "RSS feeds",
        installed_version: "1.2.0",
        requested: { hosts: ["feeds.example.com", "news.example.net"], secrets: [], kv_max_bytes: 0 },
        previously_approved: { hosts: ["feeds.example.com"], secrets: [], kv_max_bytes: 0 },
      }),
    });
    const { container } = await render(<PluginsPage />);
    await click(byText(container, "button", "Install from file"));
    const d = dialog()!;
    expect(d.querySelector("h2")?.textContent).toBe("Update RSS feeds?");
    expect(d.textContent).toContain("Updates version 1.2.0 to 2.0.0");
    const items = Array.from(d.querySelectorAll("li"));
    expect(byText(d, "li", /news\.example\.net/)?.textContent).toContain("New");
    expect(items.find((li) => li.textContent?.includes("feeds.example.com"))?.textContent).not.toContain("New");
  });

  it("lists available plugins and installs one through the approval flow", async () => {
    const installs: unknown[] = [];
    mockDaemon({
      plugins_catalog: {
        entries: [
          entry(),
          entry({ path: "/opt/c/rss.nuggetplugin", file_name: "rss.nuggetplugin", id: "rss", name: "RSS feeds", version: "1.3.0", installed_version: "1.2.0", update_available: true, needs_approval: false, requested: { hosts: ["feeds.example.com"], secrets: [], kv_max_bytes: 0 } }),
          entry({ path: "/opt/c/bad.nuggetplugin", file_name: "bad.nuggetplugin", id: "", name: "", version: "", description: "", kinds: [], signature: "untrusted", signer: "", error: "plugin signature does not match any trusted key" }),
          entry({ path: "/srv/locked-catalog", file_name: "", id: "", name: "", version: "", description: "", kinds: [], signature: "untrusted", signer: "", error: "cannot read the folder: permission denied" }),
        ],
        dirs: ["/opt/c"],
        truncated: false,
      },
      plugins_install: (args: unknown) => {
        installs.push(args);
        return staged({ id: "dev.nugget.obsidian", name: "Obsidian vault", kinds: ["connector"], requested: { hosts: [], secrets: [], kv_max_bytes: 0, file_roots: ["vault"] } });
      },
      plugins_approve: { plugin: { ...plugins[0]!, id: "dev.nugget.obsidian", name: "Obsidian vault" } },
    });
    const { container } = await render(<PluginsPage />);
    expect(container.querySelector("#plugins-available-title")?.textContent).toBe("Available plugins");
    const list = availableList(container)!;
    const rows = Array.from(list.querySelectorAll("li.source"));
    expect(rows.map((r) => r.querySelector("h3")?.textContent)).toEqual(["bad.nuggetplugin", "locked-catalog", "Obsidian vault", "RSS feeds"]);

    // A package that can't be verified shows why, with no install button and no signer badge
    // (there is no verified signer to describe).
    expect(rows[0]!.textContent).toContain("Can't install bad.nuggetplugin: plugin signature does not match any trusted key");
    expect(rows[0]!.textContent).not.toContain("signer");
    expect(rows[0]!.querySelector("button")).toBeNull();
    expect(rows[0]!.querySelector('[role="status"]')).toBeNull();
    // An unreadable catalog folder is named by its last path segment.
    expect(rows[1]!.textContent).toContain("cannot read the folder");
    expect(rows[1]!.textContent).not.toContain("signer");
    rows.splice(1, 1);

    // File access is described in plain language.
    expect(rows[1]!.textContent).toContain("Trusted signer");
    expect(rows[1]!.textContent).toContain("Read files in the folders you choose in the source settings");
    // An older installed version offers an update.
    expect(rows[2]!.textContent).toContain("Update available");
    expect(rows[2]!.textContent).toContain("Updates version 1.2.0 to 1.3.0");
    expect(rows[2]!.querySelector("button")?.getAttribute("aria-label")).toBe("Update RSS feeds to version 1.3.0");

    const install = rows[1]!.querySelector("button")!;
    expect(install.getAttribute("aria-label")).toBe("Install Obsidian vault");
    await click(install);
    expect(installs).toEqual([{ request: { path: "/opt/nugget/plugin-catalog/obsidian.nuggetplugin" } }]);
    const d = dialog()!;
    expect(d.querySelector("h2")?.textContent).toBe("Install Obsidian vault?");
    expect(d.textContent).toContain("Read files in the folders you choose in the source settings");
    expect(d.textContent).not.toContain("This plugin asks for no access.");
    await click(byText(d, "button", "Allow and install"));
    expect(dialog()).toBeNull();
  });

  it("marks installed catalog entries and hides the section on older daemons", async () => {
    mockDaemon({
      plugins_catalog: { entries: [entry({ id: "rss", name: "RSS feeds", installed_version: "1.2.0", version: "1.2.0", needs_approval: false })] },
    });
    const { container } = await render(<PluginsPage />);
    const row = availableList(container)!.querySelector("li.source")!;
    expect(row.textContent).toContain("Installed");
    expect(row.querySelector("button")).toBeNull();
    cleanup();

    // `plugins_catalog` is not in the default fixtures: the daemon answers "method not found".
    mockDaemon();
    const { container: old } = await render(<PluginsPage />);
    expect(old.querySelector("#plugins-available-title")).toBeNull();
    expect(old.textContent).toContain("RSS feeds");
  });

  it("reloads the catalog after an install, an update and a removal", async () => {
    const catalogCalls = (calls: string[]) => calls.filter((c) => c === "plugins_catalog").length;
    const calls = mockDaemon({
      plugins_catalog: {
        entries: [
          entry(),
          entry({ path: "/opt/c/rss.nuggetplugin", file_name: "rss.nuggetplugin", id: "rss", name: "RSS feeds", version: "1.3.0", installed_version: "1.2.0", update_available: true, needs_approval: false, requested: { hosts: ["feeds.example.com"], secrets: [], kv_max_bytes: 0 } }),
        ],
      },
      plugins_install: (args: unknown) => {
        const path = (args as { request: { path: string } }).request.path;
        return path.endsWith("rss.nuggetplugin")
          ? staged({ id: "rss", name: "RSS feeds", version: "1.3.0", installed_version: "1.2.0", needs_approval: false, installed: true })
          : staged({ id: "dev.nugget.obsidian", name: "Obsidian vault" });
      },
      plugins_approve: { plugin: { ...plugins[0]!, id: "dev.nugget.obsidian", name: "Obsidian vault" } },
      plugins_remove: { removed: true },
    });
    const { container } = await render(<PluginsPage />);
    expect(catalogCalls(calls)).toBe(1);

    // Approved install.
    await click(availableList(container)!.querySelector<HTMLButtonElement>('button[aria-label="Install Obsidian vault"]')!);
    await click(byText(dialog()!, "button", "Allow and install"));
    expect(catalogCalls(calls)).toBe(2);

    // Update committed at once (no approval needed).
    await click(availableList(container)!.querySelector<HTMLButtonElement>('button[aria-label="Update RSS feeds to version 1.3.0"]')!);
    expect(dialog()).toBeNull();
    expect(catalogCalls(calls)).toBe(3);

    // Removal.
    const installedList = container.querySelector<HTMLElement>('ul[aria-label="Installed plugins"]')!;
    await click(installedList.querySelector<HTMLButtonElement>('button[aria-label="Remove RSS feeds"]')!);
    await click(byText(dialog()!, "button", "Remove"));
    expect(catalogCalls(calls)).toBe(4);
  });

  it("does nothing when the file picker is cancelled", async () => {
    const calls = mockDaemon({ pick_plugin_file: null });
    const { container } = await render(<PluginsPage />);
    await click(byText(container, "button", "Install from file"));
    expect(calls).not.toContain("plugins_install");
    expect(dialog()).toBeNull();
  });
});
