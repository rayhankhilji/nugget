"""Area B: Double Tap feedback becomes voice material. An accepted draft whose final text the user
edited to include a card number and an API key is stored (source nugget-voice); does a later cloud
draft send it? Also: the locality draft.prewarm / draft.read_ahead predict vs where the draft
actually goes when only a cloud provider is usable."""
import os
import sys
import time
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import b_common as B  # noqa: E402
import ipc  # noqa: E402
from s21_cloud_draft import ctx, CONFIG, ENV  # noqa: E402,F401

NAME = "s22_cloud_draft_feedback"


def run(lab):
    _, corpus = B.add_corpus(lab)
    c = lab.client()
    pw = c.call("draft.prewarm", ctx(), timeout=30)
    ra = c.call("draft.read_ahead", {"context": ctx()}, timeout=30)
    st, text, done = c.stream("draft.begin", {"context": ctx()}, timeout=120)
    lab.check("prediction.matches_draft",
              (pw or {}).get("locality") == st.get("locality") and ra.get("skipped") == "cloud",
              f"prewarm={pw} read_ahead={ra} draft.begin provider={st.get('provider')} locality={st.get('locality')}",
              severity="Low")
    final = ("Confirmed, renewal on 14 November. Pay with card 4111 1111 1111 1111, "
             "key sk-ant-api03-GG8fakeKEYzzzz. GG8CANARY-FEEDBACKSENS-1")
    c.call("draft.feedback", {"stream_id": st["stream_id"], "outcome": "edited",
                              "final_text": final, "insert_verified": True})
    time.sleep(8)
    t = time.time()
    st2, _, _ = c.stream("draft.begin", {"context": ctx()}, timeout=120)
    reqs = B.cloud_reqs(lab, t)
    b = B.body(reqs)
    cites = [(x["item"]["key"], x.get("sensitive")) for x in st2.get("citations", [])]
    lab.check("feedback_secret.not_sent_to_cloud",
              "GG8CANARY-FEEDBACKSENS-1" not in b and "4111 1111 1111 1111" not in b,
              f"citations={cites} excluded_sensitive={st2.get('excluded_sensitive')} "
              f"in_body={'GG8CANARY-FEEDBACKSENS-1' in b}", severity="High")
    B.save_requests(lab, "draft-after-feedback", reqs)
    B.ledger_covers(lab, "after_feedback", t, "draft", corpus)
    c.close()
