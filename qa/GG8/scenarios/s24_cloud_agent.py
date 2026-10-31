"""Area B: the agent with a cloud agentic provider (mock drives a search-like tool, then finish).
What goes to the cloud: the goal, observations, context from the user's data. Sensitive items must
not be in tool results sent to the cloud; ledger feature `agent`."""
import os
import sys
import time
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import b_common as B  # noqa: E402

NAME = "s24_cloud_agent"
EXTRA_MOCKS = ["mock_agent"]
CONFIG = """
[[providers]]
id = "anthropic"
kind = "anthropic"
model = "claude-mock"
api_key = "env:ANTHROPIC_API_KEY"
tiers = ["light", "synthesis", "agentic"]
"""
ENV = {"ANTHROPIC_API_KEY": "sk-ant-GG8CANARY-APIKEY-anthropic"}


def run(lab):
    _, corpus = B.add_corpus(lab)
    c = lab.client()
    t = time.time()
    st = c.call("agent.run", {"goal": "Find how the Acme renewal will be paid and note it. GG8CANARY-GOAL-1"}, timeout=60)
    lab.note(f"agent.run -> {st}")
    ev = c.wait_note(lambda n: n.get("method") == "agent.event" and
                     any(k in str(n.get("params")) for k in ("finished", "failed", "done", "Finished", "cancel")), timeout=120)
    lab.note(f"last event: {str(ev)[:600]}")
    time.sleep(3)
    try:
        task = lab.call("agent.get", {"task_id": st["task_id"]})
        lab.note(f"agent.get: {str(task)[:1500]}")
    except Exception as e:  # noqa: BLE001
        lab.note(f"agent.get: {e}")
    reqs = B.cloud_reqs(lab, t)
    lab.check("agent.requests", len(reqs) > 0, f"{len(reqs)} requests; tools offered: "
              f"{[x.get('name') for x in (__import__('json').loads(reqs[0]['body']).get('tools') or [])] if reqs else None}")
    B.no_secrets(lab, "agent", reqs)
    B.ledger_covers(lab, "agent", t, "agent", corpus)
    B.save_requests(lab, "agent", reqs)
    c.close()
