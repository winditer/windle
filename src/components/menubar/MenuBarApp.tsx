import { useState, useEffect, useCallback } from "react";
import { getCurrentWebviewWindow } from "@tauri-apps/api/webviewWindow";
import { ChevronDown, ExternalLink, Sparkles, Activity } from "lucide-react";
import { useTranslation } from "@/hooks/useTranslation";
import {
  getSnapshot,
  startMonitor,
  stopMonitor,
  onSnapshot,
} from "@/services/monitor";
import { quitStatusBar, showMainWindow } from "@/services/menubar";
import { QuickCleanTab } from "./QuickCleanTab";
import { SystemStatusTab } from "./SystemStatusTab";
import type { MonitorSample } from "./NetworkWaveform";
import type { SystemSnapshot } from "@/types";
import { cn } from "@/lib/utils";

const HISTORY_SIZE = 30;

type TabId = "quickClean" | "systemStatus";

/** Convert a SystemSnapshot into a chart sample with raw bytes/sec. */
function snapshotToSample(snapshot: SystemSnapshot): MonitorSample {
  const net = snapshot.network?.[0];
  return {
    time: Date.now(),
    netDown: net?.rxBytesPerSec ?? 0,
    netUp: net?.txBytesPerSec ?? 0,
  };
}

export function MenuBarApp() {
  const { t } = useTranslation();
  const [tab, setTab] = useState<TabId>("quickClean");
  const [snapshot, setSnapshot] = useState<SystemSnapshot | null>(null);
  const [history, setHistory] = useState<MonitorSample[]>([]);
  const [menuOpen, setMenuOpen] = useState(false);

  // -- Monitor lifecycle: getSnapshot → startMonitor → onSnapshot → stopMonitor
  useEffect(() => {
    let cancelled = false;
    let unlisten: (() => void) | null = null;
    let unlistenFocus: (() => void) | null = null;
    let hideTimer: number | null = null;

    // Grab one snapshot immediately so the popup is not empty on open.
    getSnapshot()
      .then((snap) => {
        if (cancelled) return;
        setSnapshot(snap);
        const sample = snapshotToSample(snap);
        setHistory([sample]);
      })
      .catch(() => {});

    // Start the streaming backend at 2-second intervals.
    startMonitor(2000).catch(() => {});

    // Subscribe to snapshot events.
    onSnapshot((snap) => {
      if (cancelled) return;
      setSnapshot(snap);
      const sample = snapshotToSample(snap);
      setHistory((prev) => {
        const next = [...prev, sample];
        if (next.length > HISTORY_SIZE) next.shift();
        return next;
      });
    }).then((fn) => {
      if (cancelled) {
        fn();
      } else {
        unlisten = fn;
      }
    });

    // Window blur → hide with 100 ms debounce to avoid flicker.
    const win = getCurrentWebviewWindow();
    win
      .onFocusChanged(({ payload: focused }) => {
        if (!focused) {
          hideTimer = window.setTimeout(() => win.hide(), 100);
        } else if (hideTimer) {
          clearTimeout(hideTimer);
          hideTimer = null;
        }
      })
      .then((fn) => {
        if (cancelled) {
          fn();
        } else {
          unlistenFocus = fn;
        }
      });

    return () => {
      cancelled = true;
      stopMonitor().catch(() => {});
      if (unlisten) unlisten();
      if (unlistenFocus) unlistenFocus();
      if (hideTimer) clearTimeout(hideTimer);
    };
  }, []);

  const handleQuit = useCallback(() => {
    setMenuOpen(false);
    quitStatusBar().catch(() => {});
  }, []);

  const handleOpenWindle = useCallback(() => {
    showMainWindow().catch(() => {});
  }, []);

  return (
    <div
      className={cn(
        "flex h-screen w-full flex-col overflow-hidden",
        "rounded-xl border border-border/60 bg-card/95 backdrop-blur-xl",
        "shadow-[0_8px_32px_rgb(0_0_0/0.18)]",
      )}
    >
      {/* ----------------------------- Tab switcher ------------------------- */}
      <div className="no-drag flex items-center gap-1 border-b border-border/60 p-2">
        <div className="flex flex-1 items-center gap-0.5 rounded-lg bg-muted p-0.5">
          <button
            type="button"
            onClick={() => setTab("quickClean")}
            className={cn(
              "flex flex-1 items-center justify-center gap-1.5 rounded-md py-1 text-[12px] font-medium transition-all duration-150",
              tab === "quickClean"
                ? "bg-card text-foreground shadow-sm"
                : "text-muted-foreground hover:text-foreground",
            )}
          >
            <Sparkles className="size-3.5" />
            {t("menubar.quickClean")}
          </button>
          <button
            type="button"
            onClick={() => setTab("systemStatus")}
            className={cn(
              "flex flex-1 items-center justify-center gap-1.5 rounded-md py-1 text-[12px] font-medium transition-all duration-150",
              tab === "systemStatus"
                ? "bg-card text-foreground shadow-sm"
                : "text-muted-foreground hover:text-foreground",
            )}
          >
            <Activity className="size-3.5" />
            {t("menubar.systemStatus")}
          </button>
        </div>
      </div>

      {/* ------------------------------ Tab content ------------------------- */}
      <div className="min-h-0 flex-1 overflow-y-auto">
        {tab === "quickClean" ? (
          <QuickCleanTab snapshot={snapshot} />
        ) : (
          <SystemStatusTab snapshot={snapshot} history={history} />
        )}
      </div>

      {/* ------------------------------ Bottom bar -------------------------- */}
      <div className="no-drag relative flex items-center justify-between border-t border-border/60 px-2 py-1.5">
        {/* Quit dropdown */}
        <div className="relative">
          <button
            type="button"
            onClick={() => setMenuOpen((v) => !v)}
            className={cn(
              "flex items-center gap-1 rounded-md px-2 py-1 text-[11px] font-medium transition-colors",
              "text-muted-foreground hover:bg-accent hover:text-foreground",
            )}
          >
            <ChevronDown className="size-3.5" />
          </button>

          {menuOpen && (
            <>
              {/* Click-away overlay */}
              <div
                className="fixed inset-0 z-10"
                onClick={() => setMenuOpen(false)}
              />
              <div
                className={cn(
                  "absolute bottom-full left-0 z-20 mb-1 min-w-[140px]",
                  "rounded-lg border border-border bg-popover p-1 shadow-lg",
                  "animate-scale-in origin-bottom",
                )}
              >
                <button
                  type="button"
                  onClick={handleQuit}
                  className={cn(
                    "flex w-full items-center rounded-md px-2.5 py-1.5 text-[12px] font-medium transition-colors",
                    "text-foreground hover:bg-accent",
                  )}
                >
                  {t("menubar.quitStatusBar")}
                </button>
              </div>
            </>
          )}
        </div>

        {/* Open Windle */}
        <button
          type="button"
          onClick={handleOpenWindle}
          className={cn(
            "flex items-center gap-1.5 rounded-md px-2 py-1 text-[11px] font-medium transition-colors",
            "text-muted-foreground hover:bg-accent hover:text-foreground",
          )}
        >
          <ExternalLink className="size-3.5" />
          {t("menubar.openWindle")}
        </button>
      </div>
    </div>
  );
}
