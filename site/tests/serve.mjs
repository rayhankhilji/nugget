// A tiny static server for dist/, resolving /path to /path.html like Cloudflare Pages does.
import { createServer } from "node:http";
import { gzipSync } from "node:zlib";
import { readFile, stat } from "node:fs/promises";
import { extname, join, normalize } from "node:path";
import { fileURLToPath } from "node:url";

const DIST = process.env.ASTRO_OUT_DIR
  ? join(fileURLToPath(new URL("../", import.meta.url)), process.env.ASTRO_OUT_DIR, "/")
  : fileURLToPath(new URL("../dist/", import.meta.url));
const TYPES = {
  ".html": "text/html; charset=utf-8",
  ".css": "text/css",
  ".js": "text/javascript",
  ".svg": "image/svg+xml",
  ".png": "image/png",
  ".woff2": "font/woff2",
  ".xml": "application/xml",
  ".txt": "text/plain; charset=utf-8",
};

async function resolve(path, DIST) {
  const clean = normalize(decodeURIComponent(path)).replace(/^(\.\.[/\\])+/, "");
  for (const p of [clean, `${clean}.html`, join(clean, "index.html")]) {
    const full = join(DIST, p);
    try {
      if ((await stat(full)).isFile()) return full;
    } catch {
      /* try next */
    }
  }
  return null;
}

export function serve(port = 4173, dir = DIST) {
  const server = createServer(async (req, res) => {
    const url = new URL(req.url ?? "/", "http://x");
    const file = await resolve(url.pathname, dir);
    const body = await readFile(file ?? join(dir, "404.html"));
    const type = file ? (TYPES[extname(file)] ?? "application/octet-stream") : TYPES[".html"];
    const cache = file && /\/(_astro|fonts)\//.test(file) ? "public, max-age=31536000, immutable" : "no-cache";
    // Compress text like the production host (Cloudflare Pages) does, so Lighthouse sees real transfer sizes.
    const text = /^(text\/|application\/(json|xml)|image\/svg)/.test(type);
    const gz = text && /\bgzip\b/.test(String(req.headers["accept-encoding"] ?? ""));
    res.writeHead(file ? 200 : 404, {
      "Content-Type": type,
      "Cache-Control": cache,
      ...(gz ? { "Content-Encoding": "gzip", Vary: "Accept-Encoding" } : {}),
    });
    res.end(gz ? gzipSync(body) : body);
  });
  return new Promise((r) => server.listen(port, "127.0.0.1", () => r({ server, base: `http://127.0.0.1:${port}` })));
}

export const ROUTES = ["/", "/pricing", "/download", "/signin", "/signup", "/account", "/checkout", "/legal/privacy", "/legal/terms", "/privacy", "/docs/self-host", "/about", "/no-such-page"];
