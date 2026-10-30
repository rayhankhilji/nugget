/** Inline kind icons (no remote assets). Decorative: the kind is always also given as text. */
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
      return (
        <svg {...common}>
          <rect x="3" y="5" width="18" height="14" rx="1.8" />
          <path d="m3.5 6 8.5 7 8.5-7" />
        </svg>
      );
    case "gcal":
    case "caldav":
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
    default:
      return (
        <svg {...common}>
          <ellipse cx="12" cy="6" rx="7.5" ry="3" />
          <path d="M4.5 6v12c0 1.7 3.4 3 7.5 3s7.5-1.3 7.5-3V6M4.5 12c0 1.7 3.4 3 7.5 3s7.5-1.3 7.5-3" />
        </svg>
      );
  }
}
