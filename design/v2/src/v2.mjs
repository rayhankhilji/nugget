// Nugget v2 mockups: real HTML/CSS with the component structure the app would use.
// node v2.mjs <outdir>  -> <outdir>/*.html (then shoot.mjs)
import fs from "node:fs";
import path from "node:path";
import { V2 } from "./v2pal.mjs";

const HERE = path.dirname(new URL(import.meta.url).pathname);
const OUT = process.argv[2];
const F = path.join(HERE, "fonts/node_modules");
const ff = (fam, file, w = "100 900") => `@font-face{font-family:"${fam}";src:url("file://${F}/${file}") format("woff2");font-weight:${w};font-display:block}`;
const FONTS = [
  ff("Bricolage Grotesque", "@fontsource-variable/bricolage-grotesque/files/bricolage-grotesque-latin-wght-normal.woff2"),
  ff("Hanken Grotesk", "@fontsource-variable/hanken-grotesk/files/hanken-grotesk-latin-wght-normal.woff2"),
  ff("Source Serif 4", "@fontsource-variable/source-serif-4/files/source-serif-4-latin-wght-normal.woff2"),
  ff("IBM Plex Sans Arabic", "@fontsource/ibm-plex-sans-arabic/files/ibm-plex-sans-arabic-arabic-400-normal.woff2", 400),
  ff("IBM Plex Sans Arabic", "@fontsource/ibm-plex-sans-arabic/files/ibm-plex-sans-arabic-arabic-500-normal.woff2", 500),
].join("");

// The mark: a gold nugget whose counter is a lowercase "n".
const MARK = (s, fill = "var(--gold)") =>
  `<svg class="mark" width="${s}" height="${s}" viewBox="0 0 64 64" aria-hidden="true"><path fill="${fill}" fill-rule="evenodd" d="M12 24C14 13 23 7 34 7c12 0 20 8 21 19 1 7-1 12 0 18 1 8-4 13-12 13H20c-8 0-13-5-12-13 1-7 2-13 4-20ZM24 57V37c0-5 3.5-8.5 8-8.5s8 3.5 8 8.5v20Z"/></svg>`;

const I = {
  home: '<path d="M4 11.5 12 5l8 6.5V19a1 1 0 0 1-1 1h-4.5v-5h-5v5H5a1 1 0 0 1-1-1Z"/>',
  inbox: '<path d="M4 13.5 6.5 5.5h11l2.5 8V19a1 1 0 0 1-1 1H5a1 1 0 0 1-1-1Z"/><path d="M4 13.5h4.5l1 2h5l1-2H20"/>',
  search: '<circle cx="11" cy="11" r="6.5"/><path d="m16 16 4 4"/>',
  plug: '<path d="M9 4v4M15 4v4M6.5 8h11v3a5.5 5.5 0 0 1-11 0Z"/><path d="M12 16.5V20"/>',
  arrowUp: '<path d="M12 19V6M6.5 11.5 12 6l5.5 5.5"/>',
  at: '<circle cx="12" cy="12" r="3.5"/><path d="M15.5 9v4.5a2.5 2.5 0 0 0 5 0V12a8.5 8.5 0 1 0-3.5 6.9"/>',
  clip: '<path d="m19 11.5-6.8 6.8a4.5 4.5 0 0 1-6.4-6.4l7.1-7.1a3 3 0 0 1 4.2 4.2l-7 7a1.5 1.5 0 0 1-2.1-2.1l6.4-6.4"/>',
  check: '<path d="m5 12.5 4.5 4.5L19 7.5"/>',
  cal: '<rect x="4" y="5.5" width="16" height="14.5" rx="2"/><path d="M4 10h16M8.5 3.5v4M15.5 3.5v4"/>',
  doc: '<path d="M7 3.5h7l4 4V20H6.5V4Z"/><path d="M14 3.5V8h4M9.5 12.5h5M9.5 16h5"/>',
  bolt: '<path d="M13 3.5 5.5 13.5H12l-1 7 7.5-10H12Z"/>',
  gear: '<circle cx="12" cy="12" r="3"/><path d="M12 3.5v2.5M12 18v2.5M3.5 12H6M18 12h2.5M6 6l1.8 1.8M16.2 16.2 18 18M6 18l1.8-1.8M16.2 7.8 18 6"/>',
  sparkle: '<path d="M12 4c.6 4.2 3.8 7.4 8 8-4.2.6-7.4 3.8-8 8-.6-4.2-3.8-7.4-8-8 4.2-.6 7.4-3.8 8-8Z"/>',
  clock: '<circle cx="12" cy="12" r="8"/><path d="M12 8v4.5l3 1.8"/>',
  reply: '<path d="M10 7 5 12l5 5"/><path d="M5 12h9a5 5 0 0 1 5 5v1"/>',
  plus: '<path d="M12 5v14M5 12h14"/>',
  lock: '<rect x="5.5" y="10.5" width="13" height="9.5" rx="2"/><path d="M8.5 10.5V8a3.5 3.5 0 0 1 7 0v2.5"/>',
};
const ic = (n, s = 18) => `<svg class="ic" width="${s}" height="${s}" viewBox="0 0 24 24" aria-hidden="true">${I[n]}</svg>`;
const kbd = (...k) => k.map((x) => `<kbd>${x}</kbd>`).join("");

