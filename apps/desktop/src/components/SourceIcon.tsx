/** Inline kind icons for every connector kind (no remote assets). Decorative: the kind is always also given as text. */
export function SourceIcon({ kind, size = 20 }: { kind: string; size?: number }) {
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
    case "files":
      return (
        <svg {...common}>
          <path d="M3 6.5A1.5 1.5 0 0 1 4.5 5h4.2l2 2h8.8A1.5 1.5 0 0 1 21 8.5v9a1.5 1.5 0 0 1-1.5 1.5h-15A1.5 1.5 0 0 1 3 17.5z" />
        </svg>
      );
    case "obsidian":
      return (
        <svg {...common}>
          <path d="M12 2.5 18.5 7l-1.5 10-5 4.5-5-4.5L5.5 7z" />
          <path d="M12 2.5 10 12l2 9.5M10 12l-4.5-5M10 12l7 5" />
        </svg>
      );
    case "gmail":
    case "imap":
    case "outlook":
      return (
        <svg {...common}>
          <rect x="3" y="5" width="18" height="14" rx="1.8" />
          <path d="m3.5 6 8.5 7 8.5-7" />
        </svg>
      );
    case "gcal":
    case "caldav":
    case "outlook-calendar":
      return (
        <svg {...common}>
          <rect x="3.5" y="5" width="17" height="15.5" rx="1.8" />
          <path d="M3.5 9.5h17M8 3v4M16 3v4M8 13.5h2M14 13.5h2M8 17h2" />
        </svg>
      );
    case "gdrive":
      return (
        <svg {...common}>
          <path d="M8.5 3.5h7l6 10.5-3.5 6h-12L2.5 14z" />
          <path d="M8.5 3.5 15 14.5H2.5M15.5 3.5 9 14.5l-3 5.5" />
        </svg>
      );
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
    case "imessage":
      // A rounded speech bubble with a tail.
      return (
        <svg {...common}>
          <path d="M12 4.5c4.7 0 8.5 3 8.5 6.8s-3.8 6.8-8.5 6.8c-1 0-2-.1-2.9-.4L5 19.5l1.1-3.4C4.4 14.9 3.5 13.2 3.5 11.3c0-3.8 3.8-6.8 8.5-6.8z" />
        </svg>
      );
    case "whatsapp":
      // A round bubble with a tail at the bottom left and a handset-like stroke inside.
      return (
        <svg {...common}>
          <path d="M4.5 19.5l1.2-3.6A8.5 8.5 0 1 1 8.4 18.6z" />
          <path d="M9 8.8c0 3 2.2 5.6 5.4 6.2l1-1.4-1.9-1-.9.9c-1-.5-1.8-1.3-2.3-2.3l.9-.9-1-1.9z" />
        </svg>
      );
    case "telegram":
      // A paper plane.
      return (
        <svg {...common}>
          <path d="M20.5 4 3.5 11l5.5 2 2 6 3-4 4.5 3.5z" />
          <path d="m9 13 8-6.5" />
        </svg>
      );
    default:
      return (
        <svg {...common}>
          <ellipse cx="12" cy="6" rx="7.5" ry="3" />
          <path d="M4.5 6v12c0 1.7 3.4 3 7.5 3s7.5-1.3 7.5-3V6M4.5 12c0 1.7 3.4 3 7.5 3s7.5-1.3 7.5-3" />
        </svg>
      );
  }
}
