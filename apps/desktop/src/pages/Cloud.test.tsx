// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { emit } from "@tauri-apps/api/event";
import { act } from "react";
import { CloudError, cloudApi, type CloudAnswer, type CloudConnections, type CloudSession } from "../cloud";
import { byText, cleanup, click, flush, installDomShims, mockDaemon, render } from "../test/render";
import { CloudPage } from "./Cloud";

beforeEach(installDomShims);
afterEach(cleanup);

const signedIn: CloudSession = { configured: true, signed_in: true, email: "me@example.com" };
const signedOut: CloudSession = { configured: true, signed_in: false, email: null };

const lists: CloudConnections = {
  connections: [{ id: "k1", toolkit: "gmail", status: "ACTIVE", last_synced_at: null, last_error: null }],
  available: [
    { slug: "gmail", name: "Gmail", logo: null, connectable: true },
    { slug: "googlecalendar", name: "Google Calendar", logo: null, connectable: true },
    { slug: "slack", name: "Slack", logo: null, connectable: false },
  ],
};

const answer: CloudAnswer = {
  conversation_id: "c1",
  answer: "Your flight is on Tuesday [1]. Not a cite [9].",
  citations: [{ n: 1, item_id: "i1", title: "Booking confirmed", source: "gmail", url: "https://mail.example/i1", occurred_at: "2026-10-30T09:00:00Z", snippet: "Flight BA1 departs" }],
  usage: { input_tokens: 10, output_tokens: 5 },
};

async function type(input: HTMLInputElement, value: string) {
  await act(async () => {
    const set = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")!.set!;
    set.call(input, value);
    input.dispatchEvent(new Event("input", { bubbles: true }));
  });
}

async function submit(form: HTMLFormElement) {
  await act(async () => {
    form.requestSubmit();
  });
  await flush(3);
}

