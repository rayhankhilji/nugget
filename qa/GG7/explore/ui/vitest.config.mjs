// GG7 UI probe config: run from apps/desktop so its node_modules and src resolve.
//   cd apps/desktop && npx vitest run --config ../../qa/GG7/explore/ui/vitest.config.mjs
import { fileURLToPath } from "node:url";
const here = fileURLToPath(new URL(".", import.meta.url));
const desk = fileURLToPath(new URL("../../../../apps/desktop/", import.meta.url));
const nm = (p) => desk + "node_modules/" + p;
export default {
  root: desk,
  resolve: { alias: { react: nm("react"), "react-dom": nm("react-dom"), vitest: nm("vitest"), "@tauri-apps/api": nm("@tauri-apps/api") } },
  esbuild: { jsx: "automatic" },
  server: { fs: { allow: [desk, here] } },
  test: { include: [here + "*.probe.tsx"], environment: "jsdom" },
};
