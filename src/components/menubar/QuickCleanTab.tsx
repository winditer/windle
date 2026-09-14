import { useEffect, useState, useCallback } from "react";
import { Brain, Loader2, Trash2, X } from "lucide-react";
import { useTranslation } from "@/hooks/useTranslation";
import { getDashboardSummary } from "@/services/dashboard";
import { scanJunk, cleanPaths } from "@/services/clean";
import { listTopMemoryProcesses } from "@/services/menubar";
import { getSnapshot, killProcess } from "@/services/monitor";
import { runOptimizeTask } from "@/services/optimize";
import type { CleanCategoryId, ProcessInfo, SystemSnapshot } from "@/types";
import { cn, formatBytes } from "@/lib/utils";

/**
 * Only the safe, fast-to-measure categories are scanned from the menubar
 * popup so the user gets a responsive experience without waiting on the
 * slower locations (language files, iOS backups, etc.). The one-tap clean
 * then picks only `safe`-risk items (see `handleCleanJunk`), so a wider
 * scan scope never widens what gets permanently deleted.
 */
const QUICK_CLEAN_CATEGORIES: CleanCategoryId[] = [
  "user-cache",
  "app-logs",
  "browser-cache",
  "trash",
];

interface QuickCleanTabProps {
  snapshot: SystemSnapshot | null;
}

