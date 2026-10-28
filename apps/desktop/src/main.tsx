import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { App } from "./App";
import { initI18n } from "./i18n";
import "./styles.css";

const root = document.getElementById("root");
if (!root) throw new Error("missing #root");
// Load the user's language first so the window doesn't flash English. A failed load still renders.
void initI18n()
  .catch(() => {})
  .finally(() =>
    createRoot(root).render(
      <StrictMode>
        <App />
      </StrictMode>,
    ),
  );
