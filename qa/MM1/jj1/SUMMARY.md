# MM1 / JJ1 overlay re-verify (JJ1-2, JJ1-3)

Integration head 0b1feab5. The real debug `nugget-desktop` (`pnpm tauri build --debug --no-bundle`)
under Xvfb + openbox + a private session bus with AT-SPI (apt: xvfb openbox xdotool xautomation
x11-utils dbus-x11 at-spi2-core python3-gi gir1.2-gtk-3.0 gir1.2-atspi-2.0 libwebkit2gtk-4.1-dev
xclip …), the GTK fixture as target, the mock provider. Raw: `raw/`.

```
NUGGET_E2E_DISPLAY=:117 bash qa/JJ1/overlay/run.sh --out O --only s9
QA_SCENARIOS=overlay_mm1.mjs NUGGET_E2E_DISPLAY=:117 bash qa/JJ1/overlay/run.sh --out O --only s9,s9d --repeat 3
NUGGET_E2E_DISPLAY=:117 bash qa/JJ1/overlay/run.sh --out O        # full suite
```
`overlay_mm1.mjs` = original + `overlay_mm1.patch` (9a-mm1 clipboard check with xclip, 9d-mm1, a
`QA_SCENARIOS` hook in run.sh).

| Finding | Original | Now (0b1feab5) | Verdict |
|---|---|---|---|
| JJ1-2 (s9 9c) | `esc_in_target_hides_overlay:false, third_trigger_fired:1, third_trigger_views:[], overlay_still_visible:true`, stuck 4+ min | PASS 5/5: refocus → next trigger starts a fresh request; Esc in the target hides the overlay; third trigger `reading→streaming→done`; all later scenarios worked | fixed |
| JJ1-3 (s9 9a) | `composer:"Hello ", search_entry:"Thanks, see you at noon in the lobby."`, `inserted` | insert refused 5/5: `doubletap.error.field_changed` ("The cursor moved to another field, so nothing was inserted. The draft is on the clipboard."); composer "Hello ", search entry empty, clipboard holds the draft (9a-mm1 PASS 3/3) | fixed |

Notes: the original 9a check now reports FAIL only because it predates the "refuse and copy" fix the
JJ1 report accepted. Full suite 11/16: 3a/3b/3c fail because a refinement now sends 2 requests (JJ1-8's
one stricter retry) and the mock's fixed replies never satisfy it (mock artefact, but it changes the
"one request per burst" behaviour JJ1 listed as working); 10 is JJ1-4 (still open, EE2 paused,
`overlay_done_text:""`); 9a as above. No new JJ1 findings.
