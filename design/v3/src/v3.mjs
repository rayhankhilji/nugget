// v3 mockups: macOS-native craft, neutral brand slot, ghost-text frames.
// node v3.mjs <outdir>; then node shoot.mjs <outdir>
import fs from "node:fs";
import path from "node:path";
import { createRequire } from "node:module";
import { V3 } from "./v3pal.mjs";

const HERE = path.dirname(new URL(import.meta.url).pathname);
const OUT = process.argv[2];
const NM = path.join(HERE, "fonts/node_modules");
const si = createRequire(path.join(NM, "x.js"))("simple-icons");

const FONTS = `@font-face{font-family:"Inter";src:url("file://${NM}/@fontsource-variable/inter/files/inter-latin-opsz-normal.woff2") format("woff2");font-weight:100 900;font-display:block}
@font-face{font-family:"Source Serif 4";src:url("file://${NM}/@fontsource-variable/source-serif-4/files/source-serif-4-latin-wght-normal.woff2") format("woff2");font-weight:200 900;font-display:block}`;

// ---------- icons ----------
const I = {
  home: '<path d="M4 11.5 12 5l8 6.5V19a1 1 0 0 1-1 1h-4.5v-5h-5v5H5a1 1 0 0 1-1-1Z"/>',
  inbox: '<path d="M4 13.5 6.5 5.5h11l2.5 8V19a1 1 0 0 1-1 1H5a1 1 0 0 1-1-1Z"/><path d="M4 13.5h4.5l1 2h5l1-2H20"/>',
  search: '<circle cx="11" cy="11" r="6.5"/><path d="m16 16 4 4"/>',
  plug: '<path d="M9 4v4M15 4v4M6.5 8h11v3a5.5 5.5 0 0 1-11 0Z"/><path d="M12 16.5V20"/>',
  up: '<path d="M12 19V6M6.5 11.5 12 6l5.5 5.5"/>',
  at: '<circle cx="12" cy="12" r="3.5"/><path d="M15.5 9v4.5a2.5 2.5 0 0 0 5 0V12a8.5 8.5 0 1 0-3.5 6.9"/>',
  clip: '<path d="m19 11.5-6.8 6.8a4.5 4.5 0 0 1-6.4-6.4l7.1-7.1a3 3 0 0 1 4.2 4.2l-7 7a1.5 1.5 0 0 1-2.1-2.1l6.4-6.4"/>',
  check: '<path d="m5 12.5 4.5 4.5L19 7.5"/>',
  cal: '<rect x="4" y="5.5" width="16" height="14.5" rx="2"/><path d="M4 10h16M8.5 3.5v4M15.5 3.5v4"/>',
  doc: '<path d="M7 3.5h7l4 4V20H6.5V4Z"/><path d="M14 3.5V8h4"/>',
  spark: '<path d="M12 4c.6 4.2 3.8 7.4 8 8-4.2.6-7.4 3.8-8 8-.6-4.2-3.8-7.4-8-8 4.2-.6 7.4-3.8 8-8Z"/>',
  gear: '<circle cx="12" cy="12" r="3"/><path d="M12 3.5v2.5M12 18v2.5M3.5 12H6M18 12h2.5M6 6l1.8 1.8M16.2 16.2 18 18M6 18l1.8-1.8M16.2 7.8 18 6"/>',
  sidebar: '<rect x="3.5" y="5" width="17" height="14" rx="2.5"/><path d="M9.5 5v14"/>',
  plus: '<path d="M12 5v14M5 12h14"/>',
  reply: '<path d="M10 7 5 12l5 5"/><path d="M5 12h9a5 5 0 0 1 5 5v1"/>',
  clock: '<circle cx="12" cy="12" r="8"/><path d="M12 8v4.5l3 1.8"/>',
  lock: '<rect x="5.5" y="10.5" width="13" height="9.5" rx="2"/><path d="M8.5 10.5V8a3.5 3.5 0 0 1 7 0v2.5"/>',
  bold: '<path d="M7 5h6a3.5 3.5 0 0 1 0 7H7Zm0 7h7a3.5 3.5 0 0 1 0 7H7Z"/>',
  hash: '<path d="M9 4 7 20M17 4l-2 16M4.5 9h15M3.5 15h15"/>',
  more: '<circle cx="6" cy="12" r="1"/><circle cx="12" cy="12" r="1"/><circle cx="18" cy="12" r="1"/>',
  send: '<path d="M4.5 12 19 5l-4 14-3.5-5.5Z"/><path d="m11.5 13.5 3-3"/>',
  arrowR: '<path d="M5 12h13M13 6l6 6-6 6"/>',
};
const ic = (n, s = 16, cls = "") => `<svg class="ic ${cls}" width="${s}" height="${s}" viewBox="0 0 24 24" aria-hidden="true">${I[n]}</svg>`;
const kbd = (...k) => `<span class="keys">${k.map((x) => `<kbd>${x}</kbd>`).join("")}</span>`;

// Real app logos (simple-icons, CC0). Slack and Microsoft logos are excluded from simple-icons at the
// owners' request; the shipped app takes every logo from Composio's catalogue instead.
const SI = (slug) => si["si" + slug[0].toUpperCase() + slug.slice(1)];
const logo = (slug, s = 28) => {
  const i = SI(slug);
  const g = Math.round(s * 0.58);
  return `<span class="logo" style="width:${s}px;height:${s}px;border-radius:${Math.round(s * 0.26)}px"><svg width="${g}" height="${g}" viewBox="0 0 24 24" aria-hidden="true"><path fill="#${i.hex}" d="${i.path}"/></svg></span>`;
};

// Neutral brand slot: the name and logo are TBD. One accent token (--accent) carries the brand.
const MARK = (s = 22) => `<span class="mark" style="width:${s}px;height:${s}px;border-radius:${Math.round(s * 0.28)}px"><i></i></span>`;

const AV = { MT: ["#E7A15A", "#C2563A"], LP: ["#7FB2F0", "#4767C9"], IH: ["#9AD5A8", "#2F8F6B"], SO: ["#C9A3F0", "#7A55C9"], HF: ["#F0C36B", "#C98A2E"], RP: ["#F29BB0", "#C2456B"] };
const av = (k, s = 28) => `<span class="av" style="width:${s}px;height:${s}px;font-size:${Math.round(s * 0.38)}px;background:linear-gradient(160deg,${AV[k][0]},${AV[k][1]})">${k}</span>`;

const vars = (p) => Object.entries(p).map(([k, v]) => `--${k.replace(/[A-Z]/g, (c) => "-" + c.toLowerCase())}:${v};`).join("");

