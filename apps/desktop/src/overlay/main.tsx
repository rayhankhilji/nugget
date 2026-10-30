import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { initI18n } from "../i18n";
import { Overlay } from "./Overlay";
import "../styles.css";
import "./overlay.css";
import "../styles-a11y.css";

const root = document.getElementById("root");
if (!root) throw new Error("missing #root");
// Same locale as the main window (shared storage); a failed load still renders English.
void initI18n()
  .catch(() => {})
  .finally(() =>
    createRoot(root).render(
      <StrictMode>
        <Overlay />
      </StrictMode>,
    ),
  );
