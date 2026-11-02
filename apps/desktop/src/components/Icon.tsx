/** Line icons on a 24-px grid (1.6 stroke, round joins), drawn in currentColor. Decorative only. */
const PATHS = {
  home: "M4 11.5 12 5l8 6.5V19a1 1 0 0 1-1 1h-4.5v-5h-5v5H5a1 1 0 0 1-1-1Z",
  inbox: "M4 13.5 6.5 5.5h11l2.5 8V19a1 1 0 0 1-1 1H5a1 1 0 0 1-1-1ZM4 13.5h4.5l1 2h5l1-2H20",
  search: "M11 4.5a6.5 6.5 0 1 1 0 13 6.5 6.5 0 0 1 0-13ZM16 16l4 4",
  plug: "M9 4v4M15 4v4M6.5 8h11v3a5.5 5.5 0 0 1-11 0ZM12 16.5V20",
  chat: "M4 6.5A2.5 2.5 0 0 1 6.5 4h11A2.5 2.5 0 0 1 20 6.5v8a2.5 2.5 0 0 1-2.5 2.5H10l-4.5 3.5V17A2.5 2.5 0 0 1 4 14.5Z",
  plus: "M12 5v14M5 12h14",
  back: "M14.5 6 8.5 12l6 6",
  up: "M12 19V6M6.5 11.5 12 6l5.5 5.5",
  settings: "M4 7h10M18 7h2M4 17h4M12 17h8M16 5v4M10 15v4",
  spark: "M12 4c.6 4.2 3.8 7.4 8 8-4.2.6-7.4 3.8-8 8-.6-4.2-3.8-7.4-8-8 4.2-.6 7.4-3.8 8-8Z",
} as const;

export type IconName = keyof typeof PATHS;

export function Icon({ name, size = 16 }: { name: IconName; size?: number }) {
  return (
    <svg className="icon" width={size} height={size} viewBox="0 0 24 24" aria-hidden="true" focusable="false">
      <path d={PATHS[name]} />
    </svg>
  );
}
