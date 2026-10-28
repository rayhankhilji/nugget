import { SourceIcon } from "../components/SourceIcon";

/** Like `SourceIcon` (inline, decorative), with generic marks for the M6 connector kinds. */
export function KindIcon({ kind, size = 20 }: { kind: string; size?: number }) {
  const common = {
    width: size,
    height: size,
    viewBox: "0 0 24 24",
    fill: "none",
    stroke: "currentColor",
    strokeWidth: 1.7,
    strokeLinecap: "round" as const,
    strokeLinejoin: "round" as const,
    "aria-hidden": true,
    focusable: false,
    className: `source-icon source-icon-${kind}`,
  };
  switch (kind) {
    case "outlook":
      return <SourceIcon kind="imap" size={size} />;
    case "outlook-calendar":
      return <SourceIcon kind="caldav" size={size} />;
    case "onedrive":
      return (
        <svg {...common}>
          <path d="M7 18.5h11a3.5 3.5 0 0 0 .4-7A5.5 5.5 0 0 0 8 9.6 4.5 4.5 0 0 0 7 18.5z" />
        </svg>
      );
    case "slack":
      return (
        <svg {...common}>
          <path d="M9.5 3.5v17M14.5 3.5v17M3.5 9.5h17M3.5 14.5h17" />
        </svg>
      );
    case "notion":
      return (
        <svg {...common}>
          <rect x="4" y="3.5" width="16" height="17" rx="1.8" />
          <path d="M8.5 16.5v-9l7 9v-9" />
        </svg>
      );
    case "github":
      return (
        <svg {...common}>
          <circle cx="6.5" cy="5.5" r="2" />
          <circle cx="6.5" cy="18.5" r="2" />
          <circle cx="17.5" cy="8.5" r="2" />
          <path d="M6.5 7.5v9M17.5 10.5c0 4-5 3-9.5 6.5" />
        </svg>
      );
    case "linear":
      return (
        <svg {...common}>
          <circle cx="12" cy="12" r="8.5" />
          <path d="m5.5 13.5 5 5M4 10l10 10M6.5 6.5l11 11" />
        </svg>
      );
    default:
      return <SourceIcon kind={kind} size={size} />;
  }
}
