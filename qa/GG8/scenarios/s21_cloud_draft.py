"""Area B: Double Tap with a cloud provider. draft.read_ahead and draft.prewarm must send nothing;
draft.begin sends field/thread/subject/recipients and non-sensitive passages only; a secure field
and an excluded app are refused with nothing sent; a screenshot is not sent unless
allow_cloud_screenshots; ledger rows `draft` with accurate items."""
import os
import sys
import time
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import b_common as B  # noqa: E402
import ipc  # noqa: E402

NAME = "s21_cloud_draft"
CONFIG = """
[[providers]]
id = "anthropic"
kind = "anthropic"
model = "claude-mock"
api_key = "env:ANTHROPIC_API_KEY"
tiers = ["light", "synthesis", "agentic"]
"""
ENV = {"ANTHROPIC_API_KEY": "sk-ant-GG8CANARY-APIKEY-anthropic"}
SHOT = open("/tmp/gg8-B-shot.b64").read() if os.path.exists("/tmp/gg8-B-shot.b64") else "iVBORw0KGgo="


def ctx(secure=False, app_id="org.gnome.Evolution", app_name="Evolution", shot=False, value=""):
    return {"app": {"id": app_id, "name": app_name, "window_title": "Re: Acme renewal"},
            "field": {"role": "text_area", "label": "Message body", "value": value, "secure": secure},
            "thread": [{"author": "Priya Raman <priya@acme-logistics.example>",
                        "text": "Hi, can you confirm the Acme renewal date and the payment method we agreed? GG8CANARY-THREAD-1",
                        "from_me": False}],
            "recipients": ["priya@acme-logistics.example"], "subject": "Re: Acme renewal",
            "screenshot_png_base64": SHOT if shot else None}


def nothing_sent(lab, label, since):
    time.sleep(2)
    reqs = B.cloud_reqs(lab, since)
    return lab.check(f"{label}.nothing_sent", not reqs, [(r["path"], r.get("body", "")[:150]) for r in reqs],
                     severity="High")


def run(lab):
    _, corpus = B.add_corpus(lab)
    c = lab.client()
    # read-ahead with cloud: must skip, nothing sent
    t = time.time()
    try:
        r = c.call("draft.read_ahead", {"context": ctx(), "instruction": None}, timeout=30)
    except ipc.RpcError as e:
        r = f"error {e}"
    lab.check("read_ahead.skipped_cloud", isinstance(r, dict) and r.get("skipped") == "cloud", r)
    nothing_sent(lab, "read_ahead", t)
    # prewarm with cloud
    t = time.time()
    try:
        r = c.call("draft.prewarm", ctx(), timeout=30)
    except ipc.RpcError as e:
        r = f"error {e}"
    lab.note(f"prewarm -> {r}")
    time.sleep(3)
    reqs = B.cloud_reqs(lab, t)
    lab.check("prewarm.nothing_sent", not reqs, [(r["path"], r.get("body", "")[:300]) for r in reqs])
    # secure field
    t = time.time()
    try:
        r = c.call("draft.begin", {"context": ctx(secure=True, value="hunter2-GG8CANARY-PWD-1")}, timeout=30)
        lab.check("secure.refused", False, r)
    except ipc.RpcError as e:
        lab.check("secure.refused", True, str(e))
    nothing_sent(lab, "secure", t)
    # excluded app
    t = time.time()
    try:
        r = c.call("draft.begin", {"context": ctx(app_id="com.1password.1password", app_name="1Password")}, timeout=30)
        lab.check("excluded_app.refused", False, r)
    except ipc.RpcError as e:
        lab.check("excluded_app.refused", True, str(e))
    nothing_sent(lab, "excluded_app", t)
    # a real draft, with a screenshot attached (not allowed for cloud by default)
    t = time.time()
    st, text, done = c.stream("draft.begin", {"context": ctx(shot=True), "instruction": "confirm politely"}, timeout=120)
    lab.check("draft.answered", done and not done.get("error"), f"{st} {done}")
    lab.check("draft.excluded_sensitive", st.get("excluded_sensitive", 0) > 0, st.get("excluded_sensitive"))
    reqs = B.cloud_reqs(lab, t)
    B.no_secrets(lab, "draft", reqs)
    b = B.body(reqs)
    lab.check("draft.no_screenshot", SHOT[:40] not in b and '"image"' not in b, "image in body" if '"image"' in b else "")
    lab.check("draft.thread_sent_as_documented", "GG8CANARY-THREAD-1" in b, "")
    lab.check("draft.no_local_paths", "file://" not in b and lab.home not in b, "", severity="Low")
    B.ledger_covers(lab, "draft", t, "draft", corpus)
    B.save_requests(lab, "draft", reqs)
    try:
        c.call("draft.feedback", {"stream_id": st["stream_id"], "outcome": "accepted",
                                  "final_text": text + " GG8CANARY-FINAL-1", "insert_verified": True})
    except ipc.RpcError as e:
        lab.note(f"feedback: {e}")
    # thread containing a card number typed on screen by the other party
    t = time.time()
    cx = ctx()
    cx["thread"].append({"author": "Priya", "text": "Use card 4111 1111 1111 1111 for the deposit. GG8CANARY-THREADSENS-1", "from_me": False})
    st, text, done = c.stream("draft.begin", {"context": cx}, timeout=120)
    reqs = B.cloud_reqs(lab, t)
    b = B.body(reqs)
    lab.check("draft.thread_with_card_number", "4111 1111 1111 1111" not in b,
              f"status={st if isinstance(st, dict) else st} sent_card={'4111 1111 1111 1111' in b}", severity="Medium")
    B.save_requests(lab, "draft-cardthread", reqs)
    B.ledger_covers(lab, "draft2", t, "draft", corpus)
    # allow screenshots -> restart with allow_cloud_screenshots
    c.close()
    lab.stop_daemon()
    import os as _o
    cfgp = _o.path.join(lab.home, "config", "config.toml")
    open(cfgp, "a").write("\n[draft]\nallow_cloud_screenshots = true\n")
    lab.start_daemon()
    c = lab.client()
    t = time.time()
    st, text, done = c.stream("draft.begin", {"context": ctx(shot=True)}, timeout=120)
    reqs = B.cloud_reqs(lab, t)
    b = B.body(reqs)
    lab.note(f"allow_cloud_screenshots=true: image sent={SHOT[:40] in b or 'image' in b}")
    B.save_requests(lab, "draft-shot-allowed", reqs)
    B.keys_only_to_own_host(lab, "keys", {"GG8CANARY-APIKEY-anthropic": "api.anthropic.com"})
    c.close()
