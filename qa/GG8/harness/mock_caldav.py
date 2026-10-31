"""GG8 CalDAV mock at caldav.gg8-synthetic.test (sinkholed, so classified cloud), Basic auth.

Discovery (PROPFIND depth 0 on the root) returns a calendar-home-set; the home (depth 1) lists one
calendar without a sync-token (so the client uses the ETag listing), which holds two events, one of
them with a card number. With GG8_CALDAV_EVIL_HOME=1 the calendar-home-set points at another host
(collector.gg8-evil.test), to check that the password is not sent there.
"""
import base64
import os

HOSTS = {"caldav.gg8-synthetic.test", "collector.gg8-evil.test"}
USER = "ada@gg8-synthetic.test"
PASSWORD = os.environ.get("GG8_CALDAV_PASSWORD", "GG8CANARY-CALDAVPW-1")

EVENTS = {
    "/cal/home/work/ev1.ics": ("\"e1\"", "Acme renewal review", "Discuss the Acme renewal terms with Priya. GG8CANARY-CAL-NORMAL-1"),
    "/cal/home/work/ev2.ics": ("\"e2\"", "Acme deposit", "Pay deposit with card 4111 1111 1111 1111. GG8CANARY-CAL-SENS-1"),
}


def ics(uid, summary, desc):
    return ("BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//GG8//EN\r\nBEGIN:VEVENT\r\n"
            f"UID:{uid}\r\nDTSTAMP:20261030T090000Z\r\nDTSTART:20261105T100000Z\r\n"
            f"DTEND:20261105T110000Z\r\nSUMMARY:{summary}\r\nDESCRIPTION:{desc}\r\n"
            "END:VEVENT\r\nEND:VCALENDAR\r\n")


def ms(inner):
    return (207, {"Content-Type": "application/xml; charset=utf-8"},
            ('<?xml version="1.0" encoding="utf-8"?>\n<D:multistatus xmlns:D="DAV:" '
             'xmlns:C="urn:ietf:params:xml:ns:caldav" xmlns:CS="http://calendarserver.org/ns/">'
             + inner + "</D:multistatus>").encode())


def resp(href, props):
    return (f"<D:response><D:href>{href}</D:href><D:propstat><D:prop>{props}</D:prop>"
            "<D:status>HTTP/1.1 200 OK</D:status></D:propstat></D:response>")


def respond(host, method, path, headers, body, server):
    auth = headers.get("Authorization") or headers.get("authorization") or ""
    want = "Basic " + base64.b64encode(f"{USER}:{PASSWORD}".encode()).decode()
    if auth != want:
        return 401, {"WWW-Authenticate": 'Basic realm="gg8"', "Content-Type": "text/plain"}, b"auth"
    if host == "collector.gg8-evil.test":
        return 404, {"Content-Type": "text/plain"}, b"evil"
    b = body.decode("utf-8", "replace")
    p = path.split("?")[0]
    depth = headers.get("Depth") or headers.get("depth")
    if method == "PROPFIND" and p in ("/", "/cal/", "/dav/") and depth == "0":
        home = ("https://collector.gg8-evil.test/cal/home/" if os.environ.get("GG8_CALDAV_EVIL_HOME")
                else "/cal/home/")
        return ms(resp(p, "<D:resourcetype><D:collection/></D:resourcetype>"
                          "<D:current-user-principal><D:href>/principals/ada/</D:href></D:current-user-principal>"
                          f"<C:calendar-home-set><D:href>{home}</D:href></C:calendar-home-set>"))
    if method == "PROPFIND" and p == "/cal/home/":
        return ms(resp("/cal/home/", "<D:resourcetype><D:collection/></D:resourcetype>")
                  + resp("/cal/home/work/", "<D:resourcetype><D:collection/><C:calendar/></D:resourcetype>"
                         "<D:displayname>Work</D:displayname><CS:getctag>ctag-1</CS:getctag>"
                         '<C:supported-calendar-component-set><C:comp name="VEVENT"/>'
                         "</C:supported-calendar-component-set>"))
    if method == "PROPFIND" and p == "/cal/home/work/":
        inner = resp("/cal/home/work/", "<D:resourcetype><D:collection/><C:calendar/></D:resourcetype>")
        for href, (etag, _, _) in EVENTS.items():
            inner += resp(href, f"<D:resourcetype/><D:getetag>{etag}</D:getetag>")
        return ms(inner)
    if method == "REPORT" and "calendar-multiget" in b:
        inner = ""
        for href, (etag, s, d) in EVENTS.items():
            if href in b:
                data = ics(href.rsplit("/", 1)[-1], s, d).replace("&", "&amp;").replace("<", "&lt;")
                inner += resp(href, f"<D:getetag>{etag}</D:getetag><C:calendar-data>{data}</C:calendar-data>")
        return ms(inner)
    if method == "REPORT":
        return 501, {"Content-Type": "text/plain"}, b"not implemented"
    return 404, {"Content-Type": "text/plain"}, b"nope"
