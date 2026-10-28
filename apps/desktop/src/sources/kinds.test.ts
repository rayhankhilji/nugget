import { describe, expect, it } from "vitest";
import {
  connectMode,
  EMPTY_MICROSOFT,
  EMPTY_SAAS,
  isDrivePath,
  isGuid,
  isRepo,
  isValidTenant,
  microsoftErrors,
  microsoftSettings,
  saasErrors,
  saasSettings,
} from "./kinds";

const GUID = "1b2c3d4e-1111-2222-3333-444455556666";

describe("microsoftSettings", () => {
  it("sends only client_id when the optional fields are blank", () => {
    for (const kind of ["outlook", "outlook-calendar", "onedrive"] as const) {
      expect(microsoftSettings(kind, { ...EMPTY_MICROSOFT, clientId: ` ${GUID} `, tenant: "  " })).toEqual({ client_id: GUID });
    }
  });

  it("adds tenant, account hint and the kind's folders", () => {
    const f = { ...EMPTY_MICROSOFT, clientId: GUID, tenant: "contoso.onmicrosoft.com", accountHint: "me@contoso.com", folders: "inbox, archive", includeFolders: "/Documents\n/Work", excludeFolders: "/Pictures" };
    expect(microsoftSettings("outlook", f)).toEqual({ client_id: GUID, tenant: "contoso.onmicrosoft.com", account_hint: "me@contoso.com", folders: ["inbox", "archive"] });
    expect(microsoftSettings("onedrive", f)).toEqual({
      client_id: GUID,
      tenant: "contoso.onmicrosoft.com",
      account_hint: "me@contoso.com",
      include_folders: ["/Documents", "/Work"],
      exclude_folders: ["/Pictures"],
    });
    expect(microsoftSettings("outlook-calendar", f)).toEqual({ client_id: GUID, tenant: "contoso.onmicrosoft.com", account_hint: "me@contoso.com" });
  });

  it("validates the client id, tenant and paths", () => {
    expect(microsoftErrors("outlook", EMPTY_MICROSOFT).clientId).toBe("addSource.microsoft.clientIdMissing");
    expect(microsoftErrors("outlook", { ...EMPTY_MICROSOFT, clientId: "abc" }).clientId).toBe("addSource.microsoft.clientIdInvalid");
    expect(microsoftErrors("outlook", { ...EMPTY_MICROSOFT, clientId: GUID })).toEqual({});
    expect(microsoftErrors("outlook", { ...EMPTY_MICROSOFT, clientId: GUID, tenant: "a/b" }).tenant).toBeDefined();
    expect(microsoftErrors("outlook", { ...EMPTY_MICROSOFT, clientId: GUID, folders: "in box" }).folders).toBeDefined();
    expect(microsoftErrors("onedrive", { ...EMPTY_MICROSOFT, clientId: GUID, includeFolders: "/a/../b" }).includeFolders).toBeDefined();
    expect(isGuid(GUID)).toBe(true);
    for (const ok of ["common", "organizations", "consumers", GUID, "contoso.onmicrosoft.com"]) expect(isValidTenant(ok)).toBe(true);
    for (const bad of ["", "-x", "x.", "a..b", "a/b", "a b", "x?y"]) expect(isValidTenant(bad)).toBe(false);
    expect(isDrivePath("/Documents/Work")).toBe(true);
    expect(isDrivePath("/a/./b")).toBe(false);
  });
});

describe("saasSettings", () => {
  it("sends {} for the defaults", () => {
    for (const kind of ["slack", "notion", "github", "linear"] as const) expect(saasSettings(kind, EMPTY_SAAS)).toEqual({});
  });

  it("github: client_id only in OAuth mode", () => {
    const f = { ...EMPTY_SAAS, clientId: "Iv1.abc" };
    expect(saasSettings("github", f)).toEqual({});
    expect(saasSettings("github", { ...f, method: "oauth" })).toEqual({ client_id: "Iv1.abc" });
    expect(saasSettings("slack", { ...f, method: "oauth" })).toEqual({});
  });

  it("github: repos, all_activity only with repos, discussions off", () => {
    expect(saasSettings("github", { ...EMPTY_SAAS, allActivity: true })).toEqual({});
    expect(saasSettings("github", { ...EMPTY_SAAS, repos: "a/b, c/d", allActivity: true, includeDiscussions: false })).toEqual({
      repos: ["a/b", "c/d"],
      all_activity: true,
      include_discussions: false,
    });
  });

  it("slack, notion and linear options", () => {
    expect(saasSettings("slack", { ...EMPTY_SAAS, conversations: "#general, C012", includeDms: false })).toEqual({ conversations: ["#general", "C012"], include_dms: false });
    expect(saasSettings("notion", { ...EMPTY_SAAS, includeDatabases: false })).toEqual({ include_databases: false });
    expect(saasSettings("linear", { ...EMPTY_SAAS, method: "oauth", clientId: "lin-client", include: ["created", "assigned"], teams: "ENG, OPS" })).toEqual({
      client_id: "lin-client",
      include: ["assigned", "created"],
      teams: ["ENG", "OPS"],
    });
  });

  it("validates tokens and options", () => {
    expect(saasErrors("notion", EMPTY_SAAS, "", false).token).toBe("addSource.saas.tokenMissing");
    expect(saasErrors("notion", EMPTY_SAAS, "", true)).toEqual({});
    expect(saasErrors("slack", EMPTY_SAAS, "xoxb-123", false).token).toBe("addSource.slack.botToken");
    expect(saasErrors("slack", EMPTY_SAAS, "xoxp-123", false)).toEqual({});
    expect(saasErrors("slack", { ...EMPTY_SAAS, includeDms: false, includeChannels: false }, "xoxp-1", false).conversations).toBeDefined();
    expect(saasErrors("github", { ...EMPTY_SAAS, method: "oauth" }, "", false)).toEqual({ clientId: "addSource.saas.clientIdMissing" });
    expect(saasErrors("github", { ...EMPTY_SAAS, repos: "nope" }, "t", false).repos).toBeDefined();
    expect(saasErrors("linear", { ...EMPTY_SAAS, include: [], teams: "E-NG" }, "t", false)).toEqual({
      include: "addSource.linear.includeMissing",
      teams: "addSource.linear.teamsInvalid",
    });
    expect(isRepo("octocat/hello-world")).toBe(true);
    expect(isRepo("../x")).toBe(false);
  });
});

describe("connectMode", () => {
  it("maps kinds and settings to how they (re)connect", () => {
    expect(connectMode("gmail")).toBe("browser");
    expect(connectMode("outlook-calendar", { client_id: "x" })).toBe("browser");
    expect(connectMode("imap")).toBe("password");
    expect(connectMode("files")).toBe("none");
    expect(connectMode("slack", {})).toBe("token");
    expect(connectMode("github", {})).toBe("token");
    expect(connectMode("github", { client_id: "x" })).toBe("device");
    expect(connectMode("linear", { client_id: "x" })).toBe("browser");
    expect(connectMode("notion", { token_env: "NOTION_TOKEN" })).toBe("env");
    expect(connectMode("dropbox")).toBe("none");
  });
});
