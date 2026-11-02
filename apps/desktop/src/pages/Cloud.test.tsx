// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { emit } from "@tauri-apps/api/event";
import { act } from "react";
import { CloudError, cloudApi, type CloudAnswer, type CloudConnections, type CloudMe, type CloudPlan, type CloudSession } from "../cloud";
import { byText, cleanup, click, flush, installDomShims, mockDaemon, render } from "../test/render";
import { CloudPage } from "./Cloud";
import { AccountPage } from "./Account";

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

const ent = (plan: string, name: string, smart: number) => ({
  plan, name, max_connections: plan === "heed" ? 10 : null, sync_interval_minutes: 30, history_days: null, smart_asks_per_month: smart,
  default_model: plan === "heed" ? "fast" : "smart", devices: 1, ghost_text: true, needs_you_inbox: true, daily_brief: true,
  actions_with_approval: true, agent_tasks: false, computer_use: false, priority_support: false,
});
const plans: { plans: CloudPlan[] } = {
  plans: [
    { id: "heed", name: "Heed", monthly_gbp: 39, annual_gbp: 390, trial_days: 14, entitlements: ent("heed", "Heed", 300) },
    { id: "heed_max", name: "Heed Max", monthly_gbp: 99, annual_gbp: 990, trial_days: 14, entitlements: ent("heed_max", "Heed Max", 900) },
  ],
};
const unsubscribed: CloudMe = {
  user_id: "u1", email: "me@example.com", plan: null, embeddings: false, subscription: null, entitlements: null,
  usage: { month_cost_micros: 0, smart_asks_used: 0 },
};
const trialing: CloudMe = {
  ...unsubscribed,
  plan: "heed",
  subscription: { plan: "heed", status: "trialing", interval: "month", trial_end: "2026-11-16T12:00:00Z", current_period_end: "2026-11-16T12:00:00Z", cancel_at_period_end: false },
  entitlements: ent("heed", "Heed", 300),
  usage: { month_cost_micros: 1200, smart_asks_used: 7 },
};
/** The account and plans every signed-in page loads. */
const billing = { cloud_me: trialing, cloud_billing_plans: plans };

const answer: CloudAnswer = {
  conversation_id: "c1",
  answer: "Your flight is on Tuesday [1]. Not a cite [9].",
  citations: [{ n: 1, item_id: "i1", title: "Booking confirmed", source: "gmail", url: "https://mail.example/i1", occurred_at: "2026-10-30T09:00:00Z", snippet: "Flight BA1 departs", author: null }],
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
    expect(container.textContent).toContain("Heed Cloud isn't set up in this build");
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
    expect(container.querySelector("h1")?.textContent).toBe("Sign in to Heed Cloud");
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
    mockDaemon({ cloud_session: () => session, cloud_connections_list: lists, ...billing });
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
      ...billing,
      cloud_session: signedIn,
      cloud_connections_list: lists,
      cloud_connect: log("connect", null),
      cloud_disconnect: log("disconnect", null),
      cloud_sync: log("sync", { synced: [{ toolkit: "gmail", added: 3, updated: 1, error: null }, { toolkit: "googlecalendar", added: 0, updated: 0, error: "token expired" }] }),
    });
    const { container } = await render(<CloudPage />);
    const row = container.querySelector(".cloud-conn")!;
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
      ...billing,
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

