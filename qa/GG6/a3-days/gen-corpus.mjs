// Synthetic first-week corpus: notes (.md), mail (.eml), calendar (.ics). Deterministic.
// Every file carries a unique token `qtok<N>` so searches can be checked exactly.
// usage: node gen-corpus.mjs <dir> <count> [startIndex]
import fs from "node:fs";
import path from "node:path";

const PEOPLE = ["Ada Wexford", "Bram Hollins", "Cora Lindqvist", "Dev Okafor", "Esme Tarrant", "Finn Mabry", "Gita Rao", "Hugo Pellam", "Ines Varga", "Jonah Quill"];
const PROJECTS = ["Harbor Ledger", "Quartz Relay", "Ember Atlas", "Northwind Audit", "Saltmarsh Portal", "Tidepool Sync", "Copperfield Migration", "Juniper Forecast"];
const TOPICS = ["budget review", "vendor contract", "hiring plan", "launch checklist", "security audit", "quarterly goals", "travel booking", "design critique", "customer escalation", "data retention policy"];
const WORDS = "the a project team meeting notes follow up deadline review draft schedule client budget forecast risk mitigation owner action item decision summary next steps agreed blocked waiting approval invoice renewal contract scope milestone".split(" ");

let seed = 12345;
const rnd = () => ((seed = (seed * 1103515245 + 12345) & 0x7fffffff) / 0x7fffffff);
const pick = (a) => a[Math.floor(rnd() * a.length)];
const para = (n) => Array.from({ length: n }, () => pick(WORDS)).join(" ");
const slug = (s) => s.toLowerCase().replace(/[^a-z0-9]+/g, "-");
const email = (p) => `${slug(p).replace("-", ".")}@example.test`;

export function generate(dir, count, start = 0) {
  for (const d of ["notes", "mail", "calendar"]) fs.mkdirSync(path.join(dir, d), { recursive: true });
  const files = [];
  for (let i = start; i < start + count; i++) {
    seed = 1000 + i;
    const proj = pick(PROJECTS), topic = pick(TOPICS), a = pick(PEOPLE), b = pick(PEOPLE);
    const k = i % 10;
    let f, body;
    if (k < 6) {
      f = path.join(dir, "notes", `${slug(proj)}`, `note-${i}-${slug(topic)}.md`);
      body = `# ${proj}: ${topic} (${i})\n\nOwner: ${a}. Reviewer: ${b}. Token qtok${i}.\n\n${para(80)}\n\n## Action items\n- ${a} to ${pick(WORDS)} the ${topic} by Friday\n- ${b} to ${para(6)}\n\n${para(120)}\n`;
    } else if (k < 9) {
      const day = 1 + (i % 28);
      f = path.join(dir, "mail", `msg-${i}.eml`);
      body = `From: "${a}" <${email(a)}>\r\nTo: "Sam Rivers" <sam.rivers@example.test>\r\nSubject: ${proj} ${topic} #${i}\r\nDate: Mon, ${String(day).padStart(2, "0")} Sep 2026 09:${String(i % 60).padStart(2, "0")}:00 +0000\r\nMessage-ID: <msg-${i}@example.test>\r\nMIME-Version: 1.0\r\nContent-Type: text/plain; charset=utf-8\r\n\r\nHi Sam,\r\n\r\nAbout ${proj} and the ${topic}: ${para(60)} qtok${i}.\r\n\r\nCan you confirm by Thursday?\r\n\r\n${a}\r\n`;
    } else {
      const day = 1 + (i % 28);
      const dt = `202611${String(day).padStart(2, "0")}T${String(9 + (i % 8)).padStart(2, "0")}0000Z`;
      f = path.join(dir, "calendar", `event-${i}.ics`);
      body = `BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//QA GG6//A3//EN\r\nBEGIN:VEVENT\r\nUID:event-${i}@example.test\r\nDTSTAMP:20261001T000000Z\r\nDTSTART:${dt}\r\nDTEND:${dt.replace(/T(\d\d)/, (m, h) => "T" + String(+h + 1).padStart(2, "0"))}\r\nSUMMARY:${proj} ${topic} sync ${i}\r\nLOCATION:Room ${i % 12}\r\nDESCRIPTION:Discuss ${topic} for ${proj}. qtok${i}\r\nORGANIZER;CN="${a}":mailto:${email(a)}\r\nATTENDEE;CN="${b}";PARTSTAT=ACCEPTED:mailto:${email(b)}\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n`;
    }
    fs.mkdirSync(path.dirname(f), { recursive: true });
    fs.writeFileSync(f, body);
    files.push(f);
  }
  return files;
}

if (process.argv[1]?.endsWith("gen-corpus.mjs")) {
  const [dir, count, start] = process.argv.slice(2);
  const files = generate(dir, Number(count || 1000), Number(start || 0));
  console.log(`wrote ${files.length} files to ${dir}`);
}
