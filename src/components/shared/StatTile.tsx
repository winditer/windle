import type { LucideIcon } from "lucide-react";
import { cn } from "@/lib/utils";

export interface StatTileProps {
  icon?: LucideIcon;
  label: string;
  value: string;
  hint?: string;
  /** CSS colour for the icon chip and value. Defaults to the foreground. */
  accent?: string;
  className?: string;
}

/** Compact "big number" tile used in the header row of most modules. */
export function StatTile({
  icon: Icon,
  label,
  value,
  hint,
  accent,
  className,
}: StatTileProps) {
  return (
    <div
      className={cn(
        "flex items-center gap-3 rounded-xl border border-border bg-card/70 px-4 py-3 backdrop-blur-xl transition-all duration-200",
        className,
      )}
    >
      {Icon && (
        <span
          className="flex size-9 shrink-0 items-center justify-center rounded-lg"
          style={{
            backgroundColor: accent ? `${accent}1F` : "hsl(var(--muted))",
            color: accent ?? "hsl(var(--foreground))",
          }}
        >
          <Icon className="size-4.5" strokeWidth={1.9} />
        </span>
      )}

      <div className="min-w-0">
        <p className="text-[11px] font-medium tracking-wide text-muted-foreground uppercase">
          {label}
        </p>
        <p
          className="truncate text-[17px] leading-tight font-semibold tabular-nums"
          style={accent ? { color: accent } : undefined}
        >
          {value}
        </p>
        {hint && (
          <p className="truncate text-[11.5px] text-muted-foreground">{hint}</p>
        )}
      </div>
    </div>
  );
}