describe("Plan & billing", () => {
  const section = (c: HTMLElement) => c.querySelector<HTMLElement>('section[aria-labelledby="cloud-billing"]')!;

  it("offers both plans with a trial when there is no subscription, and reloads after checkout", async () => {
    const calls: unknown[] = [];
    let me = unsubscribed;
    mockDaemon({
      cloud_session: signedIn,
      cloud_connections_list: lists,
      cloud_me: () => me,
      cloud_billing_plans: plans,
      cloud_billing_checkout: (args: unknown) => {
        calls.push(args);
        return null;
      },
    });
    const { container } = await render(<AccountPage />);
    const s = section(container);
    expect(s.querySelector("h2")?.textContent).toBe("Plan & billing");
    const items = s.querySelectorAll(".cloud-plans li");
    expect(items).toHaveLength(2);
    expect(items[0]!.textContent).toContain("£39 a month or £390 a year");
    expect(items[1]!.textContent).toContain("£99 a month or £990 a year");
    expect(items[1]!.textContent).toContain("14-day free trial");
    expect(byText(s, "button", "Manage billing")).toBeUndefined();

    await click(items[1]!.querySelector('[aria-label="Start a Heed Max trial, billed annually"]'));
    await click(items[0]!.querySelector('[aria-label="Start a Heed trial, billed monthly"]'));
    expect(calls).toEqual([
      { plan: "heed_max", interval: "year" },
      { plan: "heed", interval: "month" },
    ]);
    expect(s.textContent).toContain("Finish in your browser");

    me = trialing;
    await act(async () => {
      await emit("nugget:cloud-billing", null);
    });
    await flush();
    expect(s.querySelector(".cloud-plans")).toBeNull();
    expect(s.textContent).toContain("Heed");
  });

  it("shows the subscription, smart asks and the portal; Max upgrades through the portal", async () => {
    let portals = 0;
    let me: CloudMe = trialing;
    mockDaemon({
      cloud_session: signedIn,
      cloud_connections_list: lists,
      cloud_me: () => me,
      cloud_billing_plans: plans,
      cloud_billing_portal: () => {
        portals++;
        return null;
      },
    });
    const { container } = await render(<AccountPage />);
    let s = section(container);
    expect(s.textContent).toMatch(/Trial until .*2026/);
    expect(s.textContent).toContain("Smart asks this month: 7 of 300");
    await click(byText(s, "button", "Manage billing"));
    await click(byText(s, "button", "Upgrade to Heed Max"));
    expect(portals).toBe(2);
    cleanup();

    me = { ...trialing, plan: "heed_max", entitlements: ent("heed_max", "Heed Max", 900), subscription: { ...trialing.subscription!, plan: "heed_max", status: "past_due" } };
    mockDaemon({ cloud_session: signedIn, cloud_connections_list: lists, cloud_me: () => me, cloud_billing_plans: plans });
    s = section((await render(<AccountPage />)).container);
    expect(s.textContent).toContain("Heed Max");
    expect(s.textContent).toContain("Payment past due");
    expect(byText(s, "button", "Upgrade to Heed Max")).toBeUndefined();
    cleanup();

    me = { ...trialing, subscription: { ...trialing.subscription!, status: "active", cancel_at_period_end: true } };
    mockDaemon({ cloud_session: signedIn, cloud_connections_list: lists, cloud_me: () => me, cloud_billing_plans: plans });
    s = section((await render(<AccountPage />)).container);
    expect(s.textContent).toMatch(/Cancels on .*2026/);
  });

  it("points gated calls at billing and explains plan limits", async () => {
    const fail = (cloud: string) => () => {
      throw { kind: "rpc", message: "server says no", code: null, cloud };
    };
    mockDaemon({
      cloud_session: signedIn,
      cloud_connections_list: lists,
      cloud_me: unsubscribed,
      cloud_billing_plans: plans,
      cloud_sync: fail("subscription_required"),
      cloud_connect: fail("connection_limit_reached"),
      cloud_ask: fail("device_limit_reached"),
    });
    const { container } = await render(
      <>
        <CloudPage />
        <AccountPage />
      </>,
    );
    await click(byText(container, "button", "Sync now"));
    const prompt = container.querySelector('section[aria-labelledby="cloud-connections"] [role="alert"]')!;
    expect(prompt.textContent).toContain("This needs a Heed subscription");
    await click(byText(prompt as HTMLElement, "button", "See plans"));
    expect(document.activeElement?.id).toBe("cloud-billing");

    await click(byText(container.querySelector(".cloud-apps")!, "button", "Connect"));
    expect(container.textContent).toContain("Your plan's limit of connected apps is reached");

    const input = container.querySelector<HTMLInputElement>("#cloud-question")!;
    await type(input, "q");
    await submit(input.closest("form")!);
    expect(container.querySelector('section[aria-labelledby="cloud-ask"] [role="alert"]')?.textContent).toContain("Your plan's device limit is reached");
  });

  it("says when a smart ask fell back to the fast model", async () => {
    mockDaemon({ ...billing, cloud_session: signedIn, cloud_connections_list: lists, cloud_ask: { ...answer, model_tier: "fast", downgraded: true } });
    const { container } = await render(<CloudPage />);
    const input = container.querySelector<HTMLInputElement>("#cloud-question")!;
    await type(input, "q");
    await submit(input.closest("form")!);
    expect(container.querySelector(".cloud-answer")?.textContent).toContain("this month's smart asks are used up");
  });
});

describe("CloudPage background import", () => {
  it("shows import progress, says the import continues, and refreshes until it ends", async () => {
    let polls = 0;
    const importing: CloudConnections = {
      ...lists,
      connections: [{ id: "k1", toolkit: "gmail", status: "active", last_synced_at: null, last_error: null, syncing: true, backfill_done: false, items_synced: 1234 }],
    };
    const finished: CloudConnections = {
      ...lists,
      connections: [{ id: "k1", toolkit: "gmail", status: "active", last_synced_at: "2026-11-02T08:00:00Z", last_error: null, syncing: false, backfill_done: true, items_synced: 2000 }],
    };
    mockDaemon({
      cloud_session: signedIn,
      ...billing,
      cloud_connections_list: () => (++polls < 3 ? importing : finished),
      cloud_sync: { synced: [], running: true },
    });
    const { container } = await render(<CloudPage />);
    const row = () => container.querySelector(".cloud-conn")!;
    expect(row().textContent).toContain("Connected");
    expect(row().textContent).toContain("Importing… 1,234 items");
    await click(byText(container, "button", "Sync now"));
    expect(container.textContent).toContain("Importing in the background");
    await act(async () => {
      await new Promise((r) => setTimeout(r, 3200));
    });
    await flush(3);
    expect(row().textContent).not.toContain("Importing…");
    expect(polls).toBeGreaterThanOrEqual(3);
  }, 10_000);
});

describe("CloudPage self-hosted", () => {
  it("shows no plans or billing buttons when the server has billing off", async () => {
    const selfHosted: CloudMe = { ...trialing, billing: false, subscription: { ...trialing.subscription!, status: "active" } };
    mockDaemon({ cloud_session: signedIn, cloud_connections_list: lists, cloud_me: selfHosted, cloud_billing_plans: plans });
    const { container } = await render(<AccountPage />);
    const section = container.querySelector('section[aria-labelledby="cloud-billing"]')!;
    expect(section.textContent).toContain("self-hosted with billing off");
    expect(byText(section as HTMLElement, "button", "Manage billing")).toBeUndefined();
    expect(section.textContent).not.toContain("£39");
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