const vars = (p) => Object.entries(p).map(([k, v]) => `--${k.replace(/[A-Z]/g, (c) => "-" + c.toLowerCase())}:${v};`).join("");

const CSS = (theme, rtl) => `${FONTS}
:root{color-scheme:${theme};${vars(V2[theme])}
--display:"Bricolage Grotesque","IBM Plex Sans Arabic","Noto Sans Devanagari","Noto Sans CJK JP",system-ui,sans-serif;
--ui:"Hanken Grotesk","IBM Plex Sans Arabic","Noto Sans Devanagari","Noto Sans CJK JP",system-ui,sans-serif;
--read:"Source Serif 4","Noto Naskh Arabic","Noto Serif Devanagari","Noto Serif CJK JP",Georgia,serif;
--s1:4px;--s2:8px;--s3:12px;--s4:16px;--s5:24px;--s6:32px;--s7:48px;--s8:64px;
--r1:6px;--r2:10px;--r3:16px;--r4:22px;
--fs-xs:12px;--fs-sm:13px;--fs-md:14px;--fs-lg:16px;--fs-read:17px;--fs-xl:22px;--fs-2xl:32px;--fs-3xl:44px;
--shadow-pop:${theme === "dark" ? "0 0 0 1px var(--line), 0 24px 64px rgb(0 0 0 / .55)" : "0 0 0 1px var(--line), 0 24px 64px rgb(20 20 25 / .14)"};
--shadow-composer:${theme === "dark" ? "0 0 0 1px var(--line), 0 12px 40px rgb(0 0 0 / .35)" : "0 0 0 1px var(--line), 0 10px 32px rgb(20 20 25 / .07)"};
--gold-edge:${theme === "dark" ? "transparent" : "rgb(0 0 0 / .14)"};
}
*{box-sizing:border-box}html,body{margin:0;height:100%}
body{width:1280px;height:800px;overflow:hidden;font:var(--fs-md)/1.5 var(--ui);background:var(--bg);color:var(--text);-webkit-font-smoothing:antialiased;display:grid;grid-template-columns:248px 1fr}
body.bare{display:block}
${rtl ? "body{font-family:var(--ui)} .read,.read p{font-family:var(--ui)!important;line-height:1.85}" : ""}
.ic{fill:none;stroke:currentColor;stroke-width:1.7;stroke-linecap:round;stroke-linejoin:round;flex:none}
kbd{font:500 11px/1 var(--ui);color:var(--muted);min-width:20px;height:20px;padding:0 5px;display:inline-grid;place-items:center;border-radius:5px;background:var(--raised);box-shadow:inset 0 -1px 0 var(--line)}
.wordmark{font:700 21px/1 var(--display);letter-spacing:-0.035em}
/* rail */
.rail{background:var(--panel);display:flex;flex-direction:column;padding:var(--s4) var(--s3);gap:2px;border-inline-end:1px solid var(--line)}
.rail .brand{display:flex;align-items:center;gap:var(--s2);padding:var(--s1) var(--s2) var(--s4)}
.cmd{display:flex;align-items:center;gap:var(--s2);height:36px;padding:0 var(--s2) 0 var(--s3);border-radius:var(--r2);background:var(--bg);box-shadow:0 0 0 1px var(--line);color:var(--muted);margin-bottom:var(--s3)}
.cmd span{flex:1}
.nav{display:flex;align-items:center;gap:var(--s3);height:34px;padding:0 var(--s3);border-radius:var(--r1);color:var(--muted);font-weight:500}
.nav.on{color:var(--text);background:var(--raised)}
.nav.on .ic{color:var(--gold-text)}
.nav .count{margin-inline-start:auto;font:600 11px/1 var(--ui);color:var(--on-gold);background:var(--gold);border-radius:999px;padding:3px 7px}
.rail h6{margin:var(--s5) var(--s3) var(--s2);font:600 var(--fs-xs) var(--ui);color:var(--muted)}
.thread{padding:6px var(--s3);border-radius:var(--r1);color:var(--muted);font-size:var(--fs-sm);white-space:nowrap;overflow:hidden;text-overflow:ellipsis}
.rail .grow{flex:1}
.dt{display:flex;gap:var(--s3);align-items:flex-start;padding:var(--s3);border-radius:var(--r2);color:var(--muted);font-size:var(--fs-xs);line-height:1.45}
.dt b{color:var(--text);font-weight:600;display:block;font-size:var(--fs-sm)}
.dt .ic{color:var(--gold-text);margin-top:2px}
.acct{display:flex;align-items:center;gap:var(--s3);padding:var(--s2) var(--s3);border-radius:var(--r2)}
.av{width:28px;height:28px;border-radius:50%;display:grid;place-items:center;background:var(--gold-soft);color:var(--gold-text);font:700 12px var(--ui)}
.acct .who{flex:1;line-height:1.25}.acct .who small{display:block;color:var(--muted);font-size:var(--fs-xs)}
/* main */
main{position:relative;overflow:hidden}
.page{max-width:720px;margin:0 auto;padding:var(--s8) var(--s5) 0}
.date{color:var(--muted);font-size:var(--fs-sm);margin-bottom:var(--s2)}
h1.hello{font:600 var(--fs-3xl)/1.05 var(--display);letter-spacing:-0.035em;margin:0 0 var(--s5)}
${rtl ? "h1.hello{letter-spacing:0;font-weight:500;line-height:1.3}" : ""}
.composer{background:var(--${theme === "dark" ? "panel" : "bg"});border-radius:var(--r4);box-shadow:var(--shadow-composer);padding:var(--s4) var(--s4) var(--s3) var(--s5)}
.composer .ph{font-size:var(--fs-lg);color:var(--muted);min-height:52px}
.composer .bar{display:flex;align-items:center;gap:var(--s2)}
.tool{display:inline-flex;align-items:center;gap:6px;height:30px;padding:0 var(--s2);border-radius:var(--r1);color:var(--muted);font-size:var(--fs-sm)}
.apps{display:inline-flex;margin-inline-start:var(--s1)}
.apps i{width:18px;height:18px;border-radius:5px;background:var(--raised);box-shadow:0 0 0 2px var(--${theme === "dark" ? "panel" : "bg"});display:grid;place-items:center;font:700 9px var(--ui);font-style:normal;color:var(--text);margin-inline-start:-4px}
.send{margin-inline-start:auto;width:36px;height:36px;border-radius:50%;display:grid;place-items:center;background:var(--gold);color:var(--on-gold);box-shadow:inset 0 0 0 1px var(--gold-edge)}
.sugg{display:flex;flex-wrap:wrap;gap:var(--s1) var(--s5);margin:var(--s4) var(--s2) 0;color:var(--muted);font-size:var(--fs-sm)}
.sugg span{display:inline-flex;align-items:center;gap:6px}.sugg .ic{color:var(--gold-text)}
.brief{margin-top:var(--s7)}
.brief header{display:flex;align-items:baseline;gap:var(--s3);margin-bottom:var(--s3)}
.brief h2{font:600 var(--fs-xl)/1.2 var(--display);letter-spacing:-0.02em;margin:0}
.brief header small{color:var(--muted);font-size:var(--fs-sm)}
.brief header a{margin-inline-start:auto;color:var(--gold-text);font-weight:600;font-size:var(--fs-sm);text-decoration:none}
.read p{font:400 var(--fs-read)/1.7 var(--read);margin:0 0 var(--s3);color:var(--text)}
.read em{font-style:normal;background:linear-gradient(transparent 62%, var(--gold-soft) 62%)}
sup.c{font:600 10px/1 var(--ui);color:var(--gold-text);background:var(--gold-soft);border-radius:4px;padding:2px 4px;margin-inline-start:2px;vertical-align:4px}
/* cmdk */
.scrim{position:absolute;inset:0;background:var(--scrim);backdrop-filter:blur(2px)}
.palette{position:absolute;inset-inline-start:50%;top:96px;transform:translateX(-50%);width:640px;background:var(--panel);border-radius:var(--r3);box-shadow:var(--shadow-pop);overflow:hidden}
.palette .in{display:flex;align-items:center;gap:var(--s3);padding:0 var(--s5);height:60px;border-bottom:1px solid var(--line);font-size:var(--fs-lg)}
.palette .in .ic{color:var(--muted)}
.caret{display:inline-block;width:2px;height:20px;background:var(--gold);vertical-align:-4px;margin-inline-start:1px}
.palette .grp{padding:var(--s2) var(--s2) 0}
.palette .grp h6{margin:var(--s2) var(--s3) var(--s1);font:600 var(--fs-xs) var(--ui);color:var(--muted)}
.row{display:flex;align-items:center;gap:var(--s3);height:42px;padding:0 var(--s3);border-radius:var(--r2);color:var(--text)}
.row .ic{color:var(--muted)}
.row .d{color:var(--muted);font-size:var(--fs-sm)}
.row .r{margin-inline-start:auto;display:flex;gap:4px;align-items:center;color:var(--muted);font-size:var(--fs-xs)}
.row.sel{background:var(--gold-soft)}
.row.sel .ic{color:var(--gold-text)}
.palette footer{display:flex;gap:var(--s5);padding:var(--s3) var(--s5);margin-top:var(--s2);border-top:1px solid var(--line);color:var(--muted);font-size:var(--fs-xs)}
.palette footer span{display:inline-flex;gap:6px;align-items:center}
/* needs you */
.split{display:grid;grid-template-columns:400px 1fr;height:100%}
.list{border-inline-end:1px solid var(--line);display:flex;flex-direction:column}
.list header{padding:var(--s6) var(--s5) 0}
.list h1,.detail h1{font:600 var(--fs-2xl)/1.1 var(--display);letter-spacing:-0.03em;margin:0}
.tabs{display:flex;gap:var(--s5);margin-top:var(--s4);border-bottom:1px solid var(--line);padding:0 var(--s5);margin-inline:calc(-1 * var(--s5))}
.tabs span{padding:var(--s2) 0 var(--s3);color:var(--muted);font-weight:500;font-size:var(--fs-sm)}
.tabs span.on{color:var(--text);box-shadow:inset 0 -2px 0 var(--gold)}
.item{display:grid;grid-template-columns:32px 1fr auto;gap:2px var(--s3);padding:var(--s4) var(--s5);border-bottom:1px solid var(--line)}
.item.sel{background:var(--panel);box-shadow:inset 3px 0 0 var(--gold)}
${rtl ? ".item.sel{box-shadow:inset -3px 0 0 var(--gold)}" : ""}
.tile{width:32px;height:32px;border-radius:var(--r1);background:var(--raised);display:grid;place-items:center;font:700 13px var(--ui);grid-row:span 3}
.item .who{font-weight:600}.item .t{color:var(--muted);font-size:var(--fs-xs);text-align:end}
.item .s{grid-column:2/4;font-size:var(--fs-sm)}
.item .y{grid-column:2/4;color:var(--muted);font-size:var(--fs-xs)}
.item .dot{display:inline-block;width:6px;height:6px;border-radius:50%;background:var(--gold);margin-inline-end:6px;vertical-align:1px}
.detail{padding:var(--s6) var(--s7);overflow:hidden}
.kick{display:flex;gap:var(--s2);align-items:center;color:var(--muted);font-size:var(--fs-sm);margin-bottom:var(--s3)}
.detail h1{font-size:28px;margin-bottom:var(--s4);max-width:24ch}
.quote{font:400 var(--fs-lg)/1.65 var(--read);color:var(--muted);border-inline-start:2px solid var(--line);padding-inline-start:var(--s4);margin:0 0 var(--s4)}
.why{font-size:var(--fs-sm);color:var(--muted);margin-bottom:var(--s5)}.why b{color:var(--text);font-weight:600}
.draft{background:var(--panel);border-radius:var(--r3);padding:var(--s5)}
.draft .lbl{display:flex;align-items:center;gap:6px;font:600 var(--fs-xs) var(--ui);color:var(--gold-text);margin-bottom:var(--s2)}
.draft p{font:400 var(--fs-read)/1.65 var(--read);margin:0 0 var(--s2)}
.acts{display:flex;align-items:center;gap:var(--s2);margin-top:var(--s5)}
.btn{display:inline-flex;align-items:center;gap:var(--s2);height:36px;padding:0 var(--s4);border-radius:var(--r2);font:600 var(--fs-md) var(--ui);color:var(--text);background:transparent}
.btn.gold{background:var(--gold);color:var(--on-gold);box-shadow:inset 0 0 0 1px var(--gold-edge)}
.btn.gold kbd{background:rgb(0 0 0 / .1);color:var(--on-gold);box-shadow:none}
.btn.line{box-shadow:inset 0 0 0 1px var(--control)}
.btn.lg{height:48px;padding:0 var(--s5);font-size:var(--fs-lg);border-radius:var(--r3)}
.acts .sp{flex:1}
.acts .ghost{color:var(--muted);font-weight:500}
/* connections */
.conn{max-width:920px;margin:0 auto;padding:var(--s7) var(--s6) 0}
.conn h1{font:600 var(--fs-2xl)/1.1 var(--display);letter-spacing:-0.03em;margin:0}
.conn .sub{color:var(--muted);margin:var(--s2) 0 var(--s5)}
.connected{display:grid;grid-template-columns:repeat(4,1fr);gap:var(--s3);margin-bottom:var(--s6)}
.ctile{display:flex;align-items:center;gap:var(--s3);padding:var(--s3);border-radius:var(--r2);background:var(--panel)}
.ctile .tile{grid-row:auto;width:36px;height:36px;background:var(--bg);box-shadow:0 0 0 1px var(--line)}
.ctile b{display:block;font-weight:600;line-height:1.2}.ctile small{color:var(--muted);font-size:var(--fs-xs);display:flex;align-items:center;gap:5px}
.ok{width:6px;height:6px;border-radius:50%;background:var(--good);display:inline-block}
.sect{display:flex;align-items:center;gap:var(--s4);margin-bottom:var(--s3)}
.sect h2{font:600 var(--fs-lg) var(--ui);margin:0}.sect h2 small{color:var(--muted);font-weight:500;margin-inline-start:6px}
.find{margin-inline-start:auto;display:flex;align-items:center;gap:var(--s2);width:260px;height:34px;padding:0 var(--s3);border-radius:var(--r2);box-shadow:inset 0 0 0 1px var(--control);color:var(--muted);font-size:var(--fs-sm)}
.grid{display:grid;grid-template-columns:repeat(3,1fr);column-gap:var(--s5);gap:0;border-top:1px solid var(--line)}
.app{display:flex;align-items:center;gap:var(--s3);padding:var(--s4) var(--s3);border-bottom:1px solid var(--line)}
.app .tile{grid-row:auto;width:36px;height:36px}
.app .n{flex:1;line-height:1.25}.app .n small{display:block;color:var(--muted);font-size:var(--fs-xs)}
.app .go{font-weight:600;font-size:var(--fs-sm);color:var(--gold-text)}
.foot-note{display:flex;gap:var(--s2);align-items:center;color:var(--muted);font-size:var(--fs-xs);margin-top:var(--s5)}
/* onboarding */
.ob{height:800px;position:relative;display:flex;flex-direction:column;align-items:center;padding:var(--s6) var(--s6) 0;background:radial-gradient(600px 300px at 50% 150px, var(--gold-soft), transparent 70%), var(--bg)}
.ob .top{width:100%;display:flex;align-items:center;gap:var(--s2)}
.steps{position:absolute;inset-inline-start:50%;transform:translateX(-50%);top:var(--s6);display:flex;gap:var(--s3);align-items:center;color:var(--muted);font-size:var(--fs-sm)}
.steps .st{display:flex;align-items:center;gap:6px}.steps .st.done{color:var(--text)}.steps .st.done .ic{color:var(--good)}
.steps .st.on{color:var(--text);font-weight:600}.steps .st.on i{background:var(--gold)}
.steps i{width:8px;height:8px;border-radius:50%;background:var(--control);display:inline-block}
.steps .bar{width:32px;height:1px;background:var(--line)}
.ob h1{font:600 56px/1 var(--display);letter-spacing:-0.045em;margin:96px 0 var(--s3);text-align:center}
.ob .lead{color:var(--muted);font-size:var(--fs-lg);margin:0 0 var(--s6);text-align:center}
.big{display:grid;grid-template-columns:repeat(4,168px);gap:var(--s3)}
.bt{height:120px;border-radius:var(--r3);background:var(--panel);box-shadow:0 0 0 1px var(--line);display:flex;flex-direction:column;align-items:center;justify-content:center;gap:var(--s2);position:relative;font-weight:600}
.bt .tile{grid-row:auto;width:44px;height:44px;border-radius:12px;font-size:17px}
.bt small{color:var(--muted);font-weight:500;font-size:var(--fs-xs)}
.bt.on{box-shadow:0 0 0 1.5px var(--gold)}
.bt.on .chk{position:absolute;top:10px;inset-inline-end:10px;width:20px;height:20px;border-radius:50%;background:var(--gold);color:var(--on-gold);display:grid;place-items:center}
.bt.on small{color:var(--good)}
.ob .cta{display:flex;flex-direction:column;align-items:center;gap:var(--s3);margin-top:var(--s6)}
.idx{display:flex;align-items:center;gap:var(--s2);color:var(--muted);font-size:var(--fs-sm)}
.idx .meter{width:120px;height:4px;border-radius:2px;background:var(--raised);overflow:hidden}
.idx .meter b{display:block;height:100%;width:62%;background:var(--gold)}
.label{position:fixed;bottom:8px;inset-inline-end:12px;font:500 10px var(--ui);color:var(--muted);opacity:.6}
`;