const CSS = (t) => {
  const d = t === "dark";
  return `${FONTS}
:root{color-scheme:${t};${vars(V3[t])}
--font:"Inter","IBM Plex Sans Arabic","Noto Sans Devanagari",-apple-system,BlinkMacSystemFont,"Hiragino Sans","PingFang SC",sans-serif;
--read:"Source Serif 4","Noto Naskh Arabic","Noto Serif Devanagari",Georgia,serif;
--t-11:11px;--t-12:12px;--t-13:13px;--t-15:15px;--t-17:17px;--t-22:22px;--t-28:28px;--t-40:40px;
--s-1:4px;--s-2:8px;--s-3:12px;--s-4:16px;--s-5:20px;--s-6:24px;--s-7:32px;--s-8:48px;
--r-xs:5px;--r-sm:7px;--r-md:10px;--r-lg:14px;--r-xl:18px;
--hair:${d ? "rgb(255 255 255 / .08)" : "rgb(0 0 0 / .08)"};
--hair-strong:${d ? "rgb(255 255 255 / .12)" : "rgb(0 0 0 / .12)"};
--hi:${d ? "inset 0 1px 0 rgb(255 255 255 / .06)" : "inset 0 1px 0 rgb(255 255 255 / .9)"};
--sh-1:0 0 0 .5px ${d ? "rgb(0 0 0 / .6)" : "rgb(0 0 0 / .1)"}, 0 1px 2px rgb(0 0 0 / ${d ? .3 : .06});
--sh-2:0 0 0 .5px ${d ? "rgb(0 0 0 / .6)" : "rgb(0 0 0 / .1)"}, 0 2px 4px rgb(0 0 0 / ${d ? .25 : .04}), 0 8px 24px rgb(0 0 0 / ${d ? .3 : .07});
--sh-3:0 0 0 .5px ${d ? "rgb(0 0 0 / .7)" : "rgb(0 0 0 / .12)"}, 0 4px 12px rgb(0 0 0 / ${d ? .3 : .08}), 0 24px 60px rgb(0 0 0 / ${d ? .5 : .18});
--sidebar-bg:${d ? "rgb(38 38 42 / .72)" : "rgb(236 236 240 / .74)"};
--ghost:${d ? "rgb(242 242 243 / .38)" : "rgb(29 29 31 / .36)"};
--wall:${d ? "radial-gradient(900px 600px at 15% 10%, #3a2f6b, transparent 60%), radial-gradient(900px 700px at 90% 90%, #0f4a5c, transparent 60%), #0b0b10"
          : "radial-gradient(900px 600px at 10% 0%, #f7d9c4, transparent 60%), radial-gradient(900px 700px at 100% 100%, #c9dcf7, transparent 60%), #eae6f2"};
}
*{box-sizing:border-box}html,body{margin:0}
body{width:1280px;height:800px;overflow:hidden;background:var(--wall);font:400 var(--t-13)/1.38 var(--font);color:var(--text);-webkit-font-smoothing:antialiased;font-feature-settings:"cv11","ss03";letter-spacing:-.003em}
.ic{fill:none;stroke:currentColor;stroke-width:1.6;stroke-linecap:round;stroke-linejoin:round;flex:none}
.keys{display:inline-flex;gap:3px}
kbd{font:500 var(--t-11)/1 var(--font);min-width:18px;height:18px;padding:0 4px;display:inline-grid;place-items:center;border-radius:var(--r-xs);color:var(--muted);
 background:linear-gradient(${d ? "#2E2E33,#26262A" : "#FFFFFF,#F2F2F4"});box-shadow:0 0 0 .5px var(--hair-strong), 0 1px 0 ${d ? "rgb(0 0 0 / .5)" : "rgb(0 0 0 / .08)"}, var(--hi)}
.logo{display:inline-grid;place-items:center;background:#fff;box-shadow:0 0 0 .5px rgb(0 0 0 / .12), 0 1px 2px rgb(0 0 0 / .08);flex:none}
.mark{display:inline-grid;place-items:center;flex:none;background:linear-gradient(160deg,${d ? "#3A3A40,#1F1F23" : "#FFFFFF,#E4E4E9"});box-shadow:0 0 0 .5px var(--hair-strong), var(--hi), 0 1px 2px rgb(0 0 0 / .15)}
.mark i{width:38%;height:38%;border-radius:50%;background:var(--accent);box-shadow:0 0 8px var(--accent)}
.av{display:inline-grid;place-items:center;border-radius:50%;color:#fff;font-weight:600;flex:none;box-shadow:inset 0 0 0 .5px rgb(0 0 0 / .15), inset 0 1px 0 rgb(255 255 255 / .25)}
/* window */
.win{position:absolute;inset:20px;border-radius:12px;overflow:hidden;display:grid;grid-template-columns:228px 1fr;background:transparent;box-shadow:0 0 0 .5px ${d ? "rgb(0 0 0 / .9)" : "rgb(0 0 0 / .22)"}, 0 30px 80px rgb(0 0 0 / ${d ? .55 : .25}), 0 10px 24px rgb(0 0 0 / ${d ? .3 : .1})}
.win::after{content:"";position:absolute;inset:0;border-radius:12px;box-shadow:inset 0 0 0 1px ${d ? "rgb(255 255 255 / .09)" : "rgb(255 255 255 / .5)"};pointer-events:none}
.win.bare{grid-template-columns:1fr;background:var(--bg)}
.lights{position:absolute;top:18px;left:18px;display:flex;gap:8px;z-index:3}
.lights i{width:12px;height:12px;border-radius:50%;box-shadow:inset 0 0 0 .5px rgb(0 0 0 / .2)}
.lights i:nth-child(1){background:#FF5F57}.lights i:nth-child(2){background:#FEBC2E}.lights i:nth-child(3){background:#28C840}
/* sidebar (vibrancy) */
.side{background:var(--sidebar-bg);backdrop-filter:blur(40px) saturate(1.8);border-right:.5px solid var(--hair-strong);padding:52px 10px 10px;display:flex;flex-direction:column;gap:1px}
.side .top{position:absolute;top:14px;left:150px;display:flex;gap:2px;color:var(--muted)}
.side .top span{width:26px;height:24px;display:grid;place-items:center;border-radius:var(--r-sm)}
.find{display:flex;align-items:center;gap:var(--s-2);height:28px;padding:0 6px 0 8px;margin:0 0 var(--s-3);border-radius:var(--r-sm);background:${d ? "rgb(255 255 255 / .06)" : "rgb(0 0 0 / .045)"};color:var(--muted);box-shadow:inset 0 0 0 .5px var(--hair)}
.find span{flex:1;white-space:nowrap}
.nav{display:flex;align-items:center;gap:var(--s-2);height:28px;padding:0 var(--s-2);border-radius:var(--r-sm);color:var(--text);font-weight:500}
.nav .ic{color:var(--muted)}
.nav.on{background:${d ? "rgb(255 255 255 / .1)" : "rgb(0 0 0 / .07)"}}
.nav.on .ic{color:var(--accent-text)}
.nav .n{margin-left:auto;font:500 var(--t-11)/1 var(--font);color:var(--muted);font-feature-settings:"tnum"}
.side h6{margin:var(--s-5) var(--s-2) var(--s-1);font:600 var(--t-11)/1 var(--font);color:var(--muted);letter-spacing:.01em}
.th{height:26px;line-height:26px;display:block;padding:0 var(--s-2);border-radius:var(--r-sm);color:var(--muted);white-space:nowrap;overflow:hidden;text-overflow:ellipsis}
.side .grow{flex:1}
.me{display:flex;align-items:center;gap:var(--s-2);padding:6px;border-radius:var(--r-md)}
.me b{font-weight:500;display:block}.me small{color:var(--muted);font-size:var(--t-11)}
.me .keys{margin-left:auto}
/* content */
.main{position:relative;display:flex;flex-direction:column;min-width:0;background:var(--bg)}
.bar{height:52px;flex:none;display:flex;align-items:center;gap:var(--s-2);padding:0 var(--s-4) 0 var(--s-5);border-bottom:.5px solid var(--hair)}
.bar h1{font:600 var(--t-13)/1 var(--font);margin:0}
.bar .sp{flex:1}
.tb{height:28px;display:inline-flex;align-items:center;gap:6px;padding:0 var(--s-2);border-radius:var(--r-sm);color:var(--muted);font-weight:500}
.btn{height:28px;display:inline-flex;align-items:center;gap:var(--s-2);padding:0 var(--s-3);border-radius:var(--r-sm);font:500 var(--t-13)/1 var(--font);color:var(--text);background:${d ? "linear-gradient(#34343A,#2C2C31)" : "linear-gradient(#FFFFFF,#F7F7F9)"};box-shadow:var(--sh-1), var(--hi)}
.btn.pri{color:var(--on-accent);background:linear-gradient(color-mix(in srgb, var(--accent) 88%, white), var(--accent));box-shadow:0 0 0 .5px color-mix(in srgb, var(--accent) 70%, black), 0 1px 2px rgb(0 0 0 / .2), inset 0 1px 0 rgb(255 255 255 / .22)}
.btn.pri kbd{background:rgb(255 255 255 / .18);color:#fff;box-shadow:none}
.btn.ghost{background:transparent;box-shadow:none;color:var(--muted)}
.btn.hover{background:${d ? "rgb(255 255 255 / .07)" : "rgb(0 0 0 / .05)"};color:var(--text)}
.btn.press{transform:scale(.97);filter:brightness(.92)}
.focus{box-shadow:0 0 0 1px var(--focus), 0 0 0 4px color-mix(in srgb, var(--focus) 30%, transparent) !important}
/* home */
.home{flex:1;overflow:hidden;padding:56px 0 0}
.col{width:640px;margin:0 auto}
.greet{font:600 var(--t-28)/1.15 var(--font);letter-spacing:-.022em;margin:0 0 4px;font-variation-settings:"opsz" 32}
.date{color:var(--muted);margin-bottom:var(--s-6)}
.comp{border-radius:var(--r-xl);background:${d ? "var(--panel)" : "var(--bg)"};box-shadow:var(--sh-2), var(--hi);padding:14px 14px 10px 16px}
.comp .ph{font-size:var(--t-15);color:var(--faint);min-height:44px;letter-spacing:-.01em}
.comp .typed{font-size:var(--t-15);color:var(--text);min-height:44px;letter-spacing:-.01em}
.caret{display:inline-block;width:1.5px;height:18px;background:var(--accent);vertical-align:-3px;margin-left:1px}
.comp .row{display:flex;align-items:center;gap:4px}
.chip{height:26px;display:inline-flex;align-items:center;gap:6px;padding:0 var(--s-2);border-radius:var(--r-sm);color:var(--muted);font-weight:500}
.chip.hover{background:${d ? "rgb(255 255 255 / .06)" : "rgb(0 0 0 / .045)"};color:var(--text)}
.stack{display:inline-flex}.stack{margin-left:2px}.stack>*{margin-left:-3px;box-shadow:0 0 0 1.5px ${d ? "var(--panel)" : "var(--bg)"}, 0 1px 2px rgb(0 0 0 / .1) !important}.stack>*:first-child{margin-left:0}
.send{margin-left:auto;width:30px;height:30px;border-radius:50%;display:grid;place-items:center;color:var(--on-accent);background:linear-gradient(color-mix(in srgb, var(--accent) 85%, white), var(--accent));box-shadow:0 0 0 .5px color-mix(in srgb, var(--accent) 70%, black), 0 1px 3px rgb(0 0 0 / .25), inset 0 1px 0 rgb(255 255 255 / .25);position:relative}
.send.off{background:${d ? "#2E2E33" : "#EDEDF0"};color:var(--faint);box-shadow:var(--hi)}
.send.sending::before{content:"";position:absolute;inset:-6px;border-radius:50%;box-shadow:0 0 0 2px color-mix(in srgb, var(--accent) 35%, transparent)}
.send.sending i{width:9px;height:9px;border-radius:2px;background:#fff}
.sugg{display:flex;gap:6px;margin:var(--s-3) 0 0;flex-wrap:wrap}
.sugg span{height:28px;display:inline-flex;align-items:center;gap:6px;padding:0 10px;border-radius:999px;color:var(--muted);box-shadow:inset 0 0 0 .5px var(--hair-strong);font-weight:500}
.sugg span.hover{color:var(--text);background:${d ? "rgb(255 255 255 / .05)" : "rgb(0 0 0 / .03)"}}
.sugg .ic{color:var(--accent-text)}
.sec{margin-top:40px}
.sec header{display:flex;align-items:center;gap:var(--s-2);margin-bottom:var(--s-3)}
.sec h2{font:600 var(--t-13)/1 var(--font);margin:0}
.sec header small{color:var(--muted)}
.sec header .sp{flex:1}
.sec header a{color:var(--accent-text);font-weight:500}
.prose p{font:400 var(--t-17)/1.6 var(--read);margin:0 0 var(--s-3);letter-spacing:-.003em}
.cite{display:inline-flex;align-items:center;gap:3px;height:18px;padding:0 5px 0 3px;border-radius:5px;background:${d ? "rgb(255 255 255 / .07)" : "rgb(0 0 0 / .045)"};font:500 var(--t-11)/1 var(--font);color:var(--muted);vertical-align:2px;margin-left:2px}
.cite .logo{width:13px!important;height:13px!important;border-radius:3px!important;box-shadow:none}
.agenda{display:grid;gap:0;margin-top:var(--s-2)}
.ev{display:grid;grid-template-columns:64px 3px 1fr auto;gap:var(--s-3);align-items:center;height:44px;border-top:.5px solid var(--hair)}
.ev .tm{color:var(--muted);font-feature-settings:"tnum";font-size:var(--t-12)}
.ev .bar2{height:24px;border-radius:2px;background:var(--accent)}
.ev .bar2.n{background:var(--control);opacity:.5}
.ev b{font-weight:500}.ev small{color:var(--muted);margin-left:6px}
.sk{border-radius:6px;background:linear-gradient(90deg, var(--raised) 0%, ${d ? "#2D2D32" : "#F7F7F9"} 40%, var(--raised) 80%);height:14px;margin:0 0 12px}
/* needs you */
.split{flex:1;display:grid;grid-template-columns:360px 1fr;min-height:0}
.list{border-right:.5px solid var(--hair);overflow:hidden;padding:6px}
.seg{display:flex;gap:2px;padding:3px;margin:6px 6px 8px;border-radius:var(--r-md);background:${d ? "rgb(255 255 255 / .05)" : "rgb(0 0 0 / .04)"}}
.seg span{flex:1;height:24px;display:grid;place-items:center;border-radius:var(--r-sm);color:var(--muted);font-weight:500;font-size:var(--t-12)}
.seg span.on{background:${d ? "#3A3A40" : "#fff"};color:var(--text);box-shadow:var(--sh-1), var(--hi)}
.it{position:relative;display:grid;grid-template-columns:34px 1fr auto;gap:1px 10px;padding:10px 10px;border-radius:var(--r-md)}
.it .avw{position:relative;grid-row:span 3;align-self:start;height:34px}
.it .avw .logo{position:absolute;right:-3px;bottom:-3px;box-shadow:0 0 0 2px var(--bg)}
.it b{font-weight:600}.it .t{color:var(--muted);font-size:var(--t-12);font-feature-settings:"tnum"}
.it .s{grid-column:2/4;white-space:nowrap;overflow:hidden;text-overflow:ellipsis}
.it .y{grid-column:2/4;color:var(--muted);font-size:var(--t-12);white-space:nowrap;overflow:hidden;text-overflow:ellipsis}
.it.sel{background:var(--accent-soft)}
.it.sel .y,.it.sel .t{color:${d ? "#A9C6F5" : "#3F5E91"}}
.it.hov{background:${d ? "rgb(255 255 255 / .04)" : "rgb(0 0 0 / .03)"}}
.it .dot{position:absolute;left:-1px;top:22px;width:6px;height:6px;border-radius:50%;background:var(--accent)}
.det{display:flex;flex-direction:column;min-width:0}
.det .body{flex:1;padding:28px 40px 0;overflow:hidden}
.from{display:flex;align-items:center;gap:10px;margin-bottom:var(--s-4)}
.from b{font-weight:600}.from small{display:block;color:var(--muted);font-size:var(--t-12)}
.from .t{margin-left:auto;color:var(--muted);font-size:var(--t-12)}
.det h1{font:600 var(--t-22)/1.2 var(--font);letter-spacing:-.018em;margin:0 0 var(--s-3)}
.q{font:400 var(--t-15)/1.55 var(--read);color:var(--muted);margin:0 0 var(--s-4)}
.why{display:flex;gap:var(--s-2);align-items:flex-start;padding:10px 12px;border-radius:var(--r-md);background:var(--panel);box-shadow:inset 0 0 0 .5px var(--hair);color:var(--muted);margin-bottom:var(--s-4)}
.why .ic{color:var(--accent-text);margin-top:1px}.why b{color:var(--text);font-weight:500}
.draft{border-radius:var(--r-lg);background:${d ? "var(--panel)" : "var(--bg)"};box-shadow:var(--sh-2), var(--hi);padding:14px 16px}
.draft .lbl{display:flex;align-items:center;gap:6px;font:600 var(--t-11)/1 var(--font);color:var(--muted);letter-spacing:.01em;margin-bottom:10px}
.draft .lbl .sp{flex:1}
.draft p{font:400 var(--t-15)/1.55 var(--read);margin:0 0 6px}
.actbar{height:44px;flex:none;display:flex;align-items:center;gap:var(--s-2);padding:0 var(--s-3) 0 var(--s-4);border-top:.5px solid var(--hair);background:var(--panel)}
.actbar .sp{flex:1}
.actbar .hint{display:inline-flex;align-items:center;gap:6px;color:var(--muted);font-weight:500;padding:0 6px}
.actbar .div{width:.5px;height:16px;background:var(--hair-strong)}
/* empty */
.empty{flex:1;display:grid;place-items:center}
.empty .in{text-align:center;width:420px}
.ring{width:64px;height:64px;margin:0 auto 20px;border-radius:50%;display:grid;place-items:center;color:var(--good);background:radial-gradient(circle, color-mix(in srgb, var(--good) 16%, transparent), transparent 70%);box-shadow:inset 0 0 0 .5px color-mix(in srgb, var(--good) 40%, transparent), 0 0 0 10px color-mix(in srgb, var(--good) 5%, transparent), 0 0 0 22px color-mix(in srgb, var(--good) 3%, transparent)}
.empty h2{font:600 var(--t-22)/1.2 var(--font);letter-spacing:-.018em;margin:0 0 6px}
.empty p{color:var(--muted);margin:0 0 var(--s-6)}
.next{display:grid;text-align:left;border-radius:var(--r-lg);box-shadow:inset 0 0 0 .5px var(--hair-strong);overflow:hidden}
.next div{display:flex;align-items:center;gap:10px;height:40px;padding:0 12px;border-top:.5px solid var(--hair)}
.next div:first-child{border-top:0}
.next .ic{color:var(--muted)}.next .keys{margin-left:auto}
/* cmdk */
.scrim{position:absolute;inset:0;background:${d ? "rgb(0 0 0 / .35)" : "rgb(0 0 0 / .12)"}}
.pal{position:absolute;left:50%;top:84px;transform:translateX(-50%);width:680px;border-radius:var(--r-xl);overflow:hidden;background:${d ? "rgb(30 30 34 / .86)" : "rgb(252 252 253 / .88)"};backdrop-filter:blur(50px) saturate(1.8);box-shadow:var(--sh-3), inset 0 0 0 .5px ${d ? "rgb(255 255 255 / .1)" : "rgb(255 255 255 / .6)"}}
.pal .in{height:56px;display:flex;align-items:center;gap:var(--s-3);padding:0 var(--s-5);font-size:var(--t-17);letter-spacing:-.012em;border-bottom:.5px solid var(--hair)}
.pal .in .ic{color:var(--muted)}
.pal .in .tag{font:500 var(--t-12)/1 var(--font);color:var(--muted);height:22px;display:inline-flex;align-items:center;gap:5px;padding:0 7px;border-radius:6px;box-shadow:inset 0 0 0 .5px var(--hair-strong);margin-left:auto}
.pal .grp{padding:6px 8px 2px}
.pal h6{margin:6px 8px 4px;font:600 var(--t-11)/1 var(--font);color:var(--muted)}
.r{height:40px;display:flex;align-items:center;gap:10px;padding:0 10px;border-radius:var(--r-md)}
.r .ico{width:24px;height:24px;border-radius:6px;display:grid;place-items:center;flex:none;background:${d ? "rgb(255 255 255 / .07)" : "rgb(0 0 0 / .045)"};color:var(--muted)}
.r .ico.acc{background:linear-gradient(color-mix(in srgb, var(--accent) 85%, white), var(--accent));color:#fff}
.r .d{color:var(--muted)}
.r .k{margin-left:auto;color:var(--muted);font-size:var(--t-12)}
.r.sel{background:${d ? "rgb(255 255 255 / .09)" : "rgb(0 0 0 / .055)"}}
.pal footer{height:42px;display:flex;align-items:center;gap:var(--s-3);padding:0 10px 0 var(--s-4);border-top:.5px solid var(--hair);background:${d ? "rgb(255 255 255 / .02)" : "rgb(0 0 0 / .015)"};color:var(--muted)}
.pal footer .sp{flex:1}
.pal footer .a{display:inline-flex;align-items:center;gap:6px;font-weight:500;color:var(--text)}
.pal footer .div{width:.5px;height:16px;background:var(--hair-strong)}
/* connections */
.cx{flex:1;overflow:hidden;padding:28px 40px}
.cx .lead{color:var(--muted);margin:0 0 var(--s-5)}
.cards{display:grid;grid-template-columns:repeat(4,1fr);gap:var(--s-3);margin-bottom:var(--s-7)}
.cc{border-radius:var(--r-lg);background:${d ? "var(--panel)" : "var(--bg)"};box-shadow:var(--sh-2), var(--hi);padding:14px}
.cc .h{display:flex;align-items:center;justify-content:space-between;margin-bottom:12px}
.cc b{display:block;font-weight:600}
.cc small{display:flex;align-items:center;gap:5px;color:var(--muted);font-size:var(--t-12)}
.ok{width:6px;height:6px;border-radius:50%;background:var(--good);box-shadow:0 0 0 3px color-mix(in srgb, var(--good) 18%, transparent)}
.tog{width:30px;height:18px;border-radius:9px;background:var(--accent);position:relative;box-shadow:inset 0 0 0 .5px rgb(0 0 0 / .15)}
.tog::after{content:"";position:absolute;top:2px;right:2px;width:14px;height:14px;border-radius:50%;background:#fff;box-shadow:0 1px 2px rgb(0 0 0 / .3)}
.cat{display:flex;align-items:center;gap:var(--s-3);margin-bottom:var(--s-3)}
.cat h2{font:600 var(--t-13)/1 var(--font);margin:0}
.cat .sp{flex:1}
.srch{width:240px;height:28px;display:flex;align-items:center;gap:6px;padding:0 8px;border-radius:var(--r-sm);background:${d ? "rgb(255 255 255 / .06)" : "rgb(0 0 0 / .04)"};color:var(--faint);box-shadow:inset 0 0 0 .5px var(--hair)}
.apps{display:grid;grid-template-columns:repeat(3,1fr);column-gap:var(--s-6)}
.ap{display:flex;align-items:center;gap:var(--s-3);height:56px;border-top:.5px solid var(--hair)}
.ap span.n{flex:1;line-height:1.3}.ap small{display:block;color:var(--muted);font-size:var(--t-12)}
.ap .btn{height:24px;padding:0 10px;font-size:var(--t-12)}
/* onboarding */
.ob{flex:1;display:flex;flex-direction:column;align-items:center;position:relative;background:radial-gradient(520px 260px at 50% 120px, color-mix(in srgb, var(--accent) 22%, transparent), transparent 70%), var(--bg)}
.ob .slot{position:absolute;top:16px;left:50%;transform:translateX(-50%);display:flex;align-items:center;gap:8px;font-weight:600}
.ob .slot em{font-style:normal;color:var(--muted);font-weight:500;font-size:var(--t-12);padding:2px 8px;border-radius:6px;box-shadow:inset 0 0 0 1px var(--hair-strong);border:1px dashed var(--control)}
.steps{display:flex;gap:6px;margin-top:84px}
.steps i{width:24px;height:4px;border-radius:2px;background:var(--hair-strong)}.steps i.on{background:var(--accent)}.steps i.done{background:var(--text);opacity:.6}
.ob h1{font:650 var(--t-40)/1.05 var(--font);letter-spacing:-.03em;margin:22px 0 10px;font-variation-settings:"opsz" 32;text-align:center}
.ob p.l{color:var(--muted);font-size:var(--t-15);margin:0 0 30px;text-align:center}
.tiles{display:grid;grid-template-columns:repeat(4,150px);gap:12px}
.tl{height:112px;border-radius:var(--r-lg);background:${d ? "var(--panel)" : "var(--bg)"};box-shadow:var(--sh-2), var(--hi);display:flex;flex-direction:column;align-items:center;justify-content:center;gap:8px;position:relative;font-weight:500}
.tl small{color:var(--muted);font-size:var(--t-12)}
.tl.on{box-shadow:0 0 0 1.5px var(--accent), 0 6px 20px color-mix(in srgb, var(--accent) 25%, transparent), var(--hi)}
.tl.on small{color:var(--good)}
.tl .chk{position:absolute;top:8px;right:8px;width:18px;height:18px;border-radius:50%;display:grid;place-items:center;color:#fff;background:var(--accent)}
.tl.busy small{color:var(--accent-text)}
.spin{width:12px;height:12px;border-radius:50%;border:1.5px solid var(--hair-strong);border-top-color:var(--accent);display:inline-block;vertical-align:-2px;margin-right:4px}
.ob .cta{margin-top:30px;display:flex;flex-direction:column;align-items:center;gap:12px}
.btn.lg{height:40px;padding:0 18px;font-size:var(--t-15);border-radius:var(--r-md)}
.idx{color:var(--muted);font-size:var(--t-12);display:flex;align-items:center;gap:8px;font-feature-settings:"tnum"}
.meter{width:120px;height:4px;border-radius:2px;background:var(--raised);overflow:hidden}.meter b{display:block;height:100%;width:64%;background:var(--accent)}
/* ghost text in host apps */
.host{position:absolute;border-radius:12px;overflow:hidden;background:var(--bg);box-shadow:0 0 0 .5px ${d ? "rgb(0 0 0 / .9)" : "rgb(0 0 0 / .22)"}, 0 30px 80px rgb(0 0 0 / ${d ? .55 : .25})}
.host .tbar{height:44px;display:flex;align-items:center;justify-content:center;border-bottom:.5px solid var(--hair);background:var(--panel);font-weight:600;color:var(--muted);position:relative}
.host .tbar .lights{top:16px;left:16px}
.field{display:flex;gap:10px;align-items:center;height:38px;padding:0 20px;border-bottom:.5px solid var(--hair);color:var(--muted)}
.field b{color:var(--text);font-weight:500}
.pill{display:inline-flex;align-items:center;gap:6px;height:24px;padding:0 8px 0 3px;border-radius:12px;background:var(--raised);color:var(--text)}
.mailbody{padding:18px 20px;font:400 14px/1.6 var(--font)}
.mailbody p{margin:0 0 12px}
.gh{color:var(--ghost)}
.shim{background:linear-gradient(90deg, var(--ghost) 0%, color-mix(in srgb, var(--accent) 70%, transparent) 50%, transparent 100%);-webkit-background-clip:text;background-clip:text;color:transparent}
.acc{background:color-mix(in srgb, var(--accent) 14%, transparent);border-radius:3px;box-shadow:0 1.5px 0 color-mix(in srgb, var(--accent) 60%, transparent)}
.tabchip{display:inline-flex;align-items:center;gap:5px;height:20px;padding:0 6px 0 3px;margin-left:6px;border-radius:6px;vertical-align:1px;font:500 11px/1 var(--font);color:var(--muted);background:${d ? "rgb(44 44 48 / .92)" : "rgb(255 255 255 / .95)"};box-shadow:0 0 0 .5px var(--hair-strong), 0 2px 6px rgb(0 0 0 / ${d ? .4 : .1})}
.tabchip kbd{height:14px;min-width:0;padding:0 4px;font-size:10px}
.tabchip .m{width:12px;height:12px;border-radius:4px;display:grid;place-items:center}
.tabchip .m i{width:5px;height:5px;border-radius:50%;background:var(--accent)}
.pop{position:absolute;width:300px;border-radius:12px;padding:6px;background:${d ? "rgb(36 36 40 / .9)" : "rgb(252 252 253 / .92)"};backdrop-filter:blur(40px) saturate(1.8);box-shadow:var(--sh-3), inset 0 0 0 .5px ${d ? "rgb(255 255 255 / .1)" : "rgb(255 255 255 / .6)"};font:400 12px/1.4 var(--font)}
.pop .h{display:flex;align-items:center;gap:6px;padding:6px 8px;color:var(--muted);font-weight:600;font-size:11px}
.pop .h .sp{flex:1}
.pop .src{display:flex;gap:8px;padding:7px 8px;border-radius:8px}
.pop .src.sel{background:${d ? "rgb(255 255 255 / .07)" : "rgb(0 0 0 / .045)"}}
.pop .src b{display:block;font-weight:500;font-size:12px}.pop .src span{color:var(--muted)}
.caption{position:absolute;left:28px;bottom:16px;display:flex;align-items:center;gap:10px;font:500 12px/1 var(--font);color:${d ? "rgb(255 255 255 / .75)" : "rgb(0 0 0 / .6)"}}
.caption b{color:${d ? "#fff" : "#000"};font-weight:600}
.chat{display:grid;grid-template-columns:200px 1fr;height:calc(100% - 44px)}
.chside{background:var(--panel);border-right:.5px solid var(--hair);padding:12px 8px;color:var(--muted)}
.chside div{height:26px;display:flex;align-items:center;gap:6px;padding:0 8px;border-radius:6px}
.chside .on{background:var(--accent-soft);color:var(--text)}
.chmain{display:flex;flex-direction:column}
.msgs{flex:1;padding:16px 20px;display:flex;flex-direction:column;justify-content:flex-end;gap:14px}
.msg{display:grid;grid-template-columns:32px 1fr;gap:10px}
.msg b{font-weight:600}.msg small{color:var(--muted);margin-left:6px}
.msg p{margin:2px 0 0;font-size:14px;line-height:1.5}
.box{margin:0 20px 18px;border-radius:10px;box-shadow:inset 0 0 0 1px var(--hair-strong);padding:10px 12px;font:400 14px/1.5 var(--font)}
.box .tools{display:flex;gap:10px;color:var(--faint);margin-top:8px}
.notes{display:grid;grid-template-columns:220px 1fr;height:calc(100% - 44px)}
.nl{background:var(--panel);border-right:.5px solid var(--hair);padding:10px}
.nl div{padding:8px 10px;border-radius:8px;margin-bottom:2px}.nl div b{display:block;font-weight:600}.nl div span{color:var(--muted);font-size:12px}
.nl .on{background:var(--raised)}
.page{padding:28px 40px;font:400 15px/1.65 var(--font)}
.page h3{font:700 22px/1.2 var(--font);letter-spacing:-.018em;margin:0 0 4px}
.page .meta{color:var(--muted);font-size:12px;margin-bottom:18px}
.page ul{margin:0 0 12px;padding-left:20px}
`;
};

