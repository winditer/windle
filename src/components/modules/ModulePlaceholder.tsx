import { NAV_BY_ID } from "@/lib/navigation";
import { Card, CardContent } from "@/components/ui";
import { useTranslation } from "@/hooks/useTranslation";
import type { ModuleId } from "@/types";

export interface ModulePlaceholderProps {
  module: ModuleId;
  /** Bullet points describing what this module will do. */
  features?: string[];
}

/**
 * Stand-in shown until a feature module is implemented. Each module replaces
 * this with its own view.
 */
export function ModulePlaceholder({
  module,
  features = [],
}: ModulePlaceholderProps) {
  const { t } = useTranslation();
  const { labelKey, descKey, icon: Icon } = NAV_BY_ID[module];

  return (
    <Card className="mx-auto max-w-lg">
      <CardContent className="flex flex-col items-center gap-3 px-8 py-10 text-center">
        <span className="flex size-11 items-center justify-center rounded-xl bg-primary/10 text-primary">
          <Icon className="size-5" strokeWidth={1.9} />
        </span>

        <div className="flex flex-col gap-1">
          <h2 className="text-[15px] font-semibold tracking-tight">{t(labelKey)}</h2>
          <p className="text-[13px] text-muted-foreground">{t(descKey)}</p>
        </div>

        {features.length > 0 && (
          <ul className="mt-2 flex w-full flex-col gap-1.5 text-left">
            {features.map((feature) => (
              <li
                key={feature}
                className="flex items-start gap-2 rounded-lg bg-muted/60 px-3 py-2 text-[12.5px] text-muted-foreground"
              >
                <span className="mt-[7px] size-1.5 shrink-0 rounded-full bg-primary/60" />
                {feature}
              </li>
            ))}
          </ul>
        )}
      </CardContent>
    </Card>
  );
}
