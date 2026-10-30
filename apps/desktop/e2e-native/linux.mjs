// Linux (X11, Xvfb) driver: keys and clicks through XTest (`xte`, from xautomation: it presses
// exactly the key asked for, where `xdotool key Control_R` also presses Control_L, which cancels
// a double tap as a real chord would), the window list through xdotool, screenshots with ImageMagick `import`, and the GTK 3 fixture app of nugget-a11y's
// AT-SPI tests as the target text field. The fixture prints its composer's text on stdout
// whenever it changes ("COMPOSER <json>"), which is the read-back: what the widget itself holds,
// independent of the accessibility API the app inserted through.
//
// Expects DISPLAY, a window manager (openbox) and a session bus; see run-linux.sh.
import { execFileSync, spawn, spawnSync } from "node:child_process";
import path from "node:path";
import readline from "node:readline";
import { REPO } from "../e2e/daemon.mjs";

const FIXTURE = path.join(REPO, "crates", "nugget-a11y", "tests", "fixtures", "gtk_test_app.py");
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

function xdotool(...args) {
  return execFileSync("xdotool", args, { encoding: "utf8", stdio: ["ignore", "pipe", "pipe"] }).trim();
}

function xte(...commands) {
  execFileSync("xte", commands, { stdio: "ignore" });
}

/** A Python that can import GTK 3 through PyGObject. */
function python() {
  for (const c of [process.env.NUGGET_E2E_PYTHON, "python3", "/usr/bin/python3", "python3.12", "python3.11"]) {
    if (!c) continue;
    const r = spawnSync(c, ["-c", "import gi; gi.require_version('Gtk', '3.0'); from gi.repository import Gtk"], { stdio: "ignore" });
    if (r.status === 0) return c;
  }
  throw new Error("no python with PyGObject and GTK 3 (install python3-gi gir1.2-gtk-3.0)");
}

export const linux = {
  name: "linux",
  targetName: "GTK fixture",
  /** The trigger the app recommends on X11: double tap Right Ctrl. */
  triggerLabel: "double tap Right Ctrl (XTest)",

  async start() {},
  async stop() {},

  appEnv(home) {
    // Keep the app's own config (doubletap.json, window state) inside the run's temp home.
    return { XDG_CONFIG_HOME: path.join(home, "xdg-config"), XDG_DATA_HOME: path.join(home, "xdg-data"), GTK_MODULES: "gail:atk-bridge", NO_AT_BRIDGE: "0" };
  },

  async openTarget(log) {
    const proc = spawn(python(), [FIXTURE, "composer"], {
      stdio: ["ignore", "pipe", "inherit"],
      env: { ...process.env, GTK_MODULES: "gail:atk-bridge", NO_AT_BRIDGE: "0" },
    });
    let composer = null;
    let ready = false;
    readline.createInterface({ input: proc.stdout }).on("line", (l) => {
      if (l === "READY") ready = true;
      else if (l.startsWith("COMPOSER ")) composer = JSON.parse(l.slice(9));
    });
    const deadline = Date.now() + 20_000;
    while (!ready) {
      if (Date.now() > deadline) throw new Error("the GTK fixture did not print READY within 20 s");
      if (proc.exitCode !== null) throw new Error(`the GTK fixture exited (${proc.exitCode})`);
      await sleep(50);
    }
    log("GTK fixture ready (composer focused, text \"Hello \")");
    return {
      title: "Nugget IT - chat",
      initial: "Hello ",
      read: async () => {
        await sleep(100);
        return composer ?? "Hello ";
      },
      close: async () => proc.kill(),
    };
  },

  async activate(target) {
    try {
      xdotool("search", "--name", `^${target.title}$`, "windowactivate", "--sync");
    } catch {
      // Best effort; foreground() tells.
    }
  },

  async foreground() {
    try {
      return xdotool("getactivewindow", "getwindowname");
    } catch {
      return "";
    }
  },

  async trigger() {
    const tap = ["keydown Control_R", "usleep 30000", "keyup Control_R", "usleep 60000"];
    xte(...tap, ...tap);
  },

  async enter() {
    xte("key Return");
  },

  async click(x, y) {
    xte(`mousemove ${Math.round(x)} ${Math.round(y)}`, "usleep 50000", "mouseclick 1");
  },

  /** Visible windows titled like the overlay, with their geometry. */
  async overlayWindows() {
    let ids = [];
    try {
      ids = xdotool("search", "--onlyvisible", "--name", "^Nugget Double Tap$").split("\n").filter(Boolean);
    } catch {
      return [];
    }
    return ids.map((id) => {
      const g = Object.fromEntries(
        xdotool("getwindowgeometry", "--shell", id)
          .split("\n")
          .map((l) => l.split("="))
          .filter((p) => p.length === 2),
      );
      return { id, title: "Nugget Double Tap", visible: true, x: +g.X, y: +g.Y, w: +g.WIDTH, h: +g.HEIGHT };
    });
  },

  async screenshot(file, rect) {
    const args = ["-window", "root"];
    if (rect) args.push("-crop", `${rect.w}x${rect.h}+${rect.x}+${rect.y}`, "+repage");
    execFileSync("import", [...args, file], { stdio: "ignore" });
  },
};
