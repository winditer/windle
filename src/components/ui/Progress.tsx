import { forwardRef } from "react";
import { cn } from "@/lib/utils";

export interface ProgressProps
  extends Omit<React.HTMLAttributes<HTMLDivElement>, "children"> {
  /** 0–100. Pass `null` for an indeterminate bar. */
  value?: number | null;
  /** Tailwind class for the filled portion, e.g. `bg-warning`. */
  indicatorClassName?: string;
}

const Progress = forwardRef<HTMLDivElement, ProgressProps>(
  ({ className, value = 0, indicatorClassName, ...props }, ref) => {
    const indeterminate = value === null;
    const clamped = indeterminate ? 0 : Math.min(100, Math.max(0, value ?? 0));

    return (
      <div
        ref={ref}
        role="progressbar"
        aria-valuemin={0}
        aria-valuemax={100}
        aria-valuenow={indeterminate ? undefined : clamped}
        className={cn(
          "relative h-1.5 w-full overflow-hidden rounded-full bg-muted",
          className,
        )}
        {...props}
      >
        <div
          className={cn(
            "h-full rounded-full bg-primary transition-[width] duration-300 ease-out",
            indeterminate && "w-1/3 animate-pulse",
            indicatorClassName,
          )}
          style={indeterminate ? undefined : { width: `${clamped}%` }}
        />
      </div>
    );
  },
);
Progress.displayName = "Progress";

export { Progress };