const S = {
  en: {
    search: "Search or ask…", home: "Home", needs: "Needs you", srch: "Search", conn: "Connections", recent: "Recent",
    threads: ["Is Tidewater Billing funded?", "Prep for Driftwood review", "What did Lucia say about the MSA?", "Moorland sync options"],
    dtT: "Double Tap", dtB: "Tap ⌥ twice in any app to draft in your voice.", name: "Sam Okafor", plan: "Pro · Settings ⌘,",
    date: "Monday, 2 November", hello: "Morning, Sam.", ph: "Ask anything about your mail, calendar, docs and chats…",
    tools: ["All sources", "Attach"], sugg: ["Prep me for the 11:00", "What did I promise this week?", "Summarise #driftwood since Friday"],
    brief: "Today's brief", briefSmall: "Written 7:02 from 4 apps", needsLink: "3 need you →",
    p1: `Three meetings today. The one that matters is the <em>Driftwood API design review</em> at 11:00: Ines wants a decision on cursor pagination before the freeze<sup class="c">1</sup>.`,
    p2: `Margaret Thistlewood is still waiting on a time for the Moorland sync; she proposed Thursday<sup class="c">2</sup>. The Vellum &amp; Thistle engagement letter is due Friday<sup class="c">3</sup>.`,
  },
  ar: {
    search: "ابحث أو اسأل…", home: "الرئيسية", needs: "يحتاجك", srch: "بحث", conn: "الاتصالات", recent: "الأخيرة",
    threads: ["هل ميزانية Tidewater مموّلة؟", "التحضير لمراجعة Driftwood", "ماذا قالت لوسيا عن العقد؟", "خيارات اجتماع Moorland"],
    dtT: "النقر المزدوج", dtB: "انقر ⌥ مرتين في أي تطبيق لكتابة مسودة بأسلوبك.", name: "سام أوكافور", plan: "Pro · الإعدادات ⌘,",
    date: "الاثنين، 2 نوفمبر", hello: "صباح الخير يا سام.", ph: "اسأل عن أي شيء في بريدك وتقويمك ومستنداتك ومحادثاتك…",
    tools: ["كل المصادر", "إرفاق"], sugg: ["حضّرني لاجتماع 11:00", "بماذا وعدت هذا الأسبوع؟", "لخّص ‎#driftwood منذ الجمعة"],
    brief: "موجز اليوم", briefSmall: "كُتب الساعة 7:02 من 4 تطبيقات", needsLink: "3 تحتاجك ←",
    p1: `ثلاثة اجتماعات اليوم. أهمها <em>مراجعة تصميم واجهة Driftwood</em> الساعة 11:00: تريد إينيس قرارًا بشأن ترقيم الصفحات قبل التجميد<sup class="c">1</sup>.`,
    p2: `ما زالت مارغريت تنتظر موعدًا لاجتماع Moorland؛ اقترحت يوم الخميس<sup class="c">2</sup>. وخطاب التعاقد مع Vellum &amp; Thistle مستحق يوم الجمعة<sup class="c">3</sup>.`,
  },
};

