// Kill-switch / confirmation probes against the real nuggetd over IPC.
//   node scenarios.mjs [scenario ...]     (default: all)   KS_REPEAT=N (default 3)
// Writes logs/<scenario>.json (every run's events, replies, extension requests, timings) and prints
// a one-line verdict per run. See README.md.
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { setup } from "./lib/setup.mjs";
import { agentEvents, isEvent, now } from "./lib/ipc.mjs";

const here = path.dirname(fileURLToPath(import.meta.url));
const LOGS = path.join(here, "logs", "r2");
const REPEAT = Number(process.env.KS_REPEAT ?? 3);
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

const SEND_SCRIPT = [
  { calls: [{ name: "browser_snapshot", args: {} }] },
  { calls: [{ name: "browser_type", args: { element: "n2", text: "Synthetic QA body: quarterly numbers attached" } }] },
  { calls: [{ name: "browser_submit", args: { element: "n3" } }] },
  { calls: [{ name: "finish", args: { outcome: "done", summary: "sent" } }] },
];

let env;
async function fresh(opts = {}) {
  if (env) await env.teardown();
  env = await setup({ outDir: path.join(LOGS, "daemon"), ...opts });
  return env;
}

async function startTask(script, goal = "Send the synthetic QA message to qa@example.test", extra = {}) {
  env.mock.setScript(script);
  env.fake.reset();
  const r = await env.ipc.call("agent.run", { goal, provider: "qa-mock", ...extra });
  return r;
}

async function untilConfirm(id) {
  const n = await env.ipc.waitNote((x) => isEvent(id, "confirmation_needed")(x) || isEvent(id, "finished")(x), 20000, "confirmation_needed");
  if (n.params.event.event !== "confirmation_needed") throw new Error("finished before a confirmation: " + JSON.stringify(n.params.event));
  return n.params.event.request;
}

const finishedOf = (id, ms = 15000) => env.ipc.waitNote(isEvent(id, "finished"), ms, "finished").catch(() => null);

async function taskLog(id) {
  // The agent's own entries for the task (agent.get reads them back from the hash-chained log).
  const g = await env.ipc.rpc("agent.get", { task_id: id });
  return g.entries;
}

function summarizeLog(entries) {
  return entries.map((e) => {
    let s = `${e.kind}`;
    if (e.tool) s += `(${e.tool})`;
    if (e.decision) s += `:${typeof e.decision === "string" ? e.decision : e.decision.decision ?? e.decision.outcome ?? JSON.stringify(e.decision)}`;
    if (e.decision?.reason && /interrupted/.test(e.decision.reason)) s += `[${e.decision.reason.match(/\(interrupted[^)]*\)/)?.[0]}]`;
    if (e.kind === "executed") s += e.interrupted ? `:INTERRUPTED(${e.result})` : `:ran`;
    if (e.outcome) s += `:${e.outcome.outcome ?? e.outcome}`;
    return s;
  });
}

async function ensureResumed() {
  await env.ipc.rpc("policy.resume");
}

const submits = () => env.fake.st.acts.filter((a) => a.kind === "submit");

