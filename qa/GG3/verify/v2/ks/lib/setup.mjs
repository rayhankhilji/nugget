// Start the real nuggetd (apps/desktop/e2e/daemon.mjs's startDaemon) with the scripted mock model
// configured as an OpenAI-compatible provider on the agentic tier (the way a BYOK/LAN provider is
// configured in config.toml), plus the fake browser extension attached over IPC.
import fs from "node:fs";
import path from "node:path";
import { startDaemon, REPO } from "../../../../../../apps/desktop/e2e/daemon.mjs";
import { Ipc } from "./ipc.mjs";
import { startMockModel } from "./mockmodel.mjs";
import { startFakeExtension } from "./fakeext.mjs";

export { REPO };

export function config(port, { agentConfirmTimeoutSecs } = {}) {
  return `# Written by qa/GG3/killswitch/lib/setup.mjs
[runtime]
enabled = false

[mcp]
http = false

[agent]
${agentConfirmTimeoutSecs ? `confirm_timeout_secs = ${agentConfirmTimeoutSecs}` : "# confirm timeout from [policy]"}

[[providers]]
id = "qa-mock"
kind = "openai_compatible"
base_url = "http://127.0.0.1:${port}/v1"
model = "mock-agent"
tiers = ["agentic"]
max_retries = 0
`;
}

export async function setup({ outDir, bin = process.env.NUGGETD ?? path.join(REPO, "target/debug/nuggetd"), agentConfirmTimeoutSecs, ext = true } = {}) {
  fs.mkdirSync(outDir, { recursive: true });
  const mock = await startMockModel();
  const daemon = await startDaemon(bin, {
    stderrPath: path.join(outDir, "nuggetd.out.log"),
    config: config(mock.port, { agentConfirmTimeoutSecs }),
  });
  const ipc = new Ipc(daemon.socket);
  await ipc.connect();
  // Broadcasts reach every connection; make one call so the connection is registered.
  await ipc.rpc("status");
  const fake = ext ? await startFakeExtension(daemon.socket) : null;
  if (fake) {
    for (let i = 0; i < 50; i++) {
      const s = await ipc.rpc("browser.status");
      if (s.connected) break;
      await new Promise((r) => setTimeout(r, 50));
    }
  }
  return {
    mock,
    daemon,
    ipc,
    fake,
    async teardown() {
      fake?.close();
      ipc.close();
      await mock.close();
      await daemon.stop({ keep: !!process.env.KS_KEEP });
    },
  };
}