const rail = (on, s = S.en) => `<aside class="rail">
<div class="brand">${MARK(26)}<span class="wordmark">nugget</span></div>
<div class="cmd">${ic("search", 16)}<span>${s.search}</span>${kbd("⌘", "K")}</div>
<div class="nav ${on === "home" ? "on" : ""}">${ic("home")}${s.home}</div>
<div class="nav ${on === "needs" ? "on" : ""}">${ic("inbox")}${s.needs}<span class="count">3</span></div>
<div class="nav ${on === "search" ? "on" : ""}">${ic("search")}${s.srch}</div>
<div class="nav ${on === "conn" ? "on" : ""}">${ic("plug")}${s.conn}</div>
<h6>${s.recent}</h6>${s.threads.map((t) => `<div class="thread">${t}</div>`).join("")}
<div class="grow"></div>
<div class="dt">${ic("bolt", 16)}<div><b>${s.dtT}</b>${s.dtB}</div></div>
<div class="acct"><span class="av">SO</span><span class="who">${s.name}<small>${s.plan}</small></span></div>
</aside>`;

const APPS = ["Gmail", "Calendar", "Slack", "Notion"];
const home = (s = S.en) => `<main><div class="page">
<div class="date">${s.date}</div>
<h1 class="hello">${s.hello}</h1>
<div class="composer"><div class="ph">${s.ph}</div>
<div class="bar"><span class="tool">${ic("at", 16)}${s.tools[0]}<span class="apps">${APPS.map((a) => `<i>${a[0]}</i>`).join("")}</span></span><span class="tool">${ic("clip", 16)}${s.tools[1]}</span><span class="send">${ic("arrowUp", 18)}</span></div></div>
<div class="sugg">${s.sugg.map((x) => `<span>${ic("sparkle", 14)}${x}</span>`).join("")}</div>
<section class="brief"><header><h2>${s.brief}</h2><small>${s.briefSmall}</small><a>${s.needsLink}</a></header>
<div class="read"><p>${s.p1}</p><p>${s.p2}</p></div></section>
</div></main>`;