// ---------------------------------------------------------------------------------------------
const scenarios = {
  // 1. confirmation pending; Stop all agents (policy.kill); then approve it.
  async s1_kill_then_approve(rep) {
    await ensureResumed();
    const r = await startTask(SEND_SCRIPT);
    const id = r.result.task_id;
    const req = await untilConfirm(id);
    const kill = await env.ipc.call("policy.kill");
    const killNote = await env.ipc.waitNote((n) => n.method === "policy.kill.changed" && n.params.halted === true && n.t >= kill.t0 - 1, 3000, "kill.changed").catch(() => null);
    const fin = await finishedOf(id, 5000);
    const approve = await env.ipc.call("agent.confirm", { task_id: id, request_id: req.request_id, choice: "once" });
    await sleep(300);
    const get = await env.ipc.call("agent.get", { task_id: id });
    const approvals = await env.ipc.rpc("policy.approvals");
    return {
      task: id,
      request: req,
      kill_rpc_ms: +(kill.t - kill.t0).toFixed(1),
      kill_changed_note: !!killNote,
      kill_to_finished_ms: fin ? +(fin.t - kill.t).toFixed(1) : null,
      finished: fin?.params.event.outcome,
      approve_reply: approve.error ?? approve.result,
      submits_at_extension: submits().length,
      ext_requests_after_kill: env.fake.st.requests.filter((x) => x.t > kill.t0).map((x) => x.type),
      get_state: get.result?.summary?.state,
      get_outcome: get.result?.summary?.outcome,
      get_pending: get.result?.pending?.length,
      policy_pending: approvals.pending.length,
      events: agentEvents(env.ipc, id).map((e) => e.event + (e.answer ? `:${e.answer}` : "")),
      log: summarizeLog(await taskLog(id)),
      ok: !!fin && submits().length === 0 && !!approve.error,
    };
  },

  // 1b. same, but the Stop comes from the browser extension's page overlay / popup (user_stop event).
  async s1b_browser_stop_then_approve(rep) {
    await ensureResumed();
    const r = await startTask(SEND_SCRIPT);
    const id = r.result.task_id;
    const req = await untilConfirm(id);
    const t0 = now();
    await env.fake.userStop();
    const fin = await finishedOf(id, 5000);
    const approve = await env.ipc.call("agent.confirm", { task_id: id, request_id: req.request_id, choice: "once" });
    const pol = await env.ipc.rpc("policy.get");
    return {
      task: id,
      stop_to_finished_ms: fin ? +(fin.t - t0).toFixed(1) : null,
      finished: fin?.params.event.outcome,
      halted: pol.halted,
      approve_reply: approve.error ?? approve.result,
      submits_at_extension: submits().length,
      ok: !!fin && submits().length === 0 && !!approve.error && pol.halted,
    };
  },

  // 1c. race: kill and approve sent back to back (pipelined on one connection), both orders.
  async s1c_race_kill_approve(rep) {
    const out = [];
    for (const order of ["kill_first", "approve_first", "approve_first_gap5ms"]) {
      await ensureResumed();
      const r = await startTask(SEND_SCRIPT);
      const id = r.result.task_id;
      const req = await untilConfirm(id);
      const a = { task_id: id, request_id: req.request_id, choice: "once" };
      let kill, approve;
      if (order === "kill_first") {
        [kill, approve] = await Promise.all([env.ipc.call("policy.kill"), env.ipc.call("agent.confirm", a)]);
      } else if (order === "approve_first") {
        [approve, kill] = await Promise.all([env.ipc.call("agent.confirm", a), env.ipc.call("policy.kill")]);
      } else {
        const p = env.ipc.call("agent.confirm", a);
        await sleep(5);
        kill = await env.ipc.call("policy.kill");
        approve = await p;
      }
      const fin = await finishedOf(id, 5000);
      await sleep(200);
      const log = summarizeLog(await taskLog(id));
      const sub = submits();
      out.push({
        order,
        task: id,
        approve_reply: approve.error ? `error: ${approve.error.message}` : "accepted (null)",
        finished: fin?.params.event.outcome,
        events: agentEvents(env.ipc, id).map((e) => e.event + (e.answer ? `:${e.answer}` : "")).slice(-4),
        submits_at_extension: sub.length,
        submit_after_kill_reply: sub.filter((s) => s.t > kill.t).length,
        submit_after_kill_sent: sub.filter((s) => s.t > kill.t0).length,
        log_tail: log.slice(-4),
        ext_requests: env.fake.st.requests.map((x) => x.type + (x.action ? `:${x.action.kind}` : "") + `@${(x.t - kill.t0).toFixed(1)}ms`),
        extension_left_halted: env.fake.st.halted,
      });
    }
    return { runs: out, ok: out.every((o) => o.submit_after_kill_reply === 0) };
  },

  // 1d. policy.kill has RETURNED (halted: true); approve immediately after (10 tries per rep).
  async s1d_approve_right_after_kill_returned(rep) {
    const tries = [];
    for (let k = 0; k < 10; k++) {
      await ensureResumed();
      const r = await startTask(SEND_SCRIPT);
      const id = r.result.task_id;
      const req = await untilConfirm(id);
      const kill = await env.ipc.call("policy.kill");
      const approve = await env.ipc.call("agent.confirm", { task_id: id, request_id: req.request_id, choice: "once" });
      const fin = await finishedOf(id, 5000);
      await sleep(100);
      const log = summarizeLog(await taskLog(id));
      tries.push({
        kill_result: kill.result,
        approve_sent_after_kill_reply_ms: +(approve.t0 - kill.t).toFixed(2),
        approve_reply: approve.error ? `error: ${approve.error.message}` : "ACCEPTED (null)",
        resolved_event: agentEvents(env.ipc, id).find((e) => e.event === "confirmation_resolved")?.answer ?? "none",
        finished: fin?.params.event.outcome?.outcome,
        submits: submits().length,
        log_tail: log.slice(-3),
      });
    }
    const accepted = tries.filter((x) => x.approve_reply.startsWith("ACCEPTED")).length;
    return { accepted_after_kill: `${accepted}/10`, submits_total: tries.reduce((a, x) => a + x.submits, 0), tries, ok: tries.every((x) => x.submits === 0) && accepted === 0 };
  },

  // 2. kill, resume, then approve the stale confirmation.
  async s2_kill_resume_approve(rep) {
    await ensureResumed();
    const r = await startTask(SEND_SCRIPT);
    const id = r.result.task_id;
    const req = await untilConfirm(id);
    await env.ipc.call("policy.kill");
    const fin = await finishedOf(id, 5000);
    await env.ipc.call("policy.resume");
    const approve = await env.ipc.call("agent.confirm", { task_id: id, request_id: req.request_id, choice: "once" });
    const approveTask = await env.ipc.call("agent.confirm", { task_id: id, request_id: req.request_id, choice: "task" });
    await sleep(500);
    const approvals = await env.ipc.rpc("policy.approvals");
    return {
      task: id,
      finished: fin?.params.event.outcome,
      approve_once_reply: approve.error ?? approve.result,
      approve_task_reply: approveTask.error ?? approveTask.result,
      task_approvals_after: approvals.task,
      submits_at_extension: submits().length,
      model_requests: env.mock.state.requests.length,
      ok: submits().length === 0 && !!approve.error && approvals.task.length === 0,
    };
  },

  // 3a. kill while a tool is executing (the extension holds an `act` for 1500 ms), with two more
  // calls queued in the same model turn.
  async s3a_kill_during_tool(rep) {
    await ensureResumed();
    const script = [
      { calls: [{ name: "browser_snapshot", args: {} }] },
      {
        calls: [
          { name: "browser_type", args: { element: "n1", text: "qa@example.test" } },
          { name: "browser_type", args: { element: "n2", text: "Synthetic body" } },
          { name: "browser_click", args: { element: "n1" } },
        ],
      },
      { calls: [{ name: "browser_type", args: { element: "n2", text: "should never run" } }] },
    ];
    const r = await startTask(script);
    const id = r.result.task_id;
    env.fake.st.actDelayMs = { type: 1500 };
    const first = await waitFor(() => env.fake.st.acts[0], 15000).catch((e) => {
      throw new Error(`no act reached the extension; events: ${JSON.stringify(agentEvents(env.ipc, id).map((x) => x.event + ":" + (x.reason ?? x.summary ?? JSON.stringify(x.outcome ?? "")).slice(0, 160)))}`);
    });
    await sleep(200);
    const kill = await env.ipc.call("policy.kill");
    const fin = await finishedOf(id, 5000);
    await sleep(2000); // let the held act complete and anything else surface
    const acts = env.fake.st.acts;
    return {
      task: id,
      kill_rpc_ms: +(kill.t - kill.t0).toFixed(1),
      kill_to_finished_ms: fin ? +(fin.t - kill.t).toFixed(1) : null,
      finished: fin?.params.event.outcome,
      acts_started: acts.map((a) => `${a.kind}:${a.action.element}@${(a.t - kill.t).toFixed(0)}ms`),
      acts_started_after_kill: acts.filter((a) => a.t > kill.t0).length,
      first_act_completed_after_kill_ms: first.done ? +(first.done - kill.t).toFixed(0) : null,
      ext_requests_after_kill: env.fake.st.requests.filter((x) => x.t > kill.t0).map((x) => x.type),
      halt_sent_to_extension: env.fake.st.requests.some((x) => x.type === "halt"),
      model_requests: env.mock.state.requests.map((q) => q.turn),
      log_tail: summarizeLog(await taskLog(id)).slice(-4),
      ok: acts.filter((a) => a.t > kill.t0).length === 0,
    };
  },

  // 3b. kill during a slow model turn (the mock holds the stream for 3 s before a tool call).
  async s3b_kill_during_model_turn(rep) {
    await ensureResumed();
    const script = [
      { calls: [{ name: "browser_snapshot", args: {} }] },
      { delayMs: 3000, calls: [{ name: "browser_type", args: { element: "n2", text: "after slow turn" } }] },
      { calls: [{ name: "browser_submit", args: { element: "n3" } }] },
    ];
    const r = await startTask(script);
    const id = r.result.task_id;
    await waitFor(() => env.mock.state.requests.find((q) => q.turn === 1), 15000);
    await sleep(500);
    const kill = await env.ipc.call("policy.kill");
    const fin = await finishedOf(id, 5000);
    await sleep(3500);
    const turn1 = env.mock.state.requests.find((q) => q.turn === 1);
    return {
      task: id,
      kill_to_finished_ms: fin ? +(fin.t - kill.t).toFixed(1) : null,
      finished: fin?.params.event.outcome,
      model_stream_aborted_by_daemon: turn1.aborted,
      acts_after_kill: env.fake.st.acts.filter((a) => a.t > kill.t0).map((a) => a.kind),
      model_requests_after_kill: env.mock.state.requests.filter((q) => q.t > kill.t).length,
      events_tail: agentEvents(env.ipc, id).map((e) => e.event).slice(-3),
      ok: !!fin && env.fake.st.acts.filter((a) => a.t > kill.t0).length === 0,
    };
  },

  // 3c. kill latency under a busy loop: fast model + instant tools, kill at a random moment.
  async s3c_kill_busy_loop(rep) {
    await ensureResumed();
    const turn = (i) => ({ calls: [{ name: "browser_type", args: { element: "n2", text: `loop ${i}` } }] });
    const script = [{ calls: [{ name: "browser_snapshot", args: {} }] }, ...Array.from({ length: 28 }, (_, i) => turn(i))];
    const r = await startTask(script, "Type into the field repeatedly (synthetic)", { budgets: { max_steps: 30 } });
    const id = r.result.task_id;
    await waitFor(() => env.fake.st.acts.length >= 3, 15000);
    await sleep(Math.floor(Math.random() * 40));
    const kill = await env.ipc.call("policy.kill");
    const fin = await finishedOf(id, 5000);
    await sleep(300);
    const after = env.fake.st.acts.filter((a) => a.t > kill.t);
    const afterSent = env.fake.st.acts.filter((a) => a.t > kill.t0);
    return {
      task: id,
      acts_before_kill: env.fake.st.acts.length - afterSent.length,
      acts_started_between_kill_sent_and_reply: afterSent.length - after.length,
      acts_started_after_kill_reply: after.length,
      kill_rpc_ms: +(kill.t - kill.t0).toFixed(2),
      kill_to_finished_ms: fin ? +(fin.t - kill.t).toFixed(1) : null,
      finished: fin?.params.event.outcome,
      ok: after.length === 0,
    };
  },

  // 9. kill during a busy browser loop, Resume in the desktop app (policy.resume), run a fresh
  // browser task. Does the browser stay stopped although the daemon says agents may run?
  async s9_resume_vs_extension_halt(rep) {
    const tries = [];
    for (let k = 0; k < 10; k++) {
      await ensureResumed();
      env.fake.resume(); // the user also pressed Resume in the extension popup (clean start)
      const turn = (i) => ({ calls: [{ name: "browser_type", args: { element: "n2", text: `loop ${i}` } }] });
      const r = await startTask([{ calls: [{ name: "browser_snapshot", args: {} }] }, ...Array.from({ length: 28 }, (_, i) => turn(i))], "Type repeatedly (synthetic)");
      const id = r.result.task_id;
      await waitFor(() => env.fake.st.acts.length >= 2, 15000);
      await sleep(Math.floor(Math.random() * 15));
      await env.ipc.call("policy.kill");
      await finishedOf(id, 5000);
      const haltSent = env.fake.st.requests.some((x) => x.type === "halt");
      await env.ipc.call("policy.resume");
      const pol = await env.ipc.rpc("policy.get");
      // A fresh task after Resume (the extension popup was NOT touched).
      const r2 = await startTask([{ calls: [{ name: "browser_snapshot", args: {} }] }, { calls: [{ name: "finish", args: { outcome: "done", summary: "read" } }] }], "Read the page (synthetic)");
      const id2 = r2.result.task_id;
      const fin2 = await finishedOf(id2, 10000);
      const snapDone = agentEvents(env.ipc, id2).find((e) => e.event === "action_done" && e.tool === "browser_snapshot");
      tries.push({
        halt_pushed_to_extension_by_kill: haltSent,
        daemon_halted_after_resume: pol.halted,
        next_task_snapshot_result: snapDone?.summary?.slice(0, 120),
        next_task_finished: fin2?.params.event.outcome,
      });
    }
    const stuck = tries.filter((x) => x.halt_pushed_to_extension_by_kill).length;
    return { extension_halted_by_kill: `${stuck}/10`, tries, ok: tries.every((x) => !/kill switch is engaged/.test(x.next_task_snapshot_result ?? "")) };
  },

  // 10. approve and Stop pipelined (approve first), 30 tries: how often is the extension left
  // halted, and what does the next task (after Resume in the app) get?
  async s10_extension_left_halted(rep) {
    let halted = 0;
    const examples = [];
    for (let k = 0; k < 30; k++) {
      await ensureResumed();
      env.fake.resume();
      const r = await startTask(SEND_SCRIPT);
      const id = r.result.task_id;
      const req = await untilConfirm(id);
      const [, kill] = await Promise.all([
        env.ipc.call("agent.confirm", { task_id: id, request_id: req.request_id, choice: "once" }),
        env.ipc.call("policy.kill"),
      ]);
      await finishedOf(id, 5000);
      if (!env.fake.st.halted) continue;
      halted++;
      const haltReq = env.fake.st.requests.find((x) => x.type === "halt");
      await env.ipc.call("policy.resume");
      const pol = await env.ipc.rpc("policy.get");
      const r2 = await startTask([{ calls: [{ name: "browser_snapshot", args: {} }] }, { calls: [{ name: "finish", args: { outcome: "done", summary: "read" } }] }], "Read the page (synthetic)");
      const id2 = r2.result.task_id;
      const fin2 = await finishedOf(id2, 10000);
      if (examples.length < 3)
        examples.push({
          try: k + 1,
          halt_request_at_ms_after_kill_sent: +(haltReq.t - kill.t0).toFixed(2),
          first_task_log_tail: summarizeLog(await taskLog(id)).slice(-3),
          daemon_halted_after_resume: pol.halted,
          next_task_snapshot_result: agentEvents(env.ipc, id2).find((e) => e.event === "action_done" && e.tool === "browser_snapshot")?.summary,
          next_task_finished: fin2?.params.event.outcome,
          next_task_policy_still_resumed: !(await env.ipc.rpc("policy.get")).halted,
        });
    }
    env.fake.resume();
    return { extension_left_halted: `${halted}/30`, examples, ok: halted === 0 };
  },

  // 4. agent.run while killed; then resume.
  async s4_run_while_killed(rep) {
    await ensureResumed();
    await env.ipc.call("policy.kill");
    const n0 = env.mock.state.requests.length;
    const r = await startTask(SEND_SCRIPT);
    const id = r.result?.task_id;
    const fin = id ? await finishedOf(id, 5000) : null;
    const modelCallsWhileKilled = env.mock.state.requests.length;
    await env.ipc.call("policy.resume");
    await sleep(1500);
    const get = id ? await env.ipc.call("agent.get", { task_id: id }) : null;
    return {
      run_reply: r.error ?? { task_id: id },
      finished: fin?.params.event.outcome,
      run_to_finished_ms: fin ? +(fin.t - r.t).toFixed(1) : null,
      model_calls_while_killed: modelCallsWhileKilled - 0,
      model_calls_after_resume: env.mock.state.requests.length - modelCallsWhileKilled,
      acts: env.fake.st.acts.length,
      state_after_resume: get?.result?.summary?.state,
      ok: !!fin && env.fake.st.acts.length === 0 && env.mock.state.requests.length === 0,
    };
  },

  // 5a. approve the same confirmation twice (sequential, and two pipelined).
  async s5a_double_approve(rep) {
    await ensureResumed();
    const r = await startTask(SEND_SCRIPT);
    const id = r.result.task_id;
    const req = await untilConfirm(id);
    const a = { task_id: id, request_id: req.request_id, choice: "once" };
    const [x, y] = await Promise.all([env.ipc.call("agent.confirm", a), env.ipc.call("agent.confirm", a)]);
    const z = await env.ipc.call("agent.confirm", a);
    const fin = await finishedOf(id, 10000);
    return {
      replies: [x, y, z].map((q) => (q.error ? `error: ${q.error.message}` : "ok")),
      finished: fin?.params.event.outcome,
      submits_at_extension: submits().length,
      ok: submits().length === 1,
    };
  },

  // 5b. approve with a wrong task id (another live task's id, an unknown id).
  async s5b_wrong_task(rep) {
    await ensureResumed();
    const r1 = await startTask(SEND_SCRIPT);
    const id1 = r1.result.task_id;
    const req1 = await untilConfirm(id1);
    // a second task, parked on ask_user, to use as "another task id"
    env.mock.setScript([{ calls: [{ name: "ask_user", args: { question: "Which address?" } }] }]);
    const r2 = await env.ipc.call("agent.run", { goal: "Ask first (synthetic)", provider: "qa-mock" });
    const id2 = r2.result.task_id;
    await env.ipc.waitNote(isEvent(id2, "suspended"), 10000, "suspended");
    const other = await env.ipc.call("agent.confirm", { task_id: id2, request_id: req1.request_id, choice: "once" });
    const unknown = await env.ipc.call("agent.confirm", { task_id: "0000", request_id: req1.request_id, choice: "once" });
    await sleep(300);
    const subsBefore = submits().length;
    const deny = await env.ipc.call("agent.confirm", { task_id: id1, request_id: req1.request_id, choice: "deny" });
    await env.ipc.call("agent.cancel", { task_id: id2 });
    const fin = await finishedOf(id1, 15000);
    return {
      other_task_reply: other.error?.message ?? "ACCEPTED",
      unknown_task_reply: unknown.error?.message ?? "ACCEPTED",
      submits_before_right_answer: subsBefore,
      deny_reply: deny.error?.message ?? "ok",
      finished: fin?.params.event.outcome,
      submits_at_extension: submits().length,
      ok: !!other.error && !!unknown.error && submits().length === 0,
    };
  },

  // 5c. approve after agent.cancel (sequential, and pipelined right behind the cancel).
  async s5c_approve_after_cancel(rep) {
    const out = {};
    for (const mode of ["after", "pipelined"]) {
      await ensureResumed();
      const r = await startTask(SEND_SCRIPT);
      const id = r.result.task_id;
      const req = await untilConfirm(id);
      const a = { task_id: id, request_id: req.request_id, choice: "once" };
      let cancel, approve;
      if (mode === "after") {
        cancel = await env.ipc.call("agent.cancel", { task_id: id });
        await finishedOf(id, 5000);
        approve = await env.ipc.call("agent.confirm", a);
      } else {
        [cancel, approve] = await Promise.all([env.ipc.call("agent.cancel", { task_id: id }), env.ipc.call("agent.confirm", a)]);
      }
      const fin = await finishedOf(id, 5000);
      await sleep(300);
      out[mode] = {
        cancel_reply: cancel.error?.message ?? "ok",
        approve_reply: approve.error?.message ?? "accepted (null)",
        finished: fin?.params.event.outcome,
        submits_at_extension: submits().length,
        events_tail: agentEvents(env.ipc, id).map((e) => e.event + (e.answer ? `:${e.answer}` : "")).slice(-3),
      };
    }
    return { ...out, ok: out.after.submits_at_extension === 0 && out.pipelined.submits_at_extension === 0 };
  },

  // 5d. approve after the confirmation timed out (policy confirm_timeout_secs = 2).
  async s5d_approve_after_timeout(rep) {
    await ensureResumed();
    const pol = await env.ipc.rpc("policy.get");
    const cfg = { ...pol.config, confirm_timeout_secs: 2 };
    await env.ipc.rpc("policy.set", { config: cfg });
    try {
      const r = await startTask(SEND_SCRIPT);
      const id = r.result.task_id;
      const req = await untilConfirm(id);
      const resolved = await env.ipc.waitNote(isEvent(id, "confirmation_resolved"), 6000, "resolved").catch(() => null);
      const approve = await env.ipc.call("agent.confirm", { task_id: id, request_id: req.request_id, choice: "once" });
      const fin = await finishedOf(id, 10000);
      return {
        timeout_ms_in_request: req.timeout_ms,
        resolved_as: resolved?.params.event.answer,
        approve_reply: approve.error?.message ?? "ACCEPTED",
        finished: fin?.params.event.outcome,
        submits_at_extension: submits().length,
        ok: !!approve.error && submits().length === 0,
      };
    } finally {
      await env.ipc.rpc("policy.set", { config: { ...cfg, confirm_timeout_secs: pol.config.confirm_timeout_secs } });
    }
  },

  // 5e. "approve for this task", killed, resumed: is a standing approval left behind?
  async s5e_task_approval_and_kill(rep) {
    await ensureResumed();
    const script = [
      { calls: [{ name: "browser_snapshot", args: {} }] },
      { calls: [{ name: "browser_submit", args: { element: "n3" } }] },
      { calls: [{ name: "browser_submit", args: { element: "n3" } }] },
      { calls: [{ name: "finish", args: { outcome: "done", summary: "sent twice" } }] },
    ];
    const r = await startTask(script);
    const id = r.result.task_id;
    const req = await untilConfirm(id);
    // approve for task, then kill right after the first submit reaches the extension
    env.fake.st.actDelayMs = { submit: 800 };
    await env.ipc.call("agent.confirm", { task_id: id, request_id: req.request_id, choice: "task" });
    const a1 = await waitFor(() => submits()[0], 5000);
    const during = await env.ipc.rpc("policy.approvals");
    const kill = await env.ipc.call("policy.kill");
    const fin = await finishedOf(id, 5000);
    await env.ipc.call("policy.resume");
    await sleep(1500);
    const after = await env.ipc.rpc("policy.approvals");
    return {
      task_approvals_while_running: during.task,
      task_approvals_after_kill_resume: after.task,
      finished: fin?.params.event.outcome,
      submits: submits().length,
      submits_after_kill: submits().filter((s) => s.t > kill.t0).length,
      ok: after.task.length === 0 && submits().filter((s) => s.t > kill.t0).length === 0,
    };
  },

  // 7a. a task parked on ask_user survives Stop all agents; after Resume, answering it lets it act.
  async s7a_ask_user_survives_kill(rep) {
    await ensureResumed();
    const script = [
      { calls: [{ name: "ask_user", args: { question: "Send to qa@example.test?" } }] },
      { calls: [{ name: "browser_snapshot", args: {} }] },
      { calls: [{ name: "browser_type", args: { element: "n2", text: "typed after Stop all agents + Resume" } }] },
      { calls: [{ name: "finish", args: { outcome: "done", summary: "typed" } }] },
    ];
    const r = await startTask(script);
    const id = r.result.task_id;
    await env.ipc.waitNote(isEvent(id, "suspended"), 10000, "suspended");
    const kill = await env.ipc.call("policy.kill");
    await sleep(500);
    const whileKilled = await env.ipc.call("agent.get", { task_id: id });
    const list = await env.ipc.rpc("agent.list", { limit: 5 });
    await env.ipc.call("policy.resume");
    await sleep(300);
    const afterResume = await env.ipc.call("agent.get", { task_id: id });
    const ans = await env.ipc.call("agent.answer", { task_id: id, answer: "yes" });
    const fin = await finishedOf(id, 10000);
    return {
      state_while_killed: whileKilled.result?.summary?.state,
      listed_state_while_killed: list.find((x) => x.task_id === id)?.state,
      state_after_resume: afterResume.result?.summary?.state,
      answer_reply: ans.error?.message ?? "ok",
      finished: fin?.params.event.outcome,
      acts_after_kill: env.fake.st.acts.filter((a) => a.t > kill.t0).map((a) => `${a.kind}:${a.action.text ?? a.action.element}`),
      ok: env.fake.st.acts.filter((a) => a.t > kill.t0).length === 0,
    };
  },

  // 7b. answering an ask_user while killed.
  async s7b_answer_while_killed(rep) {
    await ensureResumed();
    const script = [
      { calls: [{ name: "ask_user", args: { question: "Which address?" } }] },
      { calls: [{ name: "browser_snapshot", args: {} }] },
      { calls: [{ name: "browser_type", args: { element: "n1", text: "qa@example.test" } }] },
    ];
    const r = await startTask(script);
    const id = r.result.task_id;
    await env.ipc.waitNote(isEvent(id, "suspended"), 10000, "suspended");
    await env.ipc.call("policy.kill");
    const n0 = env.mock.state.requests.length;
    const ans = await env.ipc.call("agent.answer", { task_id: id, answer: "qa@example.test" });
    const fin = await finishedOf(id, 5000);
    return {
      answer_reply: ans.error?.message ?? "ok",
      finished: fin?.params.event.outcome,
      model_calls_after_answer: env.mock.state.requests.length - n0,
      acts: env.fake.st.acts.length,
      ok: fin?.params.event.outcome?.outcome === "halted" && env.fake.st.acts.length === 0,
    };
  },

  // 8. computer-use (accessibility) tools on Linux: a click on an element id never observed is
  // Send risk (fail-safe) and asks; kill + approve; and, separately, approve without kill.
  async s8_computer_use_linux(rep) {
    await ensureResumed();
    const script = [
      { calls: [{ name: "click", args: { element: "e7" } }] },
      { calls: [{ name: "observe", args: {} }] },
      { calls: [{ name: "finish", args: { outcome: "cannot", summary: "no desktop" } }] },
    ];
    const out = {};
    for (const mode of ["kill_then_approve", "approve"]) {
      await ensureResumed();
      const r = await startTask(script, "Click the Send button in the mail app (synthetic)");
      const id = r.result.task_id;
      const req = await untilConfirm(id);
      if (mode === "kill_then_approve") await env.ipc.call("policy.kill");
      const ap = await env.ipc.call("agent.confirm", { task_id: id, request_id: req.request_id, choice: "once" });
      const fin = await finishedOf(id, 30000);
      const get = await env.ipc.call("agent.get", { task_id: id });
      out[mode] = {
        tools_offered: r.result.tools.filter((t) => !t.startsWith("browser_")),
        request_action: req.action,
        request_reason: req.reason,
        approve_reply: ap.error?.message ?? "accepted",
        finished: fin?.params.event.outcome,
        executed_results: agentEvents(env.ipc, id)
          .filter((e) => e.event === "action_done" || e.event === "action_denied")
          .map((e) => `${e.tool}: ${e.summary ?? e.reason}`.slice(0, 220)),
        log_tail: summarizeLog(get.result?.entries ?? []).slice(-5),
      };
    }
    await ensureResumed();
    return { ...out, ok: !out.kill_then_approve.executed_results.some((s) => s.startsWith("click")) };
  },
};