// ---------- shared pieces ----------
const side = (on) => `<aside class="side"><div class="top"><span>${ic("sidebar", 15)}</span><span>${ic("plus", 15)}</span></div>
<div class="find">${ic("search", 14)}<span>Search</span>${kbd("⌘", "K")}</div>
<div class="nav ${on === "home" ? "on" : ""}">${ic("home")}Home</div>
<div class="nav ${on === "needs" ? "on" : ""}">${ic("inbox")}Needs you<span class="n">5</span></div>
<div class="nav ${on === "search" ? "on" : ""}">${ic("search")}Search</div>
<div class="nav ${on === "conn" ? "on" : ""}">${ic("plug")}Connections</div>
<h6>Recent</h6>
${["Is Tidewater Billing funded?", "Prep for Driftwood review", "What did Lucia say about the MSA?", "Moorland sync options", "Q4 hiring plan, summarised"].map((t) => `<div class="th">${t}</div>`).join("")}
<div class="grow"></div>
<div class="me">${av("SO", 26)}<span><b>Sam Okafor</b><small>Pro plan</small></span>${kbd("⌘", ",")}</div></aside>`;

const win = (sideOn, main, extra = "") => `<div class="win"><div class="lights"><i></i><i></i><i></i></div>${side(sideOn)}<div class="main">${main}${extra}</div></div>`;

