import icons from "../lib/maneuverIcons.json";

type Icon = { strokes?: string[]; faint?: string[]; dots?: number[][] };
const ICONS = icons as unknown as Record<string, Icon>;

/** A maneuver arrow (the same drawings as the Android notification's). */
export default function ManeuverIcon({ name, size = 48 }: { name: string; size?: number }) {
  const icon = ICONS[name] ?? ICONS.straight;
  return (
    <svg
      width={size}
      height={size}
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth={2.2}
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden="true"
    >
      {icon.faint?.map((d, i) => <path key={`f${i}`} d={d} opacity={0.4} />)}
      {icon.strokes?.map((d, i) => <path key={i} d={d} />)}
      {icon.dots?.map(([cx, cy, r], i) => <circle key={`d${i}`} cx={cx} cy={cy} r={r} fill="currentColor" stroke="none" />)}
    </svg>
  );
}
