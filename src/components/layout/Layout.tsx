import { useEffect, useState } from "react";
import { ShieldAlert, X } from "lucide-react";
import { Sidebar } from "./Sidebar";
import { WindowControls } from "./WindowControls";
import { NAV_BY_ID } from "@/lib/navigation";
import { useTranslation } from "@/hooks/useTranslation";
import { localizedScanStage } from "@/lib/cleanStage";
import { cn } from "@/lib/utils";
import { selectIsWindows, useAppStore } from "@/stores/appStore";
import { Progress, Button } from "@/components/ui";
import { openFullDiskAccessSettings } from "@/services/dashboard";

export interface LayoutProps {
  /** Optional controls rendered on the right of the content header. */
  actions?: React.ReactNode;
  children: React.ReactNode;
}

export function Layout({ actions, children }: LayoutProps) {
  const activeModule = useAppStore((state) => state.activeModule);
  const status = useAppStore((state) => state.status);
  const progress = useAppStore((state) => state.progress);
  const lastError = useAppStore((state) => state.lastError);
  const permissions = useAppStore((state) => state.permissions);
  const isWindows = useAppStore(selectIsWindows);
  const { t } = useTranslation();

  const [fdaDismissed, setFdaDismissed] = useState(false);
  const [errorDismissed, setErrorDismissed] = useState(false);

  const nav = NAV_BY_ID[activeModule];
  const busy = status === "scanning" || status === "running";
  const progressValue =
    progress?.progress != null ? progress.progress * 100 : null;

  // Reset error dismissal when a new error arrives; log to console.
  useEffect(() => {
    if (lastError) {
      setErrorDismissed(false);
      console.error(
        "[Windle]",
        lastError.code,
        lastError.message,
        lastError.path ?? "",
      );
    }
  }, [lastError]);

  const showFdaBanner = !permissions.fullDiskAccess && !fdaDismissed;
  const showError = lastError && !errorDismissed;

  async function handleOpenSettings() {
    try {
      await openFullDiskAccessSettings();
    } catch (err) {
      console.error("[Windle] Failed to open System Settings:", err);
    }
  }

  return (
    <div className="flex h-full w-full overflow-hidden bg-background">
      <Sidebar />

      <main className="flex min-w-0 flex-1 flex-col">
        {showFdaBanner && (
          <div className="flex shrink-0 items-center justify-between gap-3 border-b border-warning/30 bg-warning/10 px-6 py-2">
            <span className="flex items-center gap-2 text-[12.5px] font-medium text-warning">
              <ShieldAlert className="size-4" />
              {t("permission.fullDiskAccessMessage")}
            </span>
            <div className="flex items-center gap-2">
              <Button
                variant="outline"
                onClick={() => void handleOpenSettings()}
              >
                {t("permission.openSettings")}
              </Button>
              <button
                type="button"
                className="text-muted-foreground transition-colors hover:text-foreground"
                onClick={() => setFdaDismissed(true)}
                aria-label={t("permission.fdaAria")}
              >
                <X className="size-4" />
              </button>
            </div>
          </div>
        )}

        <header
          // `data-tauri-drag-region` is what makes Windows drag (and
          // double-click to maximize); macOS uses the CSS drag utility.
          {...(isWindows ? { "data-tauri-drag-region": "deep" } : {})}
          className={cn(
            "drag-region flex h-[52px] shrink-0 items-center justify-between gap-4 border-b border-border pl-6",
            // The caption buttons sit flush in the top-right corner.
            isWindows ? "pr-0" : "pr-6",
          )}
        >
          <div className="min-w-0">
            <h1 className="truncate text-[15px] font-semibold tracking-tight">
              {t(nav.labelKey)}
            </h1>
            <p className="truncate text-[11.5px] text-muted-foreground">
              {t(nav.descKey)}
            </p>
          </div>

          <div className="flex min-w-0 items-center self-stretch">
            {actions && (
              <div
                className={cn(
                  "no-drag flex items-center gap-2",
                  isWindows && "pr-3",
                )}
              >
                {actions}
              </div>
            )}
            {isWindows && <WindowControls />}
          </div>
        </header>

        {busy && (
          <div className="shrink-0 border-b border-border bg-card/60 px-6 py-2">
            <Progress value={progressValue} />
            {progress && (
              <p className="mt-1.5 truncate font-mono text-[11px] text-muted-foreground">
                {localizedScanStage(t, progress.currentPath)}
              </p>
            )}
          </div>
        )}

        {showError && (
          <div className="flex shrink-0 items-center justify-between gap-3 border-b border-destructive/30 bg-destructive/10 px-6 py-2 text-[12px] text-destructive">
            <span className="truncate">{lastError.message}</span>
            <button
              type="button"
              className="shrink-0 text-destructive transition-colors hover:text-destructive/70"
              onClick={() => setErrorDismissed(true)}
              aria-label={t("permission.errorAria")}
            >
              <X className="size-3.5" />
            </button>
          </div>
        )}

        <div
          className={cn(
            "flex-1 overflow-y-auto px-6 py-5",
            // Give scroll content a little breathing room at the bottom.
            "[&>*:last-child]:mb-2",
          )}
        >
          {children}
        </div>
      </main>
    </div>
  );
}