// ---------- screens ----------
const homeBar = `<div class="bar"><h1>Home</h1><span class="sp"></span><span class="tb">${ic("clock", 15)}Brief at 7:00</span><span class="btn">${ic("plus", 14)}New chat</span></div>`;
const homeLoaded = `${homeBar}<div class="home"><div class="col">
<div class="greet">Good morning, Sam</div><div class="date">Monday, November 2 · 3 meetings · 5 things need you</div>
<div class="comp focus"><div class="ph">Ask anything about your mail, calendar, docs and chats<span class="caret"></span></div>
<div class="row"><span class="chip hover">${ic("at", 15)}All sources <span class="stack">${["gmail", "googlecalendar", "notion", "googledrive"].map((s) => logo(s, 16)).join("")}</span></span><span class="chip">${ic("clip", 15)}</span><span class="send off">${ic("up", 16)}</span></div></div>
<div class="sugg"><span class="hover">${ic("spark", 13)}Prep me for the 11:00</span><span>${ic("spark", 13)}What did I promise this week?</span><span>${ic("spark", 13)}Summarise #driftwood</span></div>
<div class="sec"><header><h2>Today's brief</h2><small>· written 7:02</small><span class="sp"></span><a>Read aloud</a></header>
<div class="prose"><p>The one meeting that matters is the Driftwood API design review at 11:00. Ines wants a decision on cursor pagination before Friday's freeze<span class="cite">${logo("notion", 13)}RFC</span>. Margaret is still waiting on a time for the Moorland sync and proposed Thursday<span class="cite">${logo("gmail", 13)}Mail</span>.</p></div>
<div class="agenda">
<div class="ev"><span class="tm">11:00</span><span class="bar2"></span><span><b>Driftwood API design review</b><small>45 min · Meet</small></span><span class="stack">${av("IH", 22)}${av("LP", 22)}${av("RP", 22)}</span></div>
<div class="ev"><span class="tm">14:30</span><span class="bar2 n"></span><span><b>1:1 with Harbourline finance</b><small>30 min</small></span><span class="stack">${av("HF", 22)}</span></div>
</div></div></div></div>`;

