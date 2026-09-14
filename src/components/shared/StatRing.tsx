import { cn } from "@/lib/utils";

export interface StatRingProps {
  /** 0–100. */
  value: number;
  label?: string;
  /** Small line under the percentage, e.g. "12 GB of 16 GB". */
  caption?: string;
  /** Any CSS colour — drives the arc and the glow. */
  color?: string;
  /** Outer diameter in pixels. */
  size?: number;
  thickness?: number;
  className?: string;
}

/**
 * Donut gauge. The arc is a dash-offset animation on a rotated circle, so it
 * grows smoothly whenever `value` changes without any layout work.
 */
export function StatRing({
  value,
  label,
  caption,
  color = "hsl(var(--primary))",
  size = 132,
  thickness = 10,
  className,
}: StatRingProps) {
  const clamped = Math.min(100, Math.max(0, value));
  const radius = (size - thickness) / 2;
  const circumference = 2 * Math.PI * radius;
  const offset = circumference * (1 - clamped / 100);

  return (
    <div className={cn("flex flex-col items-center gap-2.5", className)}>
      <div className="relative" style={{ width: size, height: size }}>
        <svg
          width={size}
          height={size}
          viewBox={`0 0 ${size} ${size}`}
          className="-rotate-90"
          aria-hidden
        >
          <circle
            cx={size / 2}
            cy={size / 2}
            r={radius}
            fill="none"
            stroke="hsl(var(--muted))"
            strokeWidth={thickness}
          />
          <circle
            cx={size / 2}
            cy={size / 2}
            r={radius}
            fill="none"
            stroke={color}
            strokeWidth={thickness}
            strokeLinecap="round"
            strokeDasharray={circumference}
            strokeDashoffset={offset}
            className="transition-[stroke-dashoffset] duration-700 ease-out"
            style={{ filter: `drop-shadow(0 0 6px ${color}40)` }}
          />
        </svg>

        <div className="absolute inset-0 flex flex-col items-center justify-center">
          <span
            className="text-2xl font-semibold tracking-tight tabular-nums"
            style={{ color }}
          >
            {Math.round(clamped)}
            <span className="ml-0.5 text-sm font-medium opacity-70">%</span>
          </span>
        </div>
      </div>

      <div className="flex flex-col items-center gap-0.5 text-center">
        {label && <span className="text-[13px] font-medium">{label}</span>}
        {caption && (
          <span className="text-[11.5px] text-muted-foreground tabular-nums">
            {caption}
          </span>
        )}
      </div>
    </div>
  );
}
