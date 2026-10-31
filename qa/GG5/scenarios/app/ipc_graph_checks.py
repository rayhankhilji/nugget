#!/usr/bin/env python3
"""GG5 app scenario: knowledge.graph IPC consistency against knowledge.entities/entity.

Usage: python3 qa/GG5/scenarios/app/ipc_graph_checks.py <socket> <outdir>
Read-only. Prints findings and writes <outdir>/ipc_graph.json with the raw answers.
"""
import json
import sys
from collections import deque
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[2] / "harness"))
from nugget_qa import Client, RpcError  # noqa: E402

sock, out = sys.argv[1], Path(sys.argv[2])
out.mkdir(parents=True, exist_ok=True)
c = Client(sock)
raw = {}

ents = c.call("knowledge.entities", {"limit": 500, "offset": 0})
det = {e["id"]: c.call("knowledge.entity", {"id": e["id"]}) for e in ents}
byname = {}
for e in ents:
    byname.setdefault(e["name"], []).append(e["id"])
claims = {}
for d in det.values():
    for cl in d["claims"]:
        claims[cl["id"]] = cl
status = {k: v["status"] for k, v in claims.items()}
ent_edges = {k: v for k, v in claims.items() if v["object"].get("entity")}
print(f"entities={len(ents)} claims={len(claims)} entity->entity claims={len(ent_edges)}")
print("statuses:", {s: sum(1 for x in status.values() if x == s) for s in set(status.values())})


def check_edges(tag, g):
    bad = [(e["claim"], status.get(e["claim"]), e["predicate"]) for e in g["edges"] if status.get(e["claim"]) not in ("active", "user_asserted")]
    nodes = {n["id"] for n in g["nodes"]}
    dangling = [e["claim"] for e in g["edges"] if e["from"] not in nodes or e["to"] not in nodes]
    dup = len(g["nodes"]) - len(nodes)
    print(f"[{tag}] nodes={len(g['nodes'])} edges={len(g['edges'])} truncated={g['truncated']} non-current-edges={bad} dangling={dangling} dup_nodes={dup}")
    return bad


def name(i):
    return det[i]["name"] if i in det else i


# 1. overview
ov = c.call("knowledge.graph", {"center": None, "depth": 1, "limit": 200, "kinds": []})
raw["overview"] = ov
check_edges("overview", ov)
missing = set(det) - {n["id"] for n in ov["nodes"]}
print("  overview missing entities:", [name(i) for i in missing])
cur_edges = {k for k, v in ent_edges.items() if v["status"] in ("active", "user_asserted")}
print("  current entity->entity claims not in overview edges:", sorted(cur_edges - {e["claim"] for e in ov["edges"]}))
noncur = {k: (v["status"], v["text"]) for k, v in ent_edges.items() if v["status"] not in ("active", "user_asserted")}
print("  non-current entity claims (should not be edges):", noncur)

# 2. centred on Mara, depths 1..3, compared with a BFS over current entity claims
mara = byname["Mara Okafor"][0]
adj = {}
for k in cur_edges:
    v = ent_edges[k]
    s, o = v["subject"] if "subject" in v else None, v["object"]["entity"]["id"]
    adj.setdefault(o, set())
for d in det.values():
    for cl in d["claims"]:
        if cl["id"] in cur_edges:
            o = cl["object"]["entity"]["id"]
            # subject: the claim appears on both entities' pages; find subject as the entity that is not the object (or self)
for d in det.values():
    for cl in d["claims"]:
        if cl["id"] in cur_edges:
            o = cl["object"]["entity"]["id"]
            if o != d["id"]:
                adj.setdefault(d["id"], set()).add(o)
                adj.setdefault(o, set()).add(d["id"])


def bfs(start, depth):
    seen = {start: 0}
    q = deque([start])
    while q:
        x = q.popleft()
        if seen[x] >= depth:
            continue
        for y in adj.get(x, ()):
            if y not in seen:
                seen[y] = seen[x] + 1
                q.append(y)
    return seen


for depth in (1, 2, 3):
    g = c.call("knowledge.graph", {"center": mara, "depth": depth, "limit": 200, "kinds": []})
    raw[f"mara_d{depth}"] = g
    check_edges(f"mara d{depth}", g)
    exp = bfs(mara, depth)
    got = {n["id"]: n["depth"] for n in g["nodes"]}
    print(f"  expected {len(exp)} nodes; missing={[name(i) for i in set(exp) - set(got)]} extra={[name(i) for i in set(got) - set(exp)]} depth-mismatch={[(name(i), got[i], exp[i]) for i in got if i in exp and got[i] != exp[i]]}")
    if depth == 1:
        print("  d1 neighbours:", sorted(name(i) for i in got if i != mara))

