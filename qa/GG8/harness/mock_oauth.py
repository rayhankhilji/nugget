"""GG8 OAuth token endpoints (Google, Microsoft, Linear, GitHub device flow) and stub APIs.

Token endpoints answer any code/refresh grant with synthetic canary tokens that expire in 60 s, so a
refresh follows soon. The resource APIs answer 401/empty: GG8 only checks where tokens go.
"""
import json
from urllib.parse import parse_qs

HOSTS = {"oauth2.googleapis.com", "gmail.googleapis.com", "www.googleapis.com",
         "login.microsoftonline.com", "graph.microsoft.com", "github.com", "api.github.com",
         "accounts.google.com"}
import os
N = {"n": 0}
EXPIRES = int(os.environ.get("GG8_OAUTH_EXPIRES", "60"))


def j(status, obj):
    return status, {"Content-Type": "application/json"}, json.dumps(obj).encode()


def respond(host, method, path, headers, body, server):
    form = {k: v[0] for k, v in parse_qs(body.decode("utf-8", "replace")).items()}
    p = path.split("?")[0]
    if (host, p) in (("oauth2.googleapis.com", "/token"),) or p.endswith("/oauth2/v2.0/token"):
        N["n"] += 1
        tag = "google" if "google" in host else "microsoft"
        return j(200, {"access_token": f"GG8CANARY-ACCESS-{tag}-{N['n']}", "token_type": "Bearer",
                       "expires_in": EXPIRES, "refresh_token": f"GG8CANARY-REFRESH-{tag}-{N['n']}",
                       "scope": form.get("scope", "openid"), "id_token": None})
    if host == "github.com" and p == "/login/device/code":
        return j(200, {"device_code": "GG8CANARY-DEVICECODE", "user_code": "GG8-0000",
                       "verification_uri": "https://github.com/login/device", "expires_in": 900,
                       "interval": 5})
    if host == "github.com" and p == "/login/oauth/access_token":
        return j(200, {"error": "authorization_pending"})
    if host == "gmail.googleapis.com" and "/profile" in p:
        return j(200, {"emailAddress": "ada@gg8-synthetic.test", "historyId": "1",
                       "messagesTotal": 0, "threadsTotal": 0})
    if host == "gmail.googleapis.com" and p.endswith("/messages"):
        return j(200, {"messages": [], "resultSizeEstimate": 0})
    if host == "gmail.googleapis.com" and p.endswith("/labels"):
        return j(200, {"labels": [{"id": "INBOX", "name": "INBOX", "type": "system"}]})
    return j(404, {"error": {"code": 404, "message": "gg8 stub"}})