const homeLoading = `${homeBar}<div class="home"><div class="col">
<div class="greet">Good morning, Sam</div><div class="date">Monday, November 2 · catching up on 4 apps…</div>
<div class="comp focus"><div class="typed">What did Lucia say about the engagement letter?<span class="caret"></span></div>
<div class="row"><span class="chip">${ic("at", 15)}All sources <span class="stack">${["gmail", "googlecalendar", "notion", "googledrive"].map((s) => logo(s, 16)).join("")}</span></span><span class="chip">${ic("clip", 15)}</span><span class="send sending"><i></i></span></div></div>
<div class="sugg"><span>${ic("spark", 13)}Prep me for the 11:00</span><span>${ic("spark", 13)}What did I promise this week?</span><span>${ic("spark", 13)}Summarise #driftwood</span></div>
<div class="sec"><header><h2>Today's brief</h2><small>· writing…</small></header>
<div class="sk" style="width:96%"></div><div class="sk" style="width:88%"></div><div class="sk" style="width:62%"></div>
<div class="agenda" style="margin-top:20px">
<div class="ev"><span class="sk" style="width:40px;margin:0"></span><span class="bar2 n"></span><span class="sk" style="width:260px;margin:0"></span><span></span></div>
<div class="ev"><span class="sk" style="width:40px;margin:0"></span><span class="bar2 n"></span><span class="sk" style="width:200px;margin:0"></span><span></span></div>
</div></div></div></div>`;

