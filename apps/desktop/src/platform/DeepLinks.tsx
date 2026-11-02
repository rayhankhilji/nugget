/**
 * Acts on `nugget://` links and opened `.nuggetplugin` files, already validated by the shell
 * (`src-tauri/src/deeplink.rs`). Takes the waiting link at mount and on each `nugget:deeplink`
 * event. A page just navigates and a search is only typed in (Enter runs it); opening an item or
 * installing a plugin asks first.
 */
import { useCallback, useEffect, useState } from "react";
import { api, errorMessage, platformApi, type DeepLink, type PageId } from "../api";
import { useEvent } from "../hooks";
import { requestPluginInstall, requestSearch } from "../hooks.nav";
import { useT } from "../i18n";
import { Dialog } from "../components/Dialog";
import { Spinner } from "../components/ui";

/** How many hits to look through for the linked item. */
const ITEM_SEARCH_LIMIT = 50;

type Ask = Extract<DeepLink, { kind: "item" } | { kind: "plugin" }>;

/** The file name shown for a plugin path (either separator). */
export function fileName(path: string): string {
  return path.split(/[\\/]/).filter(Boolean).pop() ?? path;
}

/** Find the URL of the item a link names, by searching for its key. */
export async function findItemUrl(source: string, key: string): Promise<string | null> {
  const hits = await api.search(key, ITEM_SEARCH_LIMIT);
  const hit = hits.find((h) => h.item.source === source && h.item.key === key && h.url);
  return hit?.url ?? null;
}

export function DeepLinks({ pages, go }: { pages: readonly PageId[]; go: (page: PageId) => void }) {
  const [ask, setAsk] = useState<Ask | null>(null);

  const handle = useCallback(
    (link: DeepLink) => {
      switch (link.kind) {
        case "page":
          if (pages.includes(link.page)) go(link.page);
          return;
        case "search":
          requestSearch(link.query, false);
          go("search");
          return;
        case "item":
        case "plugin":
          setAsk(link);
          return;
        case "ask":
          // The shell sends questions to the quick-ask box itself.
          return;
        case "connections":
        case "billing":
          // The page reloads on `nugget:cloud-connections` / `nugget:cloud-billing`.
          if (pages.includes("connections")) go("connections");
          return;
      }
    },
    [pages, go],
  );

  const take = useCallback(() => {
    platformApi
      .takeDeepLink()
      .then((link) => link && handle(link))
      .catch(() => {
        /* outside the app */
      });
  }, [handle]);

  useEffect(take, [take]);
  useEvent(platformApi.onDeepLink, () => take());

  if (!ask) return null;
  return ask.kind === "item" ? (
    <OpenItem link={ask} onClose={() => setAsk(null)} />
  ) : (
    <InstallPlugin
      link={ask}
      onClose={() => setAsk(null)}
      onContinue={() => {
        setAsk(null);
        requestPluginInstall(ask.path);
        go("plugins");
      }}
    />
  );
}

function OpenItem({ link, onClose }: { link: Extract<DeepLink, { kind: "item" }>; onClose: () => void }) {
  const t = useT();
  const [busy, setBusy] = useState(false);
  const [note, setNote] = useState<string | null>(null);

  const open = async () => {
    setBusy(true);
    setNote(null);
    try {
      const url = await findItemUrl(link.source, link.key);
      if (!url) {
        setNote(t("deeplink.itemNotFound"));
        return;
      }
      await api.openUrl(url);
      onClose();
    } catch (e) {
      setNote(errorMessage(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <Dialog title={t("deeplink.itemTitle")} onClose={onClose}>
      <p>{t("deeplink.itemBody", { key: link.key, source: link.source })}</p>
      {note && (
        <p className="note" role="alert">
          {note}
        </p>
      )}
      <div className="dialog-actions">
        <button type="button" className="btn" onClick={onClose}>
          {t("common.cancel")}
        </button>
        <button type="button" className="btn btn-primary" onClick={() => void open()} disabled={busy}>
          {busy ? <Spinner label={t("deeplink.searching")} /> : t("deeplink.open")}
        </button>
      </div>
    </Dialog>
  );
}

function InstallPlugin({
  link,
  onClose,
  onContinue,
}: {
  link: Extract<DeepLink, { kind: "plugin" }>;
  onClose: () => void;
  onContinue: () => void;
}) {
  const t = useT();
  return (
    <Dialog title={t("deeplink.pluginTitle")} onClose={onClose}>
      <p>{t("deeplink.pluginBody", { file: fileName(link.path) })}</p>
      <div className="dialog-actions">
        <button type="button" className="btn" onClick={onClose}>
          {t("common.cancel")}
        </button>
        <button type="button" className="btn btn-primary" onClick={onContinue}>
          {t("deeplink.pluginContinue")}
        </button>
      </div>
    </Dialog>
  );
}