const cmdk = () => `${home()}<div class="scrim"></div>
<div class="palette" role="dialog"><div class="in">${ic("search", 20)}<span>driftw<span class="caret"></span></span><span style="margin-inline-start:auto">${kbd("esc")}</span></div>
<div class="grp"><div class="row sel">${ic("sparkle")}<span>Ask nugget “driftw…”</span><span class="r">${kbd("↵")}</span></div></div>
<div class="grp"><h6>Results</h6>
<div class="row">${ic("cal")}<span>Driftwood API design review</span><span class="d">Calendar · today 11:00</span></div>
<div class="row">${ic("doc")}<span>Driftwood pagination RFC</span><span class="d">Notion · edited yesterday</span></div>
<div class="row">${ic("reply")}<span>Re: Driftwood freeze date</span><span class="d">Gmail · Ines Halloran</span></div></div>
<div class="grp"><h6>Actions</h6>
<div class="row">${ic("bolt")}<span>Prep me for “Driftwood API design review”</span><span class="r">${kbd("⌘", "P")}</span></div>
<div class="row">${ic("plug")}<span>Connect an app…</span></div>
<div class="row">${ic("gear")}<span>Settings</span><span class="r">${kbd("⌘", ",")}</span></div></div>
<footer><span>${kbd("↑", "↓")} move</span><span>${kbd("↵")} open</span><span>${kbd("⌘", "↵")} ask about selection</span><span style="margin-inline-start:auto">${kbd("Tab")} filter by app</span></footer></div>`;

