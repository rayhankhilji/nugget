import type { PageId } from "../api";
import { requestPage } from "../hooks.nav";
import { useT, type MessageKey } from "../i18n";

/**
 * A small "Go to <Page>" button for guidance that points at another page ("Add a source on the
 * Sources page"). The app shell listens for `requestPage` and navigates there.
 */
export function GoToPage({ page, className = "btn" }: { page: PageId; className?: string }) {
  const t = useT();
  return (
    <button type="button" className={className} onClick={() => requestPage(page)}>
      {t("common.goToPage", { page: t(`app.nav.${page}` as MessageKey) })}
    </button>
  );
}
