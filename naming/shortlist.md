# Naming shortlist: replacing "Nugget"

Status: proposal for the founder to choose from. Nothing in the code has been renamed.
Checked on 2026-11-02. **No domain or trademark here is confirmed available.** Every result below is a hint from public lookups, not a clearance.

## How it was checked

- **DNS:** I asked Cloudflare DNS-over-HTTPS for the NS records of `<name>.com`, `.ai`, `.app` and `.so`. `dig` and `whois` aren't installed in this environment. A name that resolves is registered. NXDOMAIN only means nothing is delegated right now; the name could still be registered, reserved or premium. I checked a few `.com` names with Verisign RDAP, which is a stand-in for whois.
- **Web:** I ran one web search per name for software or AI products with the same name.
- **Mac App Store:** I searched the iTunes Search API (`entity=macSoftware`, GB store) for apps whose name starts with the candidate.
- **GitHub orgs:** I couldn't check these, because the sandbox proxy blocks `github.com/<name>` and `api.github.com/users/*`.
- **Trademarks:** I didn't search any register. See "Trademark next steps" at the end.

## 1. Forty candidates

I gave each name a quick sense check in EN, ES, FR, DE, PT, AR, HI, ZH and JA. None has an obvious bad meaning, but a native speaker should confirm the finalists. Notes are in brackets.

**Real words used freshly:** Oriel (a bay window), Margin, Keel, Cue, Vellum, Gloss, Lantern, Tally, Kite, Brief

**Short coined words:** Ploma (Catalan for "quill"), Velin, Quillo, Tessel, Orbly, Nuvo, Kairo (the German spelling of Cairo), Veyo, Inklet, Mirra (myrrh)

**Verbs about writing, remembering or knowing:** Heed, Jot, Glean, Recall, Reckon, Gather, Parse, Tend, Scribe, Mull

**Two-syllable brandables:** Tallis, Penna (Latin and Italian for "quill"), Ostro, Marlo, Corvo (Portuguese and Italian for "raven"; some cultures read the raven as a bad omen), Selva (Spanish and Portuguese for "jungle"), Tavi, Sayso, Wrenly, Sorrel

**Dropped early:**
- Noma: it is also the name of a disease.
- Glean: Glean, the enterprise AI search company, is a direct conflict.
- Recall: Microsoft Recall.
- Tide: the detergent brand.
- Lumen and Lumo: Lumen the telecom company, and Proton's Lumo AI.
- Vellum: Vellum is an AI dev platform that has raised $25M.
- Penna: there is a Penna Mac menu-bar AI writing tool.
- Velin: Velin AI is a desktop "undetectable" interview-cheating app, which is a bad association.
- Ostro: Ostro is an AI company that Veeva acquired.

## 2. Shortlist of 8, ranked

| # | Name | Same-name products found on the web | Mac App Store (exact) | DNS: .com / .ai / .app / .so | Risk |
|---|---|---|---|---|---|
| 1 | **Oriel** | A B2B company licensing AI and ML intellectual property (Romania/Cyprus). Also Oriel College, Oxford, and Oriel optics instruments, so check class 9. | none | registered (parked at atom.com, a sales marketplace) / reg. (Afternic) / reg. / **NXDOMAIN** | Low–medium |
| 2 | **Tallis** | No AI or productivity product found. Near names: Talla, Tali (medical AI). Also the composer Thomas Tallis. | none | reg. since 1996 / reg. / reg. / **NXDOMAIN** | Low–medium |
| 3 | **Heed** | heed.io: an internal-comms platform with an AI assistant. A Heed menu-bar reminder app for Mac. Heed voice notes and tasks on iOS. | none exact on Mac | reg. (Sedo parking) / reg. (Afternic) / reg. / reg. | Medium |
| 4 | **Margin** | Product names on the Mac App Store | "Margin AI", "Margin: The Writing App", plus several markdown apps | all reg. (.ai at atom.com) | High |
| 5 | **Ploma** | A "ploma" social shopping app on iOS | "Ploma: Plain Text Editor" | reg. / reg. / reg. / **NXDOMAIN** | Medium–high |
| 6 | **Sayso** | Sayso voice OS, sayso.ai (speech editing), a Sayso voice-command app on Windows, SaySo tasks on iOS | none | reg. / reg. / SERVFAIL / reg. | High |
| 7 | **Keel** | Keel Labs: a local-first desktop AI assistant with memory and briefs, almost the same product. Also Keel, a no-code ERP backed by Earlybird. | "Keel: Budget & Expense Tracker" | all reg. (.ai SERVFAIL) | Very high |
| 8 | **Cue** | Cue: a Mac writing companion with suggestions as you type and memory, almost the same product. Many others. | "Cue: Personal AI Assistant" and others | all reg. | Very high |

