import type { LucideIcon } from "lucide-react";
import { Button } from "@/components/ui";
import { cn } from "@/lib/utils";

export interface EmptyStateProps {
  icon: LucideIcon;
  title: string;
  message?: string;
  actionLabel?: string;
  onAction?: () => void;
  /** Rendered instead of the default action button when provided. */
  action?: React.ReactNode;
  className?: string;
}

/** Shown before a module has scanned anything, or when a filter matches nothing. */
export function EmptyState({
  icon: Icon,
  title,
  message,
  actionLabel,
  onAction,
  action,
  className,
}: EmptyStateProps) {
  return (
    <div
      className={cn(
        "flex flex-col items-center justify-center gap-3 px-8 py-14 text-center",
        className,
      )}
    >
      <span className="flex size-12 items-center justify-center rounded-2xl bg-primary/10 text-primary">
        <Icon className="size-5.5" strokeWidth={1.8} />
      </span>

      <div className="flex max-w-sm flex-col gap-1">
        <h3 className="text-[14px] font-semibold tracking-tight">{title}</h3>
        {message && (
          <p className="text-[12.5px] leading-relaxed text-muted-foreground">
            {message}
          </p>
        )}
      </div>

      {action ??
        (actionLabel && onAction && (
          <Button className="mt-1" onClick={onAction}>
            {actionLabel}
          </Button>
        ))}
    </div>
  );
}