const ITEMS = [
  ["MT", "gmail", "Margaret Thistlewood", "6d", "Moorland sync: can we move to Thursday?", "Asks you a question · waiting 6 days", 1],
  ["LP", "googledocs", "Lucia Penhallow", "2d", "Engagement letter needs your signature", "You said you'd return it by Friday", 1],
  ["IH", "linear", "Ines Halloran", "1h", "DRI-214 Cursor pagination: decision needed", "Assigned to you · before your 11:00", 1],
  ["RP", "googlecalendar", "Rowan Pike", "Thu", "Q4 planning has no owner for billing", "You organised this meeting", 0],
  ["HF", "gmail", "Harbourline finance", "Fri", "Invoice HB-2207 approval", "Snoozed until today", 0],
];
const needs = `<div class="bar"><h1>Needs you</h1><span class="sp"></span><span class="tb">${ic("check", 15)}Mark all done</span></div>
<div class="split"><div class="list"><div class="seg"><span class="on">All 5</span><span>Replies 3</span><span>To do 1</span><span>Snoozed</span></div>
${ITEMS.map(([a, app, w, t, s, y, n], i) => `<div class="it ${i === 0 ? "sel" : i === 2 ? "hov" : ""}">${n ? '<span class="dot"></span>' : ""}<span class="avw">${av(a, 34)}${logo(app, 16)}</span><b>${w}</b><span class="t">${t}</span><span class="s">${s}</span><span class="y">${y}</span></div>`).join("")}
</div><div class="det"><div class="body">
<div class="from">${av("MT", 36)}<span><b>Margaret Thistlewood</b><small>margaret@cindergate.co · via Gmail</small></span><span class="t">Oct 27, 09:14</span></div>
<h1>Moorland sync: can we move to Thursday?</h1>
<p class="q">“Hi Sam, would Thursday afternoon work for the Moorland sync instead? Friday is looking tight on our side. Happy to keep it to 30 minutes.”</p>
<div class="why">${ic("spark", 14)}<span><b>Why now:</b> she's waited 6 days, and you're free Thursday 14:00–16:00.</span></div>
<div class="draft"><div class="lbl">DRAFT IN YOUR VOICE<span class="sp"></span>${kbd("⌘", "E")}&nbsp;Edit</div>
<p>Hi Margaret, Thursday works. How about 14:30 for half an hour? I'll send an invite now. Sorry for the slow reply.</p><p>Sam</p></div></div>
<div class="actbar"><span class="btn ghost">${ic("clock", 14)}Snooze ${kbd("H")}</span><span class="btn ghost">${ic("check", 14)}Done ${kbd("E")}</span><span class="sp"></span><span class="hint">Actions ${kbd("⌘", "K")}</span><span class="div"></span><span class="btn pri">Send and add invite ${kbd("⌘", "↵")}</span></div></div></div>`;

