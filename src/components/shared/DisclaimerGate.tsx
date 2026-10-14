import { useState } from "react";
import { ShieldAlert } from "lucide-react";
import {
  Button,
  Dialog,
  DialogContent,
  DialogFooter,
  DialogHeader,
  DialogTitle,
  DialogDescription,
} from "@/components/ui";
import { useTranslation } from "@/hooks/useTranslation";
import { quitApp } from "@/services/platform";

/** Where the accepted version is remembered. */
const STORAGE_KEY = "windle-disclaimer-accepted";
/** Bump when the terms change so every user sees them again. */
const DISCLAIMER_VERSION = "1";

function hasAccepted(): boolean {
  try {
    return localStorage.getItem(STORAGE_KEY) === DISCLAIMER_VERSION;
  } catch {
    return false;
  }
}

/**
 * First-launch gate: an undismissable dialog asking the user to accept the
 * disclaimer (DISCLAIMER.md) before using the app. Declining quits, so no
 * cleanup action can run without an explicit agreement on record.
 */
export function DisclaimerGate() {
  const { t } = useTranslation();
  const [accepted, setAccepted] = useState(hasAccepted);

  if (accepted) return null;

  function accept() {
    try {
      localStorage.setItem(STORAGE_KEY, DISCLAIMER_VERSION);
    } catch {
      // Storage unavailable — the gate reappears on the next launch.
    }
    setAccepted(true);
  }

  function decline() {
    void quitApp().catch(() => {});
  }

  const points = [
    t("disclaimer.point1"),
    t("disclaimer.point2"),
    t("disclaimer.point3"),
    t("disclaimer.point4"),
  ];

  return (
    <Dialog open onClose={() => {}} dismissible={false} className="max-w-lg">
      <DialogHeader className="flex-row items-start gap-3 pr-10">
        <span className="mt-0.5 flex size-8 shrink-0 items-center justify-center rounded-full bg-warning/12 text-warning">
          <ShieldAlert className="size-4" strokeWidth={2.1} />
        </span>
        <div className="flex min-w-0 flex-col gap-1">
          <DialogTitle>{t("disclaimer.title")}</DialogTitle>
          <DialogDescription>{t("disclaimer.intro")}</DialogDescription>
        </div>
      </DialogHeader>

      <DialogContent className="max-h-[46vh] overflow-y-auto pl-[68px]">
        <ul className="flex flex-col gap-2">
          {points.map((point) => (
            <li
              key={point}
              className="flex gap-2 text-[13px] leading-relaxed text-muted-foreground"
            >
              <span className="mt-[7px] size-1 shrink-0 rounded-full bg-muted-foreground/60" />
              <span>{point}</span>
            </li>
          ))}
        </ul>
        <p className="mt-3 text-[12px] leading-relaxed text-muted-foreground/80">
          {t("disclaimer.more")}
        </p>
      </DialogContent>

      <DialogFooter>
        <Button variant="outline" onClick={decline}>
          {t("disclaimer.decline")}
        </Button>
        <Button onClick={accept}>{t("disclaimer.accept")}</Button>
      </DialogFooter>
    </Dialog>
  );
}
