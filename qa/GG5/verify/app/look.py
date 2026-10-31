import sys, json
sys.path.insert(0, "/home/user/nugget/qa/GG5/harness")
from nugget_qa import Client
c = Client(sys.argv[1])
ents = c.call("knowledge.entities", {"limit": 500, "offset": 0})
print("n entities", len(ents))
names = sorted((e["name"], e["kind"]) for e in ents)
print([n for n in names if any(k in n[0] for k in ("Chen","Sam","Gus","Tessell","Lumen","Corvid","Brightwell","Harbor","harbor","Jonas","Counsel","Albers","Dana","Mara"))])
def det(n, i=0):
    ids=[e["id"] for e in ents if e["name"]==n]
    return c.call("knowledge.entity", {"id": ids[i]})
for n in ("R. Chen","Sam"):
    d=det(n); print(n, "aliases", d.get("aliases"), "ids", d.get("identifiers"))
for i,e in enumerate([e for e in ents if e["name"]=="Gus Ferreira"]):
    d=c.call("knowledge.entity",{"id":e["id"]}); print("Gus",i,e, d.get("identifiers"), [cl["text"] for cl in d["claims"]])
d=det("Dana Whitfield")
for cl in d["claims"]:
    print("Dana", cl["id"], cl["status"], cl["predicate"], cl["text"], "upd", cl.get("updated_at"), "obs", [e.get("observed_at") for e in cl["evidence"]])
print("Dana keys", list(d.keys()))
print("claim keys", list(d["claims"][0].keys()))
d=det("Mara Okafor")
for cl in d["claims"]:
    if "Chen" in cl["text"] or "Counsel" in cl["text"]: print("Mara", cl["text"])
g=c.call("knowledge.graph",{"center":None,"depth":1,"limit":2001,"kinds":[]})
print("node keys", list(g["nodes"][0].keys()))