const empty = `<div class="bar"><h1>Needs you</h1></div><div class="empty"><div class="in">
<div class="ring">${ic("check", 26)}</div><h2>You're all caught up</h2><p>Nothing needs you right now. New items appear here as they arrive, with the reason they matter.</p>
<div class="next"><div>${ic("clock", 15)}Next brief<span style="color:var(--muted)">Tomorrow, 7:00</span>${kbd("⌘", "B")}</div>
<div>${ic("search", 15)}Search everything${kbd("⌘", "K")}</div><div>${ic("inbox", 15)}See snoozed (2)${kbd("⌘", "2")}</div></div></div></div>`;

const cmdk = `<div class="scrim"></div><div class="pal"><div class="in">${ic("search", 18)}<span>driftw<span class="caret"></span></span><span class="tag">${ic("at", 12)}All apps</span></div>
<div class="grp"><div class="r sel"><span class="ico acc">${ic("spark", 14)}</span><span>Ask about “driftw”</span><span class="k">Ask</span></div></div>
<div class="grp"><h6>Results</h6>
<div class="r">${logo("googlecalendar", 24)}<span>Driftwood API design review</span><span class="d">Today 11:00</span><span class="k">Event</span></div>
<div class="r">${logo("notion", 24)}<span>Driftwood · Cursor pagination RFC</span><span class="d">Edited yesterday</span><span class="k">Page</span></div>
<div class="r">${logo("linear", 24)}<span>DRI-214 Decide pagination strategy</span><span class="d">Ines Halloran</span><span class="k">Issue</span></div>
<div class="r">${logo("gmail", 24)}<span>Re: Driftwood freeze date</span><span class="d">3 messages</span><span class="k">Thread</span></div></div>
<div class="grp"><h6>Commands</h6>
<div class="r"><span class="ico">${ic("spark", 14)}</span><span>Prep me for “Driftwood API design review”</span><span class="k">${kbd("⌘", "P")}</span></div>
<div class="r"><span class="ico">${ic("plug", 14)}</span><span>Connect an app</span></div>
<div class="r"><span class="ico">${ic("gear", 14)}</span><span>Settings</span><span class="k">${kbd("⌘", ",")}</span></div></div>
<footer>${MARK(18)}<span class="sp"></span><span class="a">Ask ${kbd("↵")}</span><span class="div"></span><span class="a">Actions ${kbd("⌘", "K")}</span></footer></div>`;

const CONNECTED = [["gmail", "Gmail", "2,418 emails"], ["googlecalendar", "Google Calendar", "312 events"], ["notion", "Notion", "184 pages"], ["googledrive", "Google Drive", "1,036 files"]];
const MORE = [["linear", "Linear", "Issues and projects"], ["github", "GitHub", "Issues, PRs and code"], ["figma", "Figma", "Files and comments"], ["googledocs", "Google Docs", "Documents"],
  ["dropbox", "Dropbox", "Files"], ["zoom", "Zoom", "Meetings and transcripts"], ["hubspot", "HubSpot", "Contacts and deals"], ["asana", "Asana", "Tasks"],
  ["jira", "Jira", "Issues"], ["confluence", "Confluence", "Pages"], ["discord", "Discord", "Messages"], ["whatsapp", "WhatsApp", "Chats"]];
