/** Entry points the Add source wizard delegates to for the Microsoft 365, SaaS and messaging kinds. */
import type { ConnectorKind, MessagingKind, MicrosoftKind, SaasKind } from "../api";
import type { MessageKey } from "../i18n";
import type { FormProps } from "./form";
import { isMessagingKind, isMicrosoftKind, isSaasKind } from "./kinds";
import { MessagingForm } from "./MessagingForm";
import { MicrosoftForm } from "./MicrosoftForm";
import { SaasForm } from "./SaasForm";

export type NewKind = MicrosoftKind | SaasKind | MessagingKind;

export const isNewKind = (k: ConnectorKind): k is NewKind => isMicrosoftKind(k) || isSaasKind(k) || isMessagingKind(k);

export const NEW_KIND_TITLE: Record<NewKind, MessageKey> = {
  outlook: "addSource.addTitle.outlook",
  "outlook-calendar": "addSource.addTitle.outlook-calendar",
  onedrive: "addSource.addTitle.onedrive",
  slack: "addSource.addTitle.slack",
  notion: "addSource.addTitle.notion",
  github: "addSource.addTitle.github",
  linear: "addSource.addTitle.linear",
  imessage: "addSource.addTitle.imessage",
  whatsapp: "addSource.addTitle.whatsapp",
  telegram: "addSource.addTitle.telegram",
};

export const NEW_KIND_BLURB: Record<NewKind, MessageKey> = {
  outlook: "addSource.blurb.outlook",
  "outlook-calendar": "addSource.blurb.outlook-calendar",
  onedrive: "addSource.blurb.onedrive",
  slack: "addSource.blurb.slack",
  notion: "addSource.blurb.notion",
  github: "addSource.blurb.github",
  linear: "addSource.blurb.linear",
  imessage: "addSource.blurb.imessage",
  whatsapp: "addSource.blurb.whatsapp",
  telegram: "addSource.blurb.telegram",
};

export function NewKindForm({ kind, ...rest }: FormProps<NewKind>) {
  if (isMessagingKind(kind)) return <MessagingForm kind={kind} {...rest} />;
  return isMicrosoftKind(kind) ? <MicrosoftForm kind={kind} {...rest} /> : <SaasForm kind={kind} {...rest} />;
}
