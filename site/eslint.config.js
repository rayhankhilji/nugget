import tseslint from "typescript-eslint";
import astro from "eslint-plugin-astro";

export default [
  { ignores: ["dist/", "dist-test/", "dist-bare/", ".astro/", "node_modules/", "tests/out/"] },
  ...tseslint.configs.recommended,
  ...astro.configs.recommended,
  {
    files: ["tests/**/*.mjs"],
    languageOptions: { globals: { process: "readonly", console: "readonly", URL: "readonly" } },
  },
];