const ITEMS = [
  ["G", "Margaret Thistlewood", "6d", "Moorland sync: can we move to Thursday?", "Asks you something · waiting 6 days", true],
  ["N", "Lucia Penhallow", "2d", "Engagement letter needs your signature", "You said you'd return it by Friday", true],
  ["S", "#driftwood · Ines", "1h", "“Can you confirm pagination before 11?”", "Mentions you · before your 11:00", true],
  ["C", "Q4 planning", "Thu", "Agenda has no owner for the billing item", "You organised this meeting", false],
  ["G", "Harbourline finance", "Fri", "Invoice HB-2207 approval", "Snoozed until today", false],
];
const needs = () => `<main><div class="split"><section class="list"><header><h1>Needs you</h1>
<div class="tabs"><span class="on">All 5</span><span>Replies 3</span><span>Commitments 1</span><span>Snoozed 1</span></div></header>
${ITEMS.map(([m, w, t, s, y, n], i) => `<div class="item ${i === 0 ? "sel" : ""}"><span class="tile">${m}</span><span class="who">${n ? '<span class="dot"></span>' : ""}${w}</span><span class="t">${t}</span><span class="s">${s}</span><span class="y">${y}</span></div>`).join("")}
</section>
<article class="detail"><div class="kick">${ic("reply", 15)}Reply · Gmail · 6 days ago</div>
<h1>Moorland sync: can we move to Thursday?</h1>
<p class="quote">“Hi Sam, would Thursday afternoon work for the Moorland sync instead? Friday is looking tight on our side. Happy to keep it to 30 minutes.”</p>
<div class="why"><b>Why it's here:</b> Margaret asked you a question 6 days ago, and you're free Thursday 14:00–16:00.</div>
<div class="draft"><div class="lbl">${ic("sparkle", 14)}Draft in your voice</div>
<p>Hi Margaret, Thursday works. How about 14:30 for half an hour? I'll send an invite now. Sorry for the slow reply.</p>
<p>Sam</p></div>
<div class="acts"><span class="btn gold">Send ${kbd("⌘", "↵")}</span><span class="btn line">Edit</span><span class="sp"></span><span class="btn ghost">Snooze ${kbd("H")}</span><span class="btn ghost">Done ${kbd("E")}</span></div>
</article></div></main>`;

