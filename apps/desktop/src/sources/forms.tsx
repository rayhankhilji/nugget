/** Entry points the Add source wizard delegates to for the Microsoft 365 and SaaS kinds. */
import type { ConnectorKind, MicrosoftKind, SaasKind } from "../api";
import type { MessageKey } from "../i18n";
import type { FormProps } from "./form";
import { isMicrosoftKind, isSaasKind } from "./kinds";
import { MicrosoftForm } from "./MicrosoftForm";
import { SaasForm } from "./SaasForm";

export type NewKind = MicrosoftKind | SaasKind;

export const isNewKind = (k: ConnectorKind): k is NewKind => isMicrosoftKind(k) || isSaasKind(k);

export const NEW_KIND_TITLE: Record<NewKind, MessageKey> = {
  outlook: "addSource.addTitle.outlook",
  "outlook-calendar": "addSource.addTitle.outlook-calendar",
  onedrive: "addSource.addTitle.onedrive",
  slack: "addSource.addTitle.slack",
  notion: "addSource.addTitle.notion",
  github: "addSource.addTitle.github",
  linear: "addSource.addTitle.linear",
};

export const NEW_KIND_BLURB: Record<NewKind, MessageKey> = {
  outlook: "addSource.blurb.outlook",
  "outlook-calendar": "addSource.blurb.outlook-calendar",
  onedrive: "addSource.blurb.onedrive",
  slack: "addSource.blurb.slack",
  notion: "addSource.blurb.notion",
  github: "addSource.blurb.github",
  linear: "addSource.blurb.linear",
};

export function NewKindForm({ kind, ...rest }: FormProps<NewKind>) {
  return isMicrosoftKind(kind) ? <MicrosoftForm kind={kind} {...rest} /> : <SaasForm kind={kind} {...rest} />;
}
