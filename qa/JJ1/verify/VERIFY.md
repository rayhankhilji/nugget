# JJ1 verification layer (round 1)

A fresh agent that had not written the scenarios reproduced each candidate in `CANDIDATES.md` from
the written repro alone. It used copies of `qa/JJ1` under /tmp/jj1v, fresh homes, the prebuilt
`nuggetd` at efebc0c, and its own minimal checks where cheap (one-file fixtures, axe on the markup,
recomputation over the CI artifacts). Code under test is unchanged at 44b9e11.

| Id | Verdict | Observed |
|---|---|---|
| C-M1 | reproduced | own one-file check `14/03/2026 à 09:06`: `items: 0`, health `{"code":"ok","reason":"Up to date."}` |
| C-M2 | reproduced | `**Priya** · 2026-03-05 21:00`; `2026-11-06 09:30` (future); re-export `occurrences: 2` |
| C-M3 | reproduced | local 09:12 shown `07:12`; 00:30 on 1 June shown `2026-05-31 22:30` in "May 2026" (exports without `date_unixtime` fall back to local `date`) |
| C-M4 | reproduced | phone-only sender: hit `people []`, no entity; also iMessage handle-only and Telegram contact phones |
| C-M5 | reproduced | `state: error`, `invalid utf-8 sequence of 1 bytes from index 3`, 0 hits |
| C-M6 | reproduced | `> <Medien ausgeschlossen>`, `> <Без медиафайлов>` kept as text; iOS RU placeholder as system line |
| C-M7 | reproduced | Telegram imports a symlinked export outside the folder (`find_results` follows symlinks) |
| C-M8 | reproduced | both `…lands at 18:40` and edited `…19:10 now` present while both exports exist |
| C-S1 | reproduced | own .ics: `event 2026-04-15T20:00:00Z extracted` for `TZID=America/New_York:…T200000` |
| C-S2 | reproduced | 23:30 CET mail on 15 Jan: found at offset 60, missed at 120. The daemon applies the offset it is given; the fault is one offset per request (`api.ts` sends today's) |
| C-S5 | reproduced | `2026-02-30` passes `isDate`; daemon `-32602 filters.to: expected a date as YYYY-MM-DD` |
| C-S6 | reproduced | axe 4.14: `label-content-name-mismatch` serious (`Preview.tsx` aria-label `Open {title}`, text "Open in app") |
| C-T1 | reproduced | `sensitive caught 8/22`: en 6/8, de 0/6, es 1/4, fr 1/4; 0/27 false positives |
| C-T2 | reproduced | 9/22 stay sensitive after 4B triage; model flags 4/5 credentials, 0 health, money or legal; `triage.rs` `pii \|\| t.sensitive` lets a model `false` clear the default |
| C-T3 | reproduced | hit keys `[chunk_id,date,date_source,item,kind,people,score,sensitive,snippet,title,url]`; no `junk` in search_ui, retrieve or desktop src |
| C-D1 | reproduced | history keeps `iban-de-spaced`, `ssn-dashed`, `api-key`, `aws-key`; the PII scan flags all four |
| C-D2 | partly | 7/48 identical is right, but 3 of those echo a base draft that is itself a refusal; "Shorter" was shorter by words in 6/12 (5/12 also requires the right language); German chat drafted in English 2/2 |
| C-P1 | reproduced | `OK Search { query: "pay\u{61c}me" }`, and likewise U+00AD, U+2060, U+2028 |
