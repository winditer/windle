import { cva, type VariantProps } from "class-variance-authority";
import { useTranslation } from "@/hooks/useTranslation";
import type { TranslationKey } from "@/i18n/translations";
import { cn } from "@/lib/utils";
import type { RiskLevel } from "@/types";

const badgeVariants = cva(
  "inline-flex items-center gap-1 rounded-full border px-2 py-0.5 text-[10.5px] font-semibold tracking-wide whitespace-nowrap uppercase",
  {
    variants: {
      tone: {
        neutral: "border-border bg-muted text-muted-foreground",
        primary: "border-primary/25 bg-primary/12 text-primary",
        success: "border-success/25 bg-success/12 text-success",
        warning: "border-warning/30 bg-warning/12 text-warning",
        danger: "border-destructive/25 bg-destructive/12 text-destructive",
      },
    },
    defaultVariants: { tone: "neutral" },
  },
);

export interface BadgeProps
  extends React.HTMLAttributes<HTMLSpanElement>,
    VariantProps<typeof badgeVariants> {}

export function Badge({ className, tone, ...props }: BadgeProps) {
  return <span className={cn(badgeVariants({ tone }), className)} {...props} />;
}

const RISK_TONE: Record<RiskLevel, NonNullable<BadgeProps["tone"]>> = {
  safe: "success",
  caution: "warning",
  danger: "danger",
};

const RISK_LABEL_KEYS: Record<RiskLevel, TranslationKey> = {
  safe: "common.risk.safe",
  caution: "common.risk.caution",
  danger: "common.risk.danger",
};

/** English fallback when a translation key is missing. */
const RISK_LABEL: Record<RiskLevel, string> = {
  safe: "Safe",
  caution: "Caution",
  danger: "Careful",
};

/** Badge pre-wired to the shared risk vocabulary. */
export function RiskBadge({ risk, className }: { risk: RiskLevel; className?: string }) {
  const { t } = useTranslation();
  const key = RISK_LABEL_KEYS[risk];
  const localized = t(key);
  // `t()` returns the key itself when a translation is missing.
  const label = localized === key ? RISK_LABEL[risk] : localized;
  return (
    <Badge tone={RISK_TONE[risk]} className={className}>
      {label}
    </Badge>
  );
}

export { badgeVariants };