const ALL = [["Gmail", "Mail", 1], ["Google Calendar", "Calendar", 1], ["Slack", "Chat", 1], ["Notion", "Docs", 1], ["Google Drive", "Files", 0], ["Outlook", "Mail", 0],
  ["Microsoft Teams", "Chat", 0], ["GitHub", "Code", 0], ["Linear", "Issues", 0], ["Dropbox", "Files", 0], ["Zoom", "Meetings", 0], ["HubSpot", "CRM", 0],
  ["Asana", "Tasks", 0], ["Confluence", "Docs", 0], ["Jira", "Issues", 0], ["WhatsApp", "Chat", 0]];
const conns = () => `<main><div class="conn"><h1>Connections</h1>
<p class="sub">nugget reads only what you connect, and never sends anything without you. Disconnect any time.</p>
<div class="sect"><h2>Connected<small>4</small></h2></div>
<div class="connected">${ALL.filter((a) => a[2]).map(([n]) => `<div class="ctile"><span class="tile">${n[0]}</span><span><b>${n}</b><small><span class="ok"></span>Synced 2 min ago</small></span></div>`).join("")}</div>
<div class="sect"><h2>Add an app</h2><span class="find">${ic("search", 15)}Find an app…</span></div>
<div class="grid">${ALL.filter((a) => !a[2]).map(([n, c]) => `<div class="app"><span class="tile">${n[0]}</span><span class="n">${n}<small>${c}</small></span><span class="go">Connect</span></div>`).join("")}</div>
<div class="foot-note">${ic("lock", 14)}Tokens are stored encrypted in your account. Double Tap and quick ask run on this computer.</div>
</div></main>`;

