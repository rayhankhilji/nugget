import { useId, useState } from "react";
import { LOCALES, localeInfo, matchLocale, savedLocale, setLocale, systemLanguages, useLocale, useT } from ".";

/** Language switcher: "System (…)" follows the OS; any other choice is remembered. */
export function LanguagePicker({ className }: { className?: string }) {
  const t = useT();
  const current = useLocale();
  const id = useId();
  const [choice, setChoice] = useState<string>(() => savedLocale() ?? "");
  const system = localeInfo(matchLocale(systemLanguages(), LOCALES.filter((l) => !l.stub)));
  // Stub catalogs (e.g. the RTL layout check) are only offered in development builds.
  const offered = LOCALES.filter((l) => !l.stub || import.meta.env.DEV || l.code === current.code);

  return (
    <div className={className ?? "field-inline"}>
      <label htmlFor={id}>{t("settings.language")}</label>
      <select
        id={id}
        value={choice}
        onChange={(e) => {
          const v = e.target.value;
          setChoice(v);
          void setLocale(v || null);
        }}
      >
        <option value="">{t("settings.languageSystem", { language: system.name })}</option>
        {offered.map((l) => (
          <option key={l.code} value={l.code} lang={l.code} dir={l.dir}>
            {l.name}
          </option>
        ))}
      </select>
    </div>
  );
}
