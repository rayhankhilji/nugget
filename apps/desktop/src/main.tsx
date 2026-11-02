import { App } from "./App";
import { inTauri } from "./api";
import { isMac } from "./hooks.shortcuts";
import { mount } from "./mount";
import "./styles.css";
import "./styles-a11y.css";

// macOS: the window has an overlay title bar and sidebar vibrancy (tauri.macos.conf.json).
if (inTauri() && isMac()) document.documentElement.classList.add("mac-window");

mount(<App />);