function waitFor(fn, ms) {
  const end = Date.now() + ms;
  return new Promise((resolve, reject) => {
    const tick = () => {
      const v = fn();
      if (v) return resolve(v);
      if (Date.now() > end) return reject(new Error("waitFor timeout"));
      setTimeout(tick, 5);
    };
    tick();
  });
}

const wanted = process.argv.slice(2).length ? process.argv.slice(2) : Object.keys(scenarios);
fs.mkdirSync(LOGS, { recursive: true });
const summary = {};
try {
  await fresh();
  for (const name of wanted) {
    const runs = [];
    for (let i = 0; i < REPEAT; i++) {
      let res;
      try {
        res = await scenarios[name](i);
      } catch (e) {
        res = { error: String(e.stack ?? e) };
        await fresh(); // a wedged daemon must not poison the next run
      }
      runs.push(res);
      console.log(`${name} #${i + 1}: ${res.error ? "ERROR " + res.error.split("\n")[0] : res.ok ? "as expected" : "UNEXPECTED"} ${JSON.stringify(res).slice(0, 600)}`);
    }
    summary[name] = runs;
    fs.writeFileSync(path.join(LOGS, `${name}.json`), JSON.stringify({ commit: process.env.KS_COMMIT ?? null, runs }, null, 2));
  }
} finally {
  await env?.teardown();
}
