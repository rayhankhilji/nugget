import { describe, expect, it } from "vitest";
import {
  connectMode,
  EMPTY_MICROSOFT,
  EMPTY_SAAS,
  EMPTY_MESSAGING,
  isDrivePath,
  messagingErrors,
  messagingSettings,
  parseSinceDays,
  isGuid,
  isRepo,
  isValidTenant,
  microsoftErrors,
  saasErrors,
} from "./kinds";

const GUID = "1b2c3d4e-1111-2222-3333-444455556666";

describe("microsoftSettings", () => {
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
    expect(connectMode("gdrive", { client_id: "x" })).toBe("browser");
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

describe("messaging kinds", () => {
  it("sends the defaults explicitly and leaves blank optional fields out", () => {
    expect(messagingSettings("imessage", EMPTY_MESSAGING)).toEqual({ private: true, include_groups: true, contacts: true });
    expect(messagingSettings("whatsapp", { ...EMPTY_MESSAGING, path: " /Users/me/WhatsApp ", me: "  " })).toEqual({
      private: true,
      include_groups: true,
      folder: "/Users/me/WhatsApp",
    });
    expect(messagingSettings("telegram", { ...EMPTY_MESSAGING, path: "/home/me/Telegram Desktop", sinceDays: " " })).toEqual({
      private: true,
      include_groups: true,
      path: "/home/me/Telegram Desktop",
    });
  });

  it("checks the path and the number of days", () => {
    expect(messagingErrors("imessage", EMPTY_MESSAGING)).toEqual({});
    expect(messagingErrors("whatsapp", EMPTY_MESSAGING)).toEqual({ path: "addSource.messages.whatsappFolderMissing" });
    expect(messagingErrors("telegram", EMPTY_MESSAGING)).toEqual({ path: "addSource.messages.telegramPathMissing" });
    expect(messagingErrors("telegram", { ...EMPTY_MESSAGING, path: "Downloads/Telegram" })).toEqual({ path: "addSource.messages.fullPath" });
    expect(messagingErrors("telegram", { ...EMPTY_MESSAGING, path: "~/Downloads/Telegram Desktop/result.json" })).toEqual({});
    for (const bad of ["0", "-3", "1.5", "ten", "1e3"]) {
      expect(parseSinceDays(bad), bad).toBeNull();
      expect(messagingErrors("imessage", { ...EMPTY_MESSAGING, sinceDays: bad }).sinceDays).toBe("addSource.messages.sinceInvalid");
    }
    expect(parseSinceDays("365")).toBe(365);
  });
});
