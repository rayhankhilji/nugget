// macOS driver: macos-helper.swift (CGEvent, CGWindowList, Accessibility), compiled with swiftc,
// with TextEdit on a scratch file as the target. The read-back is TextEdit's text view value
// through Accessibility. Screenshots with screencapture.
import { execFileSync, spawn } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { startHelper } from "./helper.mjs";

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const TEXTEDIT = "com.apple.TextEdit";
let helper = null;

/** The overlay's size in points (`OVERLAY_SIZE` in src-tauri/src/doubletap/mod.rs). */
const OVERLAY = { w: 440, h: 200 };

export const macos = {
  name: "macos",
  targetName: "TextEdit",
  triggerLabel: "double tap Right Command (CGEvent)",
  appPid: 0,

  async start(log) {
    const bin = path.join(fs.mkdtempSync(path.join(os.tmpdir(), "nugget-e2e-native-")), "macos-helper");
    log("compiling macos-helper.swift");
    execFileSync("swiftc", ["-O", "-o", bin, path.join(import.meta.dirname, "macos-helper.swift")], { stdio: "inherit" });
    helper = startHelper(bin, [], { log });
    log(`helper: front ${JSON.stringify(await helper.call("front"))}`);
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
    execFileSync("/usr/bin/open", ["-a", "TextEdit", file]);
    log(`opened TextEdit on ${file}`);
    return {
      // foreground() starts with the app name.
      title: "TextEdit |",
      initial: "",
      read: async () => (await helper.call(`read ${TEXTEDIT}`)).text,
      close: async () => {
        spawn("/usr/bin/pkill", ["-9", "-x", "TextEdit"], { stdio: "ignore" });
        await sleep(300);
        fs.rmSync(file, { force: true });
      },
    };
  },

  async activate() {
    try {
      await helper.call(`activate ${TEXTEDIT}`);
    } catch {
      // Best effort; foreground() tells.
    }
  },

  /** "<app name> | <focused window title> [pid N]". */
  async foreground() {
    const f = await helper.call("front");
    return `${f.name ?? ""} | ${f.window ?? ""} [pid ${f.pid ?? 0}]`;
  },

  /** The app is frontmost (AXFrontmost) with the overlay as its focused window. */
  async overlayIsForeground() {
    const f = await helper.call(`focus ${this.appPid}`);
    return f.frontmost && f.window === "Nugget Double Tap";
  },

  async trigger() {
    await helper.call("trigger");
  },

  async enter() {
    await helper.call("enter");
  },

  async click(x, y) {
    await helper.call(`click ${x} ${y}`);
  },

  /** The app's on-screen windows of the overlay's size (titles need Screen Recording). */
  async overlayWindows() {
    const r = await helper.call(`windows ${this.appPid}`);
    return r.windows
      .filter((w) => Math.abs(w.w - OVERLAY.w) <= 2 && Math.abs(w.h - OVERLAY.h) <= 2 && w.alpha > 0)
      .map((w) => ({ ...w, title: w.title || "Nugget Double Tap", visible: true, ax_titles: r.ax_titles }));
  },

  async screenshot(file, rect) {
    const args = ["-x"];
    if (rect) args.push(`-R${Math.round(rect.x)},${Math.round(rect.y)},${Math.round(rect.w)},${Math.round(rect.h)}`);
    execFileSync("/usr/sbin/screencapture", [...args, file], { stdio: "ignore" });
  },
};
