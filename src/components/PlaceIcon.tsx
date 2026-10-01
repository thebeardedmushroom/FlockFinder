import type { PlaceKind } from "../lib/types";

/** Home (house), Work (briefcase) or a custom place (star; outlined when not saved). */
export default function PlaceIcon({ kind, filled = true, size = 16 }: { kind: PlaceKind; filled?: boolean; size?: number }) {
  const common = { width: size, height: size, viewBox: "0 0 24 24", "aria-hidden": true, className: "place-icon" } as const;
  if (kind === "home") {
    return (
      <svg {...common} fill="none" stroke="currentColor" strokeWidth={2} strokeLinejoin="round">
        <path d="M3 11.5 12 4l9 7.5" strokeLinecap="round" />
        <path d="M5.5 10v10h5v-6h3v6h5V10" />
      </svg>
    );
  }
  if (kind === "work") {
    return (
      <svg {...common} fill="none" stroke="currentColor" strokeWidth={2} strokeLinejoin="round">
        <rect x="3" y="7.5" width="18" height="12.5" rx="2" />
        <path d="M8.5 7.5V5.5a1.5 1.5 0 0 1 1.5-1.5h4a1.5 1.5 0 0 1 1.5 1.5v2M3 13h18" />
      </svg>
    );
  }
  return (
    <svg {...common} fill={filled ? "currentColor" : "none"} stroke="currentColor" strokeWidth={2} strokeLinejoin="round">
      <path d="m12 3.5 2.6 5.3 5.9.9-4.3 4.1 1 5.8L12 16.9l-5.2 2.7 1-5.8-4.3-4.1 5.9-.9z" />
    </svg>
  );
}