const OBAPPS = [["Gmail", "Connected", 1], ["Google Calendar", "Connected", 1], ["Slack", "One click", 0], ["Notion", "One click", 0],
  ["Outlook", "One click", 0], ["Google Drive", "One click", 0], ["Microsoft Teams", "One click", 0], ["WhatsApp", "One click", 0]];
const onboarding = () => `<div class="ob"><div class="top">${MARK(28)}<span class="wordmark">nugget</span></div>
<div class="steps"><span class="st done">${ic("check", 14)}Sign in</span><span class="bar"></span><span class="st on"><i></i>Connect apps</span><span class="bar"></span><span class="st"><i></i>First answer</span></div>
<h1>Bring your work together.</h1>
<p class="lead">One click each. Connect two or more and ask your first question in under a minute.</p>
<div class="big">${OBAPPS.map(([n, s, on]) => `<div class="bt ${on ? "on" : ""}">${on ? `<span class="chk">${ic("check", 12)}</span>` : ""}<span class="tile">${n[0]}</span>${n}<small>${s}</small></div>`).join("")}</div>
<div class="cta"><span class="btn gold lg">Ask your first question ${ic("arrowUp", 18).replace('class="ic"', 'class="ic" style="transform:rotate(90deg)"')}</span>
<span class="idx"><span class="meter"><b></b></span>Reading 1,284 emails and 36 events…</span></div></div>`;

const SCREENS = [
  ["home-dark", "dark", false, rail("home") + home()],
  ["home-light", "light", false, rail("home") + home()],
  ["home-rtl-dark", "dark", true, rail("home", S.ar) + home(S.ar)],
  ["cmdk-dark", "dark", false, rail("home") + cmdk()],
  ["needs-you-light", "light", false, rail("needs") + needs()],
  ["needs-you-dark", "dark", false, rail("needs") + needs()],
  ["connections-light", "light", false, rail("conn") + conns()],
  ["onboarding-dark", "dark", false, onboarding()],
];
fs.mkdirSync(OUT, { recursive: true });
for (const f of fs.readdirSync(OUT)) if (/\.(html|png)$/.test(f)) fs.rmSync(path.join(OUT, f));
for (const [name, theme, rtl, body] of SCREENS) {
  const bare = name.startsWith("onboarding") ? ' class="bare"' : "";
  fs.writeFileSync(path.join(OUT, name + ".html"),
    `<!doctype html><html lang="${rtl ? "ar" : "en"}" dir="${rtl ? "rtl" : "ltr"}"><head><meta charset="utf-8"><title>nugget · ${name}</title><style>${CSS(theme, rtl)}</style></head><body${bare}>${body}</body></html>`);
}
// Brand sheet: mark, wordmark, app icon, palette.
const sw = (t) => Object.entries(V2[t]).filter(([k]) => !["scrim"].includes(k)).map(([k, v]) => `<div style="display:flex;align-items:center;gap:8px;font:500 12px var(--ui)"><i style="width:22px;height:22px;border-radius:6px;background:${v};box-shadow:0 0 0 1px var(--line)"></i>${k}<span style="color:var(--muted)">${v}</span></div>`).join("");
fs.writeFileSync(path.join(OUT, "brand.html"), `<!doctype html><html><head><meta charset="utf-8"><style>${CSS("dark", false)}
body{display:block;height:auto;width:1280px} .sheet{display:grid;grid-template-columns:1fr 1fr}.half{padding:48px;display:flex;flex-direction:column;gap:28px}
.lock{display:flex;align-items:center;gap:16px}.lock .wordmark{font-size:56px}.icon{width:120px;height:120px;border-radius:28px;background:#16140F;display:grid;place-items:center;box-shadow:0 0 0 1px var(--line)}
.sws{display:grid;grid-template-columns:1fr 1fr;gap:8px 16px}.type{display:flex;flex-direction:column;gap:6px}</style></head><body><div class="sheet">
<div class="half" style="${vars(V2.dark)}background:var(--bg);color:var(--text)"><div class="lock">${MARK(64)}<span class="wordmark">nugget</span></div>
<div style="display:flex;gap:20px;align-items:center"><div class="icon">${MARK(84, "#FFC53D")}</div>${MARK(32)}${MARK(20)}${MARK(16)}</div>
<div class="sws">${sw("dark")}</div></div>
<div class="half" style="${vars(V2.light)}background:var(--bg);color:var(--text)"><div class="lock">${MARK(64)}<span class="wordmark">nugget</span></div>
<div class="type"><span style="font:600 40px/1 var(--display);letter-spacing:-.035em">Bricolage Grotesque</span><span style="font:500 18px var(--ui)">Hanken Grotesk: interface, labels, lists</span><span style="font:400 19px var(--read)">Source Serif 4: answers and briefs read like a page.</span><span style="font:500 18px var(--ui)" dir="rtl">IBM Plex Sans Arabic: اسأل عن أي شيء</span></div>
<div class="sws">${sw("light")}</div></div></div></body></html>`);
console.log("ok");
