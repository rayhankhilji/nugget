#!/usr/bin/env python3
"""A1 extra scenarios on top of gen.py's persona (gen.py itself is untouched).

    python3 gen_extra_a1.py OUT     # default OUT: persona/world_a1

X1  Late-evening dated promise ("by Friday" written Thu 21:40 EDT = Fri 01:40 UTC).
X2  Late-evening "tomorrow" promise (Mon Nov 2 20:30 EST = Tue 01:30 UTC).
X3  FYI forward of Dana's ask by Marcus (question only inside the forwarded block).
X4  Question addressed to Theo with Maya only on Cc.
X5  A second copy of Harriet's ask (same Message-ID) saved to Mail/Archive after Maya replied.
X6  Priya asks, then nudges in the same thread ("Any update?"): one card, not two.
"""
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import gen  # noqa: E402
from gen import ME, PRIYA, THEO, DANA, MARCUS, HARRIET, ny, mail, truth, add  # noqa: E402


def extra():
    # X1
    mail("x1-priya-ask", ny("2026-10-29T15:00"), PRIYA, [ME], "Icon audit",
         "Hi Maya, could you send me the icon audit spreadsheet? Priya", "X1-late-promise")
    mail("x1-maya-promise", ny("2026-10-29T21:40"), ME, [PRIYA], "Re: Icon audit",
         "Sure, I'll send the icon audit by Friday.\nMaya", "X1-late-promise", reply_to_id="x1-priya-ask")
    mail("x1-maya-deliver", ny("2026-11-02T10:00"), ME, [PRIYA], "Re: Icon audit",
         "Here's the icon audit, sorry for the delay.\nMaya", "X1-late-promise",
         reply_to_id="x1-maya-promise", refs=["x1-priya-ask"])
    truth("X1", "Promise written Thu 21:40 EDT 'by Friday' is due Fri Oct 30 (not Fri Nov 6)",
          expect=[{"kind": "follow_up", "match": "icon audit", "first": "2026-10-30", "last": "2026-11-02"}],
          forbid=[{"match": "icon audit", "from": "2026-11-03", "why": "delivered Mon 10:00"}])
    # X2
    mail("x2-maya-promise", ny("2026-11-02T20:30"), ME, [DANA], "Sprint notes",
         "Hi Dana, thanks for today. I'll send the sprint notes tomorrow.\nMaya", "X2-late-tomorrow")
    truth("X2", "Promise 'tomorrow' written Mon Nov 2 20:30 EST is due Tue Nov 3",
          expect=[{"kind": "follow_up", "match": "sprint notes", "first": "2026-11-03", "last": "2026-11-05"}])
    # X3
    mail("x3-marcus-fwd", ny("2026-10-27T09:00"), MARCUS, [ME], "Fwd: Revised checkout mockups",
         "FYI, see Dana's note below.\n\n---------- Forwarded message ---------\n"
         "From: Dana Whitfield <dana@northwind-outfitters.example>\nDate: Mon, Oct 26, 2026\n"
         "Subject: Revised checkout mockups\n\nHi Maya,\n\nCould you send the revised checkout mockups by Thursday (Oct 29)?\n\nDana",
         "X3-forward")
    truth("X3", "FYI forward: no second 'reply' card",
          forbid=[{"match": "fwd: revised checkout", "why": "FYI forward of a thread already carded/answered"}])
    # X4
    mail("x4-priya-theo", ny("2026-10-28T10:00"), PRIYA, [THEO], "Illustration invoice",
         "Theo, can you approve the illustration contractor invoice by Friday?\nPriya", "X4-cc-only", cc=[ME])
    truth("X4", "Question addressed to Theo, Maya on Cc only",
          forbid=[{"match": "illustration invoice", "kind": "unanswered_thread", "why": "asked of Theo, Maya is Cc"}])
    # X5: same Message-ID as s12-harriet-ask, saved into Archive after Maya's reply
    src = next(i for i in gen.ITEMS if i["id"] == "s12-harriet-ask")
    add("Mail/Archive/20261029-0915-s12-harriet-ask.eml", src["text"], ny("2026-11-03T09:00"),
        "mail", "X5-late-copy", iid="x5-harriet-copy")
    truth("X5", "Late duplicate copy (same Message-ID) must not resurrect the answered agenda card",
          forbid=[{"match": "agenda", "kind": "unanswered_thread", "from": "2026-11-04", "why": "answered Nov 2; copy is a dup"}])
    # X6
    mail("x6-priya-ask", ny("2026-10-27T14:00"), PRIYA, [ME], "Onboarding copy",
         "Could you review the onboarding copy draft? Priya", "X6-nudge")
    mail("x6-priya-nudge", ny("2026-10-29T09:30"), PRIYA, [ME], "Re: Onboarding copy",
         "Any update on the onboarding copy?", "X6-nudge", reply_to_id="x6-priya-ask")
    truth("X6", "Ask then nudge in the same thread: one card",
          expect=[{"kind": "unanswered_thread", "match": "onboarding copy", "first": "2026-10-29", "last": "2026-11-05"}])


def main():
    out = Path(sys.argv[1] if len(sys.argv) > 1 else Path(__file__).parent / "world_a1")
    gen.build()
    extra()
    gen.days()
    import json
    for it in gen.ITEMS:
        p = out / "files" / it["path"]
        p.parent.mkdir(parents=True, exist_ok=True)
        p.write_text(it["text"], newline="")
    tl = [{k: v for k, v in it.items() if k != "text"} for it in sorted(gen.ITEMS, key=lambda x: x["at"])]
    (out / "timeline.json").write_text(json.dumps(tl, indent=1))
    (out / "truth.json").write_text(json.dumps(gen.TRUTH, indent=1))
    (out / "days.json").write_text(json.dumps(gen.DAYS, indent=1))
    print(f"{len(gen.ITEMS)} items, {len(gen.TRUTH)} scenarios -> {out}")


if __name__ == "__main__":
    main()
