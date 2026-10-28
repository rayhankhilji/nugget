export function Logo({ size = 22 }: { size?: number }) {
  return (
    <svg width={size} height={size} viewBox="0 0 64 64" aria-hidden="true" focusable="false">
      <path
        d="M32 6c12 0 24 9 26 23 2 15-9 29-26 29S4 44 6 29C8 15 20 6 32 6Z"
        fill="var(--accent)"
      />
      <path
        d="M22 24c3-5 8-7 13-6"
        fill="none"
        stroke="var(--accent-contrast)"
        strokeWidth="4"
        strokeLinecap="round"
        opacity="0.75"
      />
    </svg>
  );
}
