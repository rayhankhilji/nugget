// Windows driver: windows-helper.ps1 (SendInput, EnumWindows, UI Automation, System.Drawing),
// with Notepad on a scratch file as the target. The read-back is Notepad's own text through UIA.
import { spawn } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { startHelper } from "./helper.mjs";

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
let helper = null;

export const windows = {
  name: "windows",
  targetName: "Notepad",
  triggerLabel: "double tap Right Ctrl (SendInput)",

  async start(log) {
    helper = startHelper("powershell.exe", ["-NoProfile", "-ExecutionPolicy", "Bypass", "-File", path.join(import.meta.dirname, "windows-helper.ps1")], { log });
    // The first call waits for the C# block to compile.
    await helper.call("foreground", 120_000);
  },
  async stop() {
    helper?.stop();
  },

  appEnv() {
    return {};
  },

  async openTarget(log) {
    const file = path.join(os.tmpdir(), `nugget-overlay-${process.pid}.txt`);
    fs.writeFileSync(file, "");
    const proc = spawn("notepad.exe", [file], { stdio: "ignore", detached: false });
    const title = path.basename(file);
    log(`opened Notepad on ${file}`);
    return {
      title,
      initial: "",
      read: async () => (await helper.call(`read ${title}`)).text.replace(/\r\n/g, "\n"),
      close: async () => {
        spawn("taskkill", ["/F", "/IM", "notepad.exe"], { stdio: "ignore" });
        proc.kill();
        await sleep(300);
        fs.rmSync(file, { force: true });
      },
    };
  },

  async activate(target) {
    try {
      await helper.call(`activate ${target.title}`);
    } catch {
      // Best effort; foreground() tells.
    }
  },

  async foreground() {
    return (await helper.call("foreground")).title ?? "";
  },

  async trigger() {
    await helper.call("trigger");
  },

  async enter() {
    await helper.call("enter");
  },

  async click(x, y) {
    await helper.call(`click ${Math.round(x)} ${Math.round(y)}`);
  },

  async overlayWindows() {
    return (await helper.call("windows Nugget Double Tap")).windows ?? [];
  },

  async screenshot(file, rect) {
    const r = rect ? ` ${rect.x} ${rect.y} ${rect.w} ${rect.h}` : "";
    await helper.call(`screenshot ${file}${r}`);
  },
};