export function QuickCleanTab({ snapshot }: QuickCleanTabProps) {
  const { t } = useTranslation();

  // -- System cleanup state --
  const [junkSize, setJunkSize] = useState(0);
  const [cleaningJunk, setCleaningJunk] = useState(false);
  const [junkFreed, setJunkFreed] = useState<number | null>(null);
  const [junkError, setJunkError] = useState(false);

  // -- Memory release state --
  const [releasingMemory, setReleasingMemory] = useState(false);
  const [memoryFreed, setMemoryFreed] = useState<number | null>(null);
  const [memoryError, setMemoryError] = useState(false);

  // -- Process list state --
  const [processes, setProcesses] = useState<ProcessInfo[]>([]);

  const memory = snapshot?.memory;
  const memUsed = memory?.usedBytes ?? 0;
  const memTotal = memory?.totalBytes ?? 0;
  const memPercent = memTotal > 0 ? (memUsed / memTotal) * 100 : 0;

  // Poll junk size from the dashboard summary every 30s.
  useEffect(() => {
    let cancelled = false;
    const fetchJunk = () => {
      getDashboardSummary()
        .then((s) => {
          if (!cancelled) setJunkSize(s.junkSize);
        })
        .catch(() => {});
    };
    fetchJunk();
    const id = setInterval(fetchJunk, 30000);
    return () => {
      cancelled = true;
      clearInterval(id);
    };
  }, []);

  // Poll top memory processes every 3s.
  const fetchProcesses = useCallback(() => {
    listTopMemoryProcesses(6)
      .then(setProcesses)
      .catch(() => {});
  }, []);

  useEffect(() => {
    fetchProcesses();
    const id = setInterval(fetchProcesses, 3000);
    return () => clearInterval(id);
  }, [fetchProcesses]);

  // Auto-clear junk freed message after 4 seconds.
  useEffect(() => {
    if (junkFreed === null) return;
    const id = setTimeout(() => setJunkFreed(null), 4000);
    return () => clearTimeout(id);
  }, [junkFreed]);

  // Auto-clear memory freed message after 4 seconds.
  useEffect(() => {
    if (memoryFreed === null) return;
    const id = setTimeout(() => setMemoryFreed(null), 4000);
    return () => clearTimeout(id);
  }, [memoryFreed]);

  const handleCleanJunk = async () => {
    setCleaningJunk(true);
    setJunkError(false);
    setJunkFreed(null);
    try {
      const scanResult = await scanJunk(QUICK_CLEAN_CATEGORIES);
      // The menubar entry deletes permanently with no confirmation dialog,
      // so only safe-risk items are picked up: sandboxed app logs (a newer
      // app-logs source) and Caution-level system entries are left for the
      // full Deep Clean view where the user can review them.
      const paths = scanResult.categories.flatMap((category) =>
        category.items
          .filter(
            (item) => item.risk === "safe" && item.group?.id !== "sandbox-logs",
          )
          .map((item) => item.path),
      );
      if (paths.length === 0) {
        setJunkFreed(0);
        return;
      }
      // Permanently delete junk files instead of moving them to ~/.Trash.
      // Moving to Trash creates a circular dependency: quick_junk_estimate()
      // counts ~/.Trash as junk, so files just move Caches/Logs → Trash and
      // the total junk size stays the same.
      const outcome = await cleanPaths(paths, true);
      setJunkFreed(outcome.freedBytes);
      // Refresh junk size after cleaning. The backend invalidated its junk
      // cache inside clean_paths, so this re-scan returns the true post-clean
      // value immediately rather than the stale 60 s TTL cache.
      try {
        const summary = await getDashboardSummary();
        setJunkSize(summary.junkSize);
      } catch {
        // Non-fatal: the 30 s poll will catch up eventually.
      }
    } catch {
      setJunkError(true);
    } finally {
      setCleaningJunk(false);
    }
  };

  const handleReleaseMemory = async () => {
    setReleasingMemory(true);
    setMemoryError(false);
    setMemoryFreed(null);
    try {
      const before = await getSnapshot();
      const beforeMem = before.memory?.usedBytes ?? 0;
      const outcome = await runOptimizeTask("purge-memory");
      if (outcome.succeeded) {
        const after = await getSnapshot();
        const afterMem = after.memory?.usedBytes ?? 0;
        const freed = Math.max(0, beforeMem - afterMem);
        setMemoryFreed(freed);
      } else {
        setMemoryError(true);
      }
    } catch {
      setMemoryError(true);
    } finally {
      setReleasingMemory(false);
    }
  };

  const handleKill = async (pid: number) => {
    try {
      await killProcess(pid, true);
      setProcesses((prev) => prev.filter((p) => p.pid !== pid));
    } catch {
      // Ignore kill errors silently in the popup.
    }
  };

  return (
    <div className="flex flex-col">
      {/* ================================================================ */}
      {/* System Cleanup Section                                           */}
      {/* ================================================================ */}
      <div className="flex flex-col gap-2 px-3 py-3">
        {/* Section header */}
        <div className="flex items-center gap-1.5">
          <Trash2 className="size-3.5 text-muted-foreground" strokeWidth={2} />
          <span className="text-[11px] font-semibold tracking-wide text-muted-foreground uppercase">
            {t("menubar.systemCleanup")}
          </span>
        </div>

        {/* Junk size + clean button */}
        <div className="flex items-center justify-between rounded-lg bg-muted/50 px-2.5 py-2">
          <div className="flex flex-col">
            <span className="text-[10px] text-muted-foreground">
              {t("menubar.junkSize")}
            </span>
            <span className="text-[18px] font-bold leading-tight tabular-nums">
              {formatBytes(junkSize)}
            </span>
          </div>

          <button
            type="button"
            onClick={handleCleanJunk}
            disabled={cleaningJunk}
            className={cn(
              "flex items-center gap-1.5 rounded-md px-3 py-1.5 text-[12px] font-medium transition-all duration-150",
              "bg-primary text-primary-foreground shadow-sm hover:bg-primary/90",
              "disabled:opacity-50 active:scale-[0.98]",
            )}
          >
            {cleaningJunk ? (
              <Loader2 className="size-3.5 animate-spin" />
            ) : (
              <Trash2 className="size-3.5" />
            )}
            {cleaningJunk ? t("menubar.cleaning") : t("menubar.clean")}
          </button>
        </div>

        {/* Status line */}
        {junkFreed !== null && (
          <p className="text-center text-[11px] font-medium text-primary">
            {t("menubar.freed", { bytes: formatBytes(junkFreed) })}
          </p>
        )}
        {junkError && (
          <p className="text-center text-[11px] font-medium text-destructive">
            {t("menubar.cleanFailed")}
          </p>
        )}
      </div>

      {/* ================================================================ */}
      {/* Divider                                                          */}
      {/* ================================================================ */}
      <div className="h-px bg-border/60" />

      {/* ================================================================ */}
      {/* Memory & Processes Section                                       */}
      {/* ================================================================ */}
      <div className="flex flex-col gap-2 px-3 py-3">
        {/* Section header */}
        <div className="flex items-center gap-1.5">
          <Brain className="size-3.5 text-muted-foreground" strokeWidth={2} />
          <span className="text-[11px] font-semibold tracking-wide text-muted-foreground uppercase">
            {t("menubar.memoryAndProcesses")}
          </span>
        </div>

        {/* Memory usage card */}
        <div className="rounded-lg bg-muted/50 px-2.5 py-2">
          <div className="flex items-center justify-between">
            <span className="text-[10px] text-muted-foreground">
              {t("menubar.memoryUsage")}
            </span>
            <span className="text-[12px] font-semibold tabular-nums">
              {Math.round(memPercent)}%
            </span>
          </div>

          <div className="mt-0.5 flex items-baseline gap-1.5">
            <span className="text-[18px] font-bold leading-none tabular-nums">
              {formatBytes(memUsed)}
            </span>
            <span className="text-[11px] text-muted-foreground tabular-nums">
              / {formatBytes(memTotal)}
            </span>
          </div>

          {/* Memory bar */}
          <div className="mt-1.5 h-1.5 w-full overflow-hidden rounded-full bg-muted">
            <div
              className={cn(
                "h-full rounded-full transition-all duration-300",
                memPercent > 80 ? "bg-destructive" : "bg-primary",
              )}
              style={{ width: `${Math.min(100, memPercent)}%` }}
            />
          </div>

          {/* Release memory button */}
          <button
            type="button"
            onClick={handleReleaseMemory}
            disabled={releasingMemory}
            className={cn(
              "mt-2 flex w-full items-center justify-center gap-1.5 rounded-md py-1.5 text-[12px] font-medium transition-all duration-150",
              "bg-primary text-primary-foreground shadow-sm hover:bg-primary/90",
              "disabled:opacity-50 active:scale-[0.98]",
            )}
          >
            {releasingMemory ? (
              <Loader2 className="size-3.5 animate-spin" />
            ) : (
              <Brain className="size-3.5" />
            )}
            {releasingMemory
              ? t("menubar.releasing")
              : t("menubar.releaseMemory")}
          </button>

          {/* Status line */}
          {memoryFreed !== null && (
            <p className="mt-1.5 text-center text-[11px] font-medium text-primary">
              {t("menubar.freed", { bytes: formatBytes(memoryFreed) })}
            </p>
          )}
          {memoryError && (
            <p className="mt-1.5 text-center text-[11px] font-medium text-destructive">
              {t("menubar.cleanFailed")}
            </p>
          )}
        </div>

        {/* Process ranking */}
        <div>
          <p className="mb-1 px-0.5 text-[10px] font-medium tracking-wide text-muted-foreground uppercase">
            {t("menubar.processRanking")}
          </p>

          {processes.length > 0 ? (
            <div className="flex flex-col">
              {processes.map((proc, i) => (
                <div
                  key={proc.pid}
                  className={cn(
                    "group flex items-center gap-2 rounded-md px-1.5 py-1",
                    "hover:bg-accent/60 transition-colors",
                    i > 0 && "mt-0.5",
                  )}
                >
                  <div className="min-w-0 flex-1">
                    <p className="truncate text-[12px] font-medium leading-tight">
                      {proc.name}
                    </p>
                    <p className="font-mono text-[10px] leading-tight text-muted-foreground">
                      pid {proc.pid}
                    </p>
                  </div>

                  <span className="text-[11.5px] font-semibold tabular-nums">
                    {formatBytes(proc.memoryBytes)}
                  </span>

                  <button
                    type="button"
                    onClick={() => handleKill(proc.pid)}
                    className={cn(
                      "flex size-5 items-center justify-center rounded opacity-0",
                      "transition-all duration-100 hover:bg-destructive hover:text-destructive-foreground",
                      "group-hover:opacity-100",
                    )}
                    title={t("menubar.killProcess")}
                  >
                    <X className="size-3" strokeWidth={2.5} />
                  </button>
                </div>
              ))}
            </div>
          ) : (
            <p className="py-3 text-center text-[12px] text-muted-foreground">
              {t("menubar.noProcesses")}
            </p>
          )}
        </div>
      </div>
    </div>
  );
}
