import { mount } from "../mount";
import { Overlay } from "./Overlay";
import { Ghost } from "./Ghost";
import "../styles.css";
import "./overlay.css";
import "../styles-a11y.css";

// One entry for both windows: `overlay.html#ghost` is the ghost text at the caret, plain
// `overlay.html` the Double Tap panel (the fallback where the caret can't be placed).
const ghost = window.location.hash === "#ghost";
if (ghost) {
  document.documentElement.classList.add("ghost-root");
  document.body.classList.add("ghost-body");
}
mount(ghost ? <Ghost /> : <Overlay />);
