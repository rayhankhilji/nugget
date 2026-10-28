// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import type { PluginInstallResult } from "../api";
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

  it("does nothing when the file picker is cancelled", async () => {
    const calls = mockDaemon({ pick_plugin_file: null });
    const { container } = await render(<PluginsPage />);
    await click(byText(container, "button", "Install from file"));
    expect(calls).not.toContain("plugins_install");
    expect(dialog()).toBeNull();
  });
});