describe("CloudPage", () => {
  it("says so when this build has no cloud", async () => {
    mockDaemon({ cloud_session: { configured: false, signed_in: false, email: null } });
    const { container } = await render(<CloudPage />);
    expect(container.textContent).toContain("Nugget Cloud isn't set up in this build");
    expect(byText(container, "button", "Continue with Google")).toBeUndefined();
  });

  it("gates on sign-in: providers open the browser, email sends a magic link", async () => {
    const calls: unknown[] = [];
    mockDaemon({
      cloud_session: signedOut,
      cloud_sign_in: (args: unknown) => {
        calls.push(args);
        return null;
      },
    });
    const { container } = await render(<CloudPage />);
    expect(container.querySelector("h1")?.textContent).toBe("Sign in to Nugget Cloud");
    await click(byText(container, "button", "Continue with Google"));
    expect(container.textContent).toContain("Finish signing in in your browser");
    expect(byText(container, "button", "Continue with Apple")).toBeUndefined();
    await type(container.querySelector<HTMLInputElement>("#cloud-email")!, "me@example.com");
    await submit(container.querySelector("form")!);
    expect(calls).toEqual([{ provider: "google" }, { email: "me@example.com" }]);
    expect(container.textContent).toContain("we sent a sign-in link to me@example.com");
  });

  it("shows a failed sign-in from the session event, then the signed-in page after a success", async () => {
    let session = signedOut;
    mockDaemon({ cloud_session: () => session, cloud_connections_list: lists });
    const { container } = await render(<CloudPage />);
    await act(async () => {
      await emit("nugget:cloud-session", { signed_in: false, error: "User cancelled" });
    });
    await flush();
    expect(container.textContent).toContain("Sign-in didn't finish: User cancelled");
    session = signedIn;
    await act(async () => {
      await emit("nugget:cloud-session", { signed_in: true, error: null });
    });
    await flush();
    expect(container.textContent).toContain("Signed in as me@example.com");
  });

  it("lists connections, connects only connectable apps, syncs and disconnects", async () => {
    const calls: [string, unknown][] = [];
    const log = (cmd: string, r: unknown) => (args: unknown) => {
      calls.push([cmd, args]);
      return r;
    };
    mockDaemon({
      cloud_session: signedIn,
      cloud_connections_list: lists,
      cloud_connect: log("connect", null),
      cloud_disconnect: log("disconnect", null),
      cloud_sync: log("sync", { synced: [{ toolkit: "gmail", added: 3, updated: 1, error: null }, { toolkit: "googlecalendar", added: 0, updated: 0, error: "token expired" }] }),
    });
    const { container } = await render(<CloudPage />);
    const row = container.querySelector("tbody tr")!;
    expect(row.textContent).toContain("Gmail");
    expect(row.textContent).toContain("Connected");
    expect(row.textContent).toContain("never");

    const apps = container.querySelector(".cloud-apps")!;
    expect(byText(apps, "button", "Connect")?.getAttribute("aria-label")).toBe("Connect Google Calendar");
    expect(apps.querySelectorAll("button")).toHaveLength(1);
    expect(apps.textContent).toContain("Coming soon");
    await click(byText(apps, "button", "Connect"));
    expect(container.textContent).toContain("Finish connecting Google Calendar in your browser");

    await click(byText(container, "button", "Sync now"));
    const results = container.querySelector(".cloud-sync-results")!;
    expect(results.textContent).toContain("Gmail: 3 new, 1 updated");
    expect(results.textContent).toContain("Google Calendar: token expired");

    await click(byText(row, "button", "Disconnect"));
    expect(calls).toEqual([
      ["connect", { toolkit: "googlecalendar" }],
      ["sync", {}],
      ["disconnect", { id: "k1" }],
    ]);
  });

  it("asks with citations, keeps the conversation and shows quota errors", async () => {
    const asks: unknown[] = [];
    let quota = false;
    const opened: unknown[] = [];
    mockDaemon({
      cloud_session: signedIn,
      cloud_connections_list: lists,
      cloud_ask: (args: unknown) => {
        asks.push(args);
        if (quota) throw { kind: "rpc", message: "You've used this month's AI allowance.", code: null, cloud: "quota_exceeded" };
        return answer;
      },
      open_url: (args: unknown) => {
        opened.push(args);
        return "opened";
      },
    });
    const { container } = await render(<CloudPage />);
    const input = container.querySelector<HTMLInputElement>("#cloud-question")!;
    const form = input.closest("form")!;
    await type(input, "When is my flight?");
    await submit(form);
    const text = container.querySelector(".cloud-answer p")!;
    expect(text.textContent).toBe("Your flight is on Tuesday [1]. Not a cite [9].");
    // Only real citations become buttons.
    expect(text.querySelectorAll("button.cite")).toHaveLength(1);
    await click(text.querySelector("button.cite"));
    const item = container.querySelector<HTMLElement>("#cloud-cite-1")!;
    expect(document.activeElement).toBe(item);
    expect(item.textContent).toContain("Booking confirmed");
    expect(item.textContent).toContain("gmail");
    await click(byText(item, "button", "Open"));
    expect(opened).toEqual([{ url: "https://mail.example/i1" }]);

    quota = true;
    await type(input, "And the return?");
    await submit(form);
    expect(asks).toEqual([{ question: "When is my flight?" }, { question: "And the return?", conversationId: "c1" }]);
    expect(container.querySelector('[role="alert"]')?.textContent).toContain("You've used this month's AI allowance.");
  });
});

describe("cloudApi", () => {
  it("keeps the cloud error code", async () => {
    mockDaemon({
      cloud_sync: () => {
        throw { kind: "rpc", message: "not signed in to Nugget Cloud", code: null, cloud: "not_signed_in" };
      },
    });
    const err = await cloudApi.sync().catch((e: unknown) => e);
    expect(err).toBeInstanceOf(CloudError);
    expect((err as CloudError).notSignedIn).toBe(true);
    expect((err as CloudError).message).toBe("not signed in to Nugget Cloud");
  });
});