const conn = `<div class="bar"><h1>Connections</h1><span class="sp"></span><span class="btn">${ic("plus", 14)}Request an app</span></div><div class="cx">
<p class="lead">Connect an app once and it stays in sync. You choose what's read, and nothing is sent without you.</p>
<div class="cards">${CONNECTED.map(([s, n, c]) => `<div class="cc"><div class="h">${logo(s, 32)}<span class="tog"></span></div><b>${n}</b><small><span class="ok"></span>${c} · synced 2m ago</small></div>`).join("")}</div>
<div class="cat"><h2>Add apps</h2><span class="sp"></span><span class="srch">${ic("search", 14)}Search apps</span></div>
<div class="apps">${MORE.map(([s, n, c], i) => `<div class="ap">${logo(s, 32)}<span class="n">${n}<small>${c}</small></span><span class="btn ${i === 1 ? "hover" : ""}">Connect</span></div>`).join("")}</div></div>`;

const OB = [["gmail", "Gmail", "on"], ["googlecalendar", "Calendar", "on"], ["notion", "Notion", "busy"], ["googledrive", "Drive", ""], ["linear", "Linear", ""], ["github", "GitHub", ""], ["figma", "Figma", ""], ["whatsapp", "WhatsApp", ""]];
const onboarding = `<div class="win bare"><div class="lights"><i></i><i></i><i></i></div><div class="main"><div class="ob">
<div class="slot">${MARK(22)}<em>wordmark</em></div>
<div class="steps"><i class="done"></i><i class="on"></i><i></i></div>
<h1>Connect your apps</h1><p class="l">One click each. Two is enough for a great first answer.</p>
<div class="tiles">${OB.map(([s, n, st]) => `<div class="tl ${st}">${st === "on" ? `<span class="chk">${ic("check", 11)}</span>` : ""}${logo(s, 40)}${n}<small>${st === "on" ? "Connected" : st === "busy" ? '<span class="spin"></span>Authorising' : "Connect"}</small></div>`).join("")}</div>
<div class="cta"><span class="btn pri lg">Continue ${kbd("↵")}</span><span class="idx"><span class="meter"><b></b></span>Reading 1,284 emails and 36 events</span></div>
</div></div></div>`;

// Ghost-text frames
const ghost1 = `<div class="host" style="left:150px;top:70px;width:980px;height:600px"><div class="tbar"><div class="lights"><i></i><i></i><i></i></div>New message</div>
<div class="field">To <span class="pill">${av("MT", 18)}<b>Margaret Thistlewood</b></span></div><div class="field">Subject <b>Re: Moorland sync: can we move to Thursday?</b></div>
<div class="mailbody"><p>Hi Margaret,</p><p>Thursday works for me<span class="caret"></span><span class="gh">. How about 14:30 for half an hour? I'll send an </span><span class="shim">invite now and</span><span class="tabchip"><span class="m"><i></i></span>${kbd("Tab")}</span></p></div></div>
<div class="caption"><b>1 · Suggesting</b>Ghost text streams in at the caret, the tail shimmers while it streams, and a tiny Tab chip sits at the end. Keep typing to ignore it. In a mail composer, such as Gmail.</div>`;

const ghost2 = `<div class="host" style="left:110px;top:60px;width:1060px;height:620px"><div class="tbar"><div class="lights"><i></i><i></i><i></i></div>#driftwood</div>
<div class="chat"><div class="chside"><div>${ic("hash", 14)}general</div><div class="on">${ic("hash", 14)}driftwood</div><div>${ic("hash", 14)}billing</div><div>${ic("hash", 14)}random</div></div>
<div class="chmain"><div class="msgs">
<div class="msg">${av("IH", 32)}<div><b>Ines Halloran</b><small>10:12</small><p>Can you confirm cursor vs offset pagination before 11? I'd like to lock it in the RFC.</p></div></div>
<div class="msg">${av("RP", 32)}<div><b>Rowan Pike</b><small>10:14</small><p>+1, the mobile client needs to know by Friday.</p></div></div></div>
<div class="box"><span class="acc">Cursor, as in the RFC: opaque</span><span class="caret"></span><span class="gh"> tokens, 50 per page, and we keep offset for the admin export only.</span><span class="tabchip"><span class="m"><i></i></span>${kbd("⌥", "→")} word · ${kbd("Tab")} all</span>
<div class="tools">${ic("bold", 15)}${ic("at", 15)}${ic("clip", 15)}<span style="margin-left:auto">${ic("send", 15)}</span></div></div></div></div></div>
<div class="caption"><b>2 · Partial accept</b>⌥→ takes the next word and Tab takes the rest. Accepted words flash with the accent underline and the rest stays ghosted. In a chat app, such as Slack.</div>`;

const ghost3 = `<div class="host" style="left:150px;top:70px;width:980px;height:600px"><div class="tbar"><div class="lights"><i></i><i></i><i></i></div>Notes</div>
<div class="notes"><div class="nl"><div class="on"><b>Driftwood review prep</b><span>Today · 10:31</span></div><div><b>Q4 budget questions</b><span>Yesterday</span></div><div><b>Moorland sync</b><span>Oct 27</span></div></div>
<div class="page"><h3>Driftwood review prep</h3><div class="meta">Today at 10:31</div>
<ul><li>Decision: cursor pagination (Ines needs it before Friday's freeze)</li><li>Open question: offset for the admin export?</li></ul>
<p>Budget: Tidewater Billing is funded in Q4, owned by Ines; the vendor contract is with Vellum &amp; Thistle<span class="caret"></span><span class="tabchip"><span class="m"><i></i></span>2 sources</span></p></div></div></div>
<div class="pop" style="left:448px;top:340px"><div class="h">SOURCES<span class="sp"></span>${kbd("⌘", "Z")} undo</div>
<div class="src sel">${logo("googledrive", 22)}<div><b>Q4 budget.xlsx</b><span>“Tidewater Billing: funded, owner I. Halloran”</span></div></div>
<div class="src">${logo("gmail", 22)}<div><b>Re: Tidewater vendor contract</b><span>Lucia Penhallow · Oct 21</span></div></div></div>
<div class="caption"><b>3 · Accepted</b>The text is now yours. The chip turns into "2 sources", and clicking it or pressing ⌥S opens a mini popover. Undo with ⌘Z. In a notes app.</div>`;

const SCREENS = [
  ["home-dark", "dark", win("home", homeLoaded)],
  ["home-light", "light", win("home", homeLoaded)],
  ["home-light-sending", "light", win("home", homeLoading)],
  ["needs-you-dark", "dark", win("needs", needs)],
  ["needs-you-empty-light", "light", win("needs", empty).replace(`<span class="n">5</span>`, "")],
  ["cmdk-dark", "dark", win("home", homeLoaded, cmdk)],
  ["connections-light", "light", win("conn", conn)],
  ["onboarding-dark", "dark", onboarding],
  ["ghost-1-suggesting-light", "light", ghost1],
  ["ghost-2-partial-dark", "dark", ghost2],
  ["ghost-3-accepted-light", "light", ghost3],
];
fs.mkdirSync(OUT, { recursive: true });
for (const f of fs.readdirSync(OUT)) if (/\.(html|png)$/.test(f)) fs.rmSync(path.join(OUT, f));
for (const [name, theme, body] of SCREENS)
  fs.writeFileSync(path.join(OUT, name + ".html"), `<!doctype html><html lang="en"><head><meta charset="utf-8"><title>${name}</title><style>${CSS(theme)}</style></head><body>${body}</body></html>`);
console.log("ok");
