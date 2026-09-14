import { Check, Minus } from "lucide-react";
import { cn } from "@/lib/utils";

export interface CheckboxProps {
  checked: boolean;
  /** Renders the dash glyph — use for partially selected groups. */
  indeterminate?: boolean;
  onChange: (checked: boolean) => void;
  disabled?: boolean;
  label?: string;
  className?: string;
}

/** macOS-style checkbox. A button rather than an input so it can sit anywhere. */
export function Checkbox({
  checked,
  indeterminate = false,
  onChange,
  disabled = false,
  label,
  className,
}: CheckboxProps) {
  const active = checked || indeterminate;

  return (
    <button
      type="button"
      role="checkbox"
      aria-checked={indeterminate ? "mixed" : checked}
      aria-label={label}
      disabled={disabled}
      onClick={(event) => {
        event.stopPropagation();
        onChange(!checked);
      }}
      className={cn(
        "flex size-[18px] shrink-0 items-center justify-center rounded-[5px] border transition-all duration-150 outline-none focus-visible:ring-2 focus-visible:ring-ring/60",
        active
          ? "border-primary bg-primary text-primary-foreground shadow-sm"
          : "border-border bg-card hover:border-primary/60",
        disabled && "pointer-events-none opacity-40",
        className,
      )}
    >
      {indeterminate ? (
        <Minus className="size-3" strokeWidth={3} />
      ) : (
        checked && <Check className="size-3" strokeWidth={3.2} />
      )}
    </button>
  );
}
