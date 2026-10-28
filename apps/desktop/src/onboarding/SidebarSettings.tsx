import { useT } from "../i18n";
import { LanguagePicker } from "../i18n/LanguagePicker";
import { restartOnboarding } from "./machine";
import "./onboarding.css";

/** Small settings block at the foot of the sidebar: language, and running setup again. */
export function SidebarSettings({ onSetup }: { onSetup: () => void }) {
  const t = useT();
  return (
    <div className="sidebar-settings">
      <LanguagePicker />
      <button
        type="button"
        className="btn btn-ghost btn-small"
        onClick={() => {
          restartOnboarding();
          onSetup();
        }}
      >
        {t("settings.setupAgain")}
      </button>
    </div>
  );
}