# 3. kinds filter
for kinds in (["person"], ["person", "organization"], ["event"]):
    g = c.call("knowledge.graph", {"center": None, "depth": 1, "limit": 200, "kinds": kinds})
    raw[f"overview_{'+'.join(kinds)}"] = g
    check_edges(f"overview kinds={kinds}", g)
    wrong = [n["name"] for n in g["nodes"] if n["kind"] not in kinds]
    exp = [e["name"] for e in ents if e["kind"] in kinds]
    print(f"  wrong-kind nodes={wrong}; expected {len(exp)} got {len(g['nodes'])}")
for kinds in (["person"], ["organization"]):
    g = c.call("knowledge.graph", {"center": mara, "depth": 2, "limit": 200, "kinds": kinds})
    raw[f"mara_d2_{'+'.join(kinds)}"] = g
    check_edges(f"mara d2 kinds={kinds}", g)
    linked = {e["from"] for e in g["edges"]} | {e["to"] for e in g["edges"]}
    iso = [(n["name"], n["depth"]) for n in g["nodes"] if n["id"] not in linked]
    print(f"  nodes={[ (n['name'], n['kind'], n['depth']) for n in g['nodes']]}")
    print(f"  nodes with no drawn edge (reached only via filtered-out kinds): {iso}")

# 4. source filter
srcs = c.call("sources.list")
for s in srcs:
    g = c.call("knowledge.graph", {"center": None, "depth": 1, "limit": 200, "kinds": [], "source": s["id"]})
    raw[f"overview_source_{s['id']}"] = g
    check_edges(f"overview source={s['name']}", g)
    g2 = c.call("knowledge.graph", {"center": mara, "depth": 2, "limit": 200, "kinds": [], "source": s["id"]})
    check_edges(f"mara d2 source={s['name']}", g2)
for bogus in ("no-such-source",):
    for center in (None, mara):
        try:
            g = c.call("knowledge.graph", {"center": center, "depth": 2, "limit": 200, "kinds": [], "source": bogus})
            raw[f"bogus_{center}"] = g
            print(f"[bogus source center={name(center) if center else None}] nodes={[n['name'] for n in g['nodes']]} edges={len(g['edges'])} truncated={g['truncated']}")
        except RpcError as e:
            print(f"[bogus source center={center}] error {e}")

# 5. limits / truncated
for lim in (1, 5, 44, 45, 46):
    g = c.call("knowledge.graph", {"center": None, "depth": 1, "limit": lim, "kinds": []})
    print(f"[overview limit={lim}] nodes={len(g['nodes'])} truncated={g['truncated']}")
for lim in (1, 5, 19, 20):
    g = c.call("knowledge.graph", {"center": mara, "depth": 1, "limit": lim, "kinds": []})
    print(f"[mara d1 limit={lim}] nodes={len(g['nodes'])} truncated={g['truncated']} edges={len(g['edges'])}")
g = c.call("knowledge.graph", {"center": mara, "depth": 2, "limit": 5, "kinds": ["organization"]})
print(f"[mara d2 limit=5 kinds=organization] nodes={[(n['name'], n['kind']) for n in g['nodes']]} truncated={g['truncated']}")
g = c.call("knowledge.graph", {"center": mara, "depth": 2, "limit": 0, "kinds": []})
print(f"[mara d2 limit=0] nodes={len(g['nodes'])} truncated={g['truncated']}")
for bad in ({"center": "ent_doesnotexist", "depth": 1, "limit": 10, "kinds": []}, {"center": None, "depth": 1, "limit": 10, "kinds": ["planet"]}, {"center": mara, "depth": 99, "limit": 10, "kinds": []}):
    try:
        g = c.call("knowledge.graph", bad)
        print(f"[{bad}] ok nodes={len(g['nodes'])} truncated={g['truncated']}")
    except RpcError as e:
        print(f"[{bad}] error {e}")

# 6. sensitive: health entities in the graph
for nm in ("Jonas Albers", "Counselling session", "Albers Counselling, 3 Quay Lane, Halden Bay", "Dr. Lena Brandt", "health"):
    for i in byname.get(nm, []):
        g = c.call("knowledge.graph", {"center": i, "depth": 1, "limit": 50, "kinds": []})
        print(f"[sensitive {nm}] in overview={i in {n['id'] for n in ov['nodes']}} d1 neighbours={[n['name'] for n in g['nodes'] if n['id'] != i]} attrs={det[i]['attributes']}")

(out / "ipc_graph.json").write_text(json.dumps(raw, indent=1, ensure_ascii=False))
c.close()
