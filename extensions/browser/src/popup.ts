// Toolbar popup: per-site consent (optional host permissions, requested on a user gesture),
// connection status and the kill switch.

const $ = (id: string) => document.getElementById(id) as HTMLElement;

async function currentOrigin(): Promise<string | undefined> {
  const [tab] = await chrome.tabs.query({ active: true, currentWindow: true });
  // Opening the popup grants `activeTab` for this tab, which is what makes `tab.url` visible
  // here. It does not count as consent: the background checks the persistent grant only.
  if (!tab?.url) return undefined;
  try {
    const u = new URL(tab.url);
    return /^https?:$/.test(u.protocol) ? u.origin : undefined;
  } catch {
    return undefined;
  }
}

async function render(): Promise<void> {
  const origin = await currentOrigin();
  const status = (await chrome.runtime.sendMessage({ nugget: "status" })) as { connected: boolean; halted: boolean };
  $("site").textContent = origin ?? "This page can't be controlled";
  const granted = origin ? await chrome.permissions.contains({ origins: [`${origin}/*`] }) : false;
  ($("grant") as HTMLButtonElement).disabled = !origin;
  $("grant").hidden = granted;
  $("revoke").hidden = !granted;
  $("stop").hidden = status.halted;
  $("resume").hidden = !status.halted;
  $("status").textContent = [
    status.connected ? "Connected to Nugget" : "Nugget app not running",
    status.halted ? "stopped" : granted ? "allowed on this site" : "not allowed on this site",
  ].join(" · ");
}

$("grant").addEventListener("click", async () => {
  const origin = await currentOrigin();
  if (origin) await chrome.permissions.request({ origins: [`${origin}/*`] });
  await render();
});
$("revoke").addEventListener("click", async () => {
  const origin = await currentOrigin();
  if (origin) await chrome.permissions.remove({ origins: [`${origin}/*`] });
  await render();
});
$("stop").addEventListener("click", async () => {
  await chrome.runtime.sendMessage({ nugget: "user_stop", reason: "popup stop button" });
  await render();
});
$("resume").addEventListener("click", async () => {
  await chrome.runtime.sendMessage({ nugget: "resume" });
  await render();
});

void render();