With a prefix, `getheed.ai`, `getheed.so`, `heedapp.ai`, `heedapp.app` and `useheed.so` returned NXDOMAIN. That is only a hint.

### Why the top 3

1. **Oriel.** It is a real word that hardly anyone uses: a window that juts out of a wall so you can see the whole street. That matches the product, which shows one view across mail, calendar, docs and chats and points out what matters. It is easy to say in every target language and has no bad meanings I could find. It sits alongside Linear and Arc: a quiet real word, used freshly. No productivity app uses it. The `.com` is parked on a sales marketplace, so it may be buyable at a premium price.
2. **Tallis.** It sounds editorial and premium, like a person rather than a gadget. It echoes "tally" (keeping count of everything) and Thomas Tallis (quiet, exact craft). It is in the same family as Granola and Raycast: warm, and it reads as a brand. I found no software conflicts. The `.com` is old and held by someone, and its status suggests it is actively managed.
3. **Heed.** It fits the job best: "tells you what needs your attention" in one syllable, and it works as a verb ("Heed it"). It is short and confident, in the spirit of Superhuman. The catch is crowding: heed.io sells AI-assisted comms software, a real class 42 neighbour, and the bare `.com` is parked. Pick it only after a trademark search, and plan on a domain like `heed.app` (held by someone now) or `getheed`.

## 3. Brand ideas for the top 3

| | Personality | Palette (hex) | Logo idea | Wordmark font (open licence) |
|---|---|---|---|---|
| **Oriel** | Calm and clear-sighted, like a window seat with a view of everything | Ink `#111318` · Paper `#F5F3EE` · Pane blue `#4C6FFF` · Daylight `#F2B544` | An arched window that doubles as a text caret, with light falling through it | Instrument Sans (OFL), set tight in lowercase |
| **Tallis** | Composed and quietly expert, like an editor who knows your files | Night `#0F1115` · Bone `#F4F1EA` · Tallis green `#2F6B57` · Brass `#C69C5D` | A lowercase "t" whose crossbar runs on as faint ghost text: the Tab-to-accept moment | Fraunces (OFL), soft optical size |
| **Heed** | Alert and decisive, like a good chief of staff | Graphite `#16171B` · Chalk `#F7F7F5` · Signal coral `#FF5A3C` · Mist `#9AA3AE` | One dot above a line, an attention mark that pulses gently in the menu bar | Geist (OFL), medium weight |

## Trademark next steps (not done)

- Search the UKIPO (trademarks.ipo.gov.uk), USPTO (tmsearch.uspto.gov) and EUIPO (eSearch plus, or TMview for national EU marks).
- Search Nice classes **9** (downloadable software), **42** (SaaS and AI) and **35** (office functions). Optionally add **38** (messaging).
- Include phonetic and spelling variants, such as Oriel/Oriol/Orielle, Tallis/Talis/Tallys and Heed/Hede/Heedly.
- Look for live marks owned by software companies, and for any wider "assistant" or "AI" claims.
- **Known candidates to look at:** Oriel optics instruments (class 9 risk for Oriel), heed.io (class 42 risk for Heed).
- Get a trademark attorney to clear the chosen name before buying a domain or launching publicly.
