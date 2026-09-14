import { AlertTriangle } from "lucide-react";
import {
  Button,
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui";
import { useTranslation } from "@/hooks/useTranslation";
import { cn } from "@/lib/utils";
import type { TranslationKey } from "@/i18n/translations";

export interface ConfirmDialogProps {
  open: boolean;
  title: string;
  message: string;
  /** Extra detail — usually a list of what is about to be removed. */
  children?: React.ReactNode;
  confirmLabel?: string;
  /** Translation key for the confirm label (takes precedence over confirmLabel). */
  confirmLabelKey?: TranslationKey;
  cancelLabel?: string;
  /** Translation key for the cancel label (takes precedence over cancelLabel). */
  cancelLabelKey?: TranslationKey;
  /** Red confirm button plus a warning glyph. */
  destructive?: boolean;
  /** Disables both buttons while the action is in flight. */
  busy?: boolean;
  onConfirm: () => void;
  onClose: () => void;
}

/** Standard two-choice dialog used before anything is deleted. */
export function ConfirmDialog({
  open,
  title,
  message,
  children,
  confirmLabel,
  confirmLabelKey,
  cancelLabel,
  cancelLabelKey,
  destructive = false,
  busy = false,
  onConfirm,
  onClose,
}: ConfirmDialogProps) {
  const { t } = useTranslation();

  const resolvedConfirm = confirmLabelKey ? t(confirmLabelKey) : confirmLabel ?? t("confirm.continue");
  const resolvedCancel = cancelLabelKey ? t(cancelLabelKey) : cancelLabel ?? t("confirm.cancel");

  return (
    <Dialog open={open} onClose={onClose} dismissible={!busy}>
      <DialogHeader className="flex-row items-start gap-3 pr-10">
        {destructive && (
          <span className="mt-0.5 flex size-8 shrink-0 items-center justify-center rounded-full bg-destructive/12 text-destructive">
            <AlertTriangle className="size-4" strokeWidth={2.1} />
          </span>
        )}
        <div className="flex min-w-0 flex-col gap-1">
          <DialogTitle>{title}</DialogTitle>
          <DialogDescription>{message}</DialogDescription>
        </div>
      </DialogHeader>

      {children && (
        <DialogContent className={cn(destructive && "pl-[68px]")}>
          {children}
        </DialogContent>
      )}

      <DialogFooter>
        <Button variant="outline" onClick={onClose} disabled={busy}>
          {resolvedCancel}
        </Button>
        <Button
          variant={destructive ? "destructive" : "default"}
          onClick={onConfirm}
          disabled={busy}
        >
          {busy ? t("confirm.working") : resolvedConfirm}
        </Button>
      </DialogFooter>
    </Dialog>
  );
}
