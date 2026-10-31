// Smoke: the mock model drives the agent to a browser_submit confirmation, which is approved.
import path from "node:path";
import { fileURLToPath } from "node:url";
import { setup } from "./lib/setup.mjs";
import { agentEvents, isEvent } from "./lib/ipc.mjs";

const here = path.dirname(fileURLToPath(import.meta.url));
const env = await setup({ outDir: path.join(here, "logs", "smoke") });
const { ipc, mock, fake } = env;
try {
  console.log("browser.status", JSON.stringify(await ipc.rpc("browser.status")));
  console.log("providers", JSON.stringify(await ipc.rpc("providers.list")));
  mock.setScript([
    { calls: [{ name: "browser_snapshot", args: {} }] },
    { calls: [{ name: "browser_type", args: { element: "n2", text: "Synthetic hello from QA" } }] },
    { calls: [{ name: "browser_submit", args: { element: "n3" } }] },
    { calls: [{ name: "finish", args: { outcome: "done", summary: "sent" } }] },
  ]);
  const started = await ipc.rpc("agent.run", { goal: "Send the synthetic QA message", provider: "qa-mock" });
  console.log("agent.run", JSON.stringify(started));
  const id = started.task_id;
  const conf = await ipc.waitNote((n) => isEvent(id, "confirmation_needed")(n) || isEvent(id, "finished")(n), 20000, "confirmation");
  console.log("got", JSON.stringify(conf.params.event));
  if (conf.params.event.event === "confirmation_needed") {
    const r = await ipc.call("agent.confirm", { task_id: id, request_id: conf.params.event.request.request_id, choice: "once" });
    console.log("confirm", JSON.stringify(r));
  }
  await ipc.waitNote(isEvent(id, "finished"), 20000, "finished");
  for (const e of agentEvents(ipc, id)) console.log(e.event, JSON.stringify(e).slice(0, 300));
  console.log("ext acts", JSON.stringify(fake.st.acts));
  console.log("ext requests", JSON.stringify(fake.st.requests.map((r) => r.type)));
} finally {
  await env.teardown();
}
