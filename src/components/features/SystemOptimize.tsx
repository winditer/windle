import { useEffect, useMemo, useState } from "react";
import {
  AppWindow,
  CalendarClock,
  Check,
  CircleAlert,
  CircleMinus,
  Globe,
  Loader2,
  MemoryStick,
  Play,
  RotateCcw,
  ScrollText,
  Search,
  Sparkles,
  Stethoscope,
  Timer,
  Type,
  Zap,
  type LucideIcon,
} from "lucide-react";
import {
  Button,
  Card,
  CardContent,
  CardHeader,
  CardTitle,
  Progress,
} from "@/components/ui";
import { Badge, EmptyState, RiskBadge, StatTile } from "@/components/shared";
import { useTranslation } from "@/hooks/useTranslation";
import type { TranslationKey } from "@/i18n/translations";
import { cn } from "@/lib/utils";
import { useAppStore } from "@/stores/appStore";
import {
  listOptimizeTasks,
  onOptimizeProgress,
  runOptimizeTask,
  runOptimizeTasks,
} from "@/services/optimize";
import type {
  OptimizeOutcome,
  OptimizeProgress,
  OptimizeTask,
  OptimizeTaskId,
} from "@/types";

type ItemState = "ready" | "running" | "done" | "error" | "skipped";

/**
 * Cross-platform marker emitted by the backend when a task could not run
 * (e.g. macOS no longer ships periodic maintenance scripts). Any successful
 * outcome whose message starts with this prefix is treated as "skipped".
 */
const SKIPPED_MESSAGE_PREFIX = "Skipped:";

const isSkippedMessage = (message: string): boolean =>
  message.startsWith(SKIPPED_MESSAGE_PREFIX);

/** One line in the operation log. */
interface LogEntry {
  key: string;
  at: number;
  taskId: OptimizeTaskId;
  message: string;
  succeeded: boolean;
  skipped: boolean;
  durationMs: number;
}

const TIME_FORMAT = new Intl.DateTimeFormat("en-US", {
  hour: "2-digit",
  minute: "2-digit",
  second: "2-digit",
  hour12: false,
});

const TASK_ICONS: Record<OptimizeTaskId, LucideIcon> = {
  "flush-dns": Globe,
  "purge-memory": MemoryStick,
  "rebuild-spotlight": Search,
  "rebuild-launch-services": AppWindow,
  "clear-quicklook": Type,
  "run-maintenance-scripts": CalendarClock,
  "reset-dock": AppWindow,
  "verify-disk": Stethoscope,
};

/** Translate task labels/descriptions by id — the backend only ships English. */
const TASK_LABEL_KEYS: Record<OptimizeTaskId, TranslationKey> = {
  "flush-dns": "optimize.flushDns",
  "purge-memory": "optimize.purgeMemory",
  "rebuild-spotlight": "optimize.rebuildSpotlight",
  "rebuild-launch-services": "optimize.rebuildLaunchServices",
  "clear-quicklook": "optimize.clearQuickLook",
  "run-maintenance-scripts": "optimize.runMaintenance",
  "reset-dock": "optimize.resetDock",
  "verify-disk": "optimize.verifyDisk",
};

const TASK_DESC_KEYS: Record<OptimizeTaskId, TranslationKey> = {
  "flush-dns": "optimize.flushDnsDesc",
  "purge-memory": "optimize.purgeMemoryDesc",
  "rebuild-spotlight": "optimize.rebuildSpotlightDesc",
  "rebuild-launch-services": "optimize.rebuildLaunchServicesDesc",
  "clear-quicklook": "optimize.clearQuickLookDesc",
  "run-maintenance-scripts": "optimize.runMaintenanceDesc",
  "reset-dock": "optimize.resetDockDesc",
  "verify-disk": "optimize.verifyDiskDesc",
};

const STATE_TONE: Record<ItemState, "neutral" | "primary" | "success" | "danger" | "warning"> = {
  ready: "neutral",
  running: "primary",
  done: "success",
  error: "danger",
  skipped: "warning",
};

export function SystemOptimize() {
  const setModuleStatus = useAppStore((state) => state.setModuleStatus);
  const status = useAppStore((state) => state.moduleStatus.optimize);
  const { t } = useTranslation();

  const [tasks, setTasks] = useState<OptimizeTask[]>([]);
  const [states, setStates] = useState<Record<string, ItemState>>({});
  const [outcomes, setOutcomes] = useState<Record<string, OptimizeOutcome>>({});
  const [queue, setQueue] = useState<OptimizeTaskId[]>([]);
  const [completedInRun, setCompletedInRun] = useState(0);
  /** Append-only operation log, newest first. */
  const [log, setLog] = useState<LogEntry[]>([]);
  const [loading, setLoading] = useState(true);
  const [loadError, setLoadError] = useState(false);

  const busy = status === "running";

  // Fetch the real task catalogue from the backend.
  useEffect(() => {
    let cancelled = false;
    setLoading(true);
    setLoadError(false);
    listOptimizeTasks()
      .then((realTasks) => {
        if (cancelled) return;
        if (realTasks.length > 0) setTasks(realTasks);
        setLoading(false);
      })
      .catch((error) => {
        console.error("[SystemOptimize] listOptimizeTasks failed:", error);
        if (cancelled) return;
        setLoadError(true);
        setLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, []);

  // Subscribe to `optimize://progress` events for real-time task status.
  useEffect(() => {
    let unlisten: (() => void) | null = null;
    let cancelled = false;
    onOptimizeProgress((event: OptimizeProgress) => {
      if (cancelled) return;
      const id = event.taskId;
      const stateMap: Record<string, ItemState> = {
        pending: "ready",
        running: "running",
        done: "done",
        error: "error",
      };
      setStates((current) => ({
        ...current,
        [id]: stateMap[event.status] ?? "ready",
      }));
      if (event.status === "running" && event.total > 0) {
        setQueue((q) => (q.length === event.total ? q : Array.from({ length: event.total }, (_, i) => tasks[i]?.id).filter(Boolean) as OptimizeTaskId[]));
        setCompletedInRun(event.index);
      }
      if (event.message && (event.status === "done" || event.status === "error")) {
        setCompletedInRun(event.index + 1);
      }
    }).then((fn) => {
      if (cancelled) {
        fn();
      } else {
        unlisten = fn;
      }
    }).catch(() => {});
    return () => {
      cancelled = true;
      if (unlisten) unlisten();
    };
  }, [tasks]);

  const stateOf = (id: OptimizeTaskId): ItemState => states[id] ?? "ready";

  const totalSeconds = useMemo(
    () =>
      tasks.reduce((sum, task) => sum + task.estimatedSeconds, 0),
    [tasks],
  );

  const doneCount = tasks.filter(
    (task) => stateOf(task.id) === "done",
  ).length;
  const errorCount = tasks.filter(
    (task) => stateOf(task.id) === "error",
  ).length;
  const skippedCount = tasks.filter(
    (task) => stateOf(task.id) === "skipped",
  ).length;

  async function runTask(task: OptimizeTask) {
    setStates((current) => ({ ...current, [task.id]: "running" }));

    const started = Date.now();

    try {
      const outcome = await runOptimizeTask(task.id);
      const succeeded = outcome.succeeded;
      const message = outcome.message;
      const durationMs = Date.now() - started;
      // A successful outcome carrying a "Skipped:" marker means the backend
      // did not actually run the task — surface it as its own state.
      const skipped = succeeded && isSkippedMessage(message);
      const displayMessage = skipped ? t("optimize.skippedNoPeriodic") : message;

      setStates((current) => ({
        ...current,
        [task.id]: succeeded ? (skipped ? "skipped" : "done") : "error",
      }));
      setOutcomes((current) => ({
        ...current,
        [task.id]: {
          taskId: task.id,
          succeeded,
          message: displayMessage,
          durationMs: outcome.durationMs || durationMs,
        },
      }));
      setLog((current) =>
        [
          {
            key: `${task.id}-${started}`,
            at: Date.now(),
            taskId: task.id,
            message: displayMessage,
            succeeded,
            skipped,
            durationMs: outcome.durationMs || durationMs,
          },
          ...current,
        ].slice(0, 40),
      );
    } catch (err) {
      console.error(`[SystemOptimize] runOptimizeTask("${task.id}") failed:`, err);
      const durationMs = Date.now() - started;
      const message = t("optimize.taskFailed");

      setStates((current) => ({
        ...current,
        [task.id]: "error",
      }));
      setOutcomes((current) => ({
        ...current,
        [task.id]: {
          taskId: task.id,
          succeeded: false,
          message,
          durationMs,
        },
      }));
      setLog((current) =>
        [
          {
            key: `${task.id}-${started}`,
            at: Date.now(),
            taskId: task.id,
            message,
            succeeded: false,
            skipped: false,
            durationMs,
          },
          ...current,
        ].slice(0, 40),
      );
    }
  }

  async function runOne(task: OptimizeTask) {
    setModuleStatus("optimize", "running");
    setQueue([task.id]);
    setCompletedInRun(0);
    await runTask(task);
    setCompletedInRun(1);
    setQueue([]);
    setModuleStatus("optimize", "done");
  }

  async function runAll() {
    setModuleStatus("optimize", "running");
    setStates({});
    setOutcomes({});
    setCompletedInRun(0);
    const taskIds = tasks.map((task) => task.id);
    setQueue(taskIds);

    try {
      const results = await runOptimizeTasks(taskIds);
      // Update states and outcomes from the returned results
      const newStates: Record<string, ItemState> = {};
      const newOutcomes: Record<string, OptimizeOutcome> = {};
      const logBatch: LogEntry[] = [];
      const batchTime = Date.now();
      results.forEach((outcome, i) => {
        const taskId = taskIds[i];
        const skipped =
          outcome.succeeded && isSkippedMessage(outcome.message);
        const displayMessage = skipped
          ? t("optimize.skippedNoPeriodic")
          : outcome.message;
        newStates[taskId] = outcome.succeeded
          ? skipped
            ? "skipped"
            : "done"
          : "error";
        newOutcomes[taskId] = {
          taskId,
          succeeded: outcome.succeeded,
          message: displayMessage,
          durationMs: outcome.durationMs,
        };
        logBatch.push({
          key: `${taskId}-${batchTime}`,
          at: batchTime,
          taskId,
          message: displayMessage,
          succeeded: outcome.succeeded,
          skipped,
          durationMs: outcome.durationMs,
        });
      });
      setStates(newStates);
      setOutcomes(newOutcomes);
      setLog((current) => [...logBatch.reverse(), ...current].slice(0, 40));
      setCompletedInRun(taskIds.length);
    } catch {
      // If the batch fails entirely, mark all as error
      const errorStates: Record<string, ItemState> = {};
      taskIds.forEach((id) => {
        errorStates[id] = "error";
      });
      setStates(errorStates);
    }

    setQueue([]);
    setModuleStatus("optimize", "done");
  }

  function reset() {
    setStates({});
    setOutcomes({});
    setCompletedInRun(0);
    setLog([]);
    setModuleStatus("optimize", "idle");
  }

  function refetchTasks() {
    setLoading(true);
    setLoadError(false);
    listOptimizeTasks()
      .then((realTasks) => {
        if (realTasks.length > 0) setTasks(realTasks);
        setLoading(false);
      })
      .catch((error) => {
        console.error("[SystemOptimize] listOptimizeTasks failed:", error);
        setLoadError(true);
        setLoading(false);
      });
  }

  const runProgress = queue.length
    ? (completedInRun / queue.length) * 100
    : 0;

  if (loading) {
    return (
      <Card>
        <CardContent className="flex flex-col items-center gap-3 px-8 py-14 text-center">
          <Loader2 className="size-6 animate-spin text-primary" />
          <p className="text-[14px] font-medium">{t("common.loading")}</p>
        </CardContent>
      </Card>
    );
  }

  if (loadError) {
    return (
      <Card>
        <EmptyState
          icon={CircleAlert}
          title={t("optimize.loadFailed")}
          message={t("optimize.loadFailedMsg")}
          actionLabel={t("common.retry")}
          onAction={refetchTasks}
        />
      </Card>
    );
  }

  return (
    <div className="flex flex-col gap-4">
      {/* -------------------------------- Header ---------------------------- */}
      <div className="flex flex-wrap items-end justify-between gap-4">
        <div>
          <h2 className="text-[20px] leading-tight font-semibold tracking-tight">
            {t("optimize.maintenanceChecklist")}
          </h2>
          <p className="mt-1 text-[13px] text-muted-foreground">
            {t("optimize.maintenanceDesc")}
          </p>
        </div>

        <div className="flex items-center gap-2">
          {(doneCount > 0 || errorCount > 0 || skippedCount > 0) && !busy && (
            <Button variant="ghost" onClick={reset}>
              <RotateCcw className="size-4" />
              {t("common.reset")}
            </Button>
          )}
          <Button size="lg" onClick={runAll} disabled={busy || tasks.length === 0}>
            {busy ? (
              <Loader2 className="size-4 animate-spin" />
            ) : (
              <Zap className="size-4" />
            )}
            {busy ? t("optimize.optimizing") : t("optimize.optimizeAll")}
          </Button>
        </div>
      </div>

      {/* -------------------------------- Stats ----------------------------- */}
      <div className="grid gap-3 sm:grid-cols-3">
        <StatTile
          icon={Sparkles}
          label={t("optimize.completed")}
          value={t("optimize.completedValue", { done: doneCount, total: tasks.length })}
          hint={
            errorCount > 0
              ? t("optimize.needAttention", { count: errorCount })
              : skippedCount > 0
                ? t("optimize.skipped") + ` × ${skippedCount}`
                : t("optimize.noIssues")
          }
          accent={doneCount > 0 ? "hsl(var(--success))" : undefined}
        />
        <StatTile
          icon={Timer}
          label={t("optimize.estimatedRun")}
          value={`~${totalSeconds}s`}
          hint={t("optimize.fullChecklist")}
        />
        <StatTile
          icon={Stethoscope}
          label={t("optimize.needsAdmin")}
          value={`${tasks.filter((task) => task.requiresElevation).length} ${t("common.run").toLowerCase()}`}
          hint={t("optimize.needsAdminHint")}
          accent="hsl(var(--warning))"
        />
      </div>

      {/* ------------------------------ Run progress ------------------------ */}
      {busy && (
        <Card className="bg-card/70 backdrop-blur-xl">
          <CardContent className="flex flex-col gap-2.5 pt-5">
            <div className="flex items-center justify-between gap-4">
              <span className="flex items-center gap-2.5 text-[13px] font-medium">
                <Loader2 className="size-4 animate-spin text-primary" />
                {t("optimize.running", { current: Math.min(completedInRun + 1, queue.length), total: queue.length })}
              </span>
              <span className="text-[12.5px] text-muted-foreground tabular-nums">
                {Math.round(runProgress)}%
              </span>
            </div>
            <Progress value={runProgress} />
          </CardContent>
        </Card>
      )}

      {/* -------------------------------- Tasks ----------------------------- */}
      <div className="flex flex-col gap-2.5">
        {tasks.map((task) => (
          <TaskCard
            key={task.id}
            task={task}
            state={stateOf(task.id)}
            outcome={outcomes[task.id]}
            disabled={busy}
            onRun={() => runOne(task)}
          />
        ))}
      </div>

      {/* ------------------------------- Summary ---------------------------- */}
      {!busy && doneCount + errorCount + skippedCount > 0 && (
        <Card
          className={cn(
            errorCount > 0
              ? "border-warning/30 bg-warning/8"
              : skippedCount > 0
                ? "border-warning/30 bg-warning/8"
                : "border-success/30 bg-success/8",
          )}
        >
          <CardHeader className="flex-row items-center gap-2.5">
            <span
              className={cn(
                "flex size-8 items-center justify-center rounded-lg",
                errorCount > 0 || skippedCount > 0
                  ? "bg-warning/15 text-warning"
                  : "bg-success/15 text-success",
              )}
            >
              {errorCount > 0 ? (
                <CircleAlert className="size-4" />
              ) : skippedCount > 0 ? (
                <CircleMinus className="size-4" />
              ) : (
                <Check className="size-4" strokeWidth={2.6} />
              )}
            </span>
            <CardTitle>
              {errorCount > 0
                ? t("optimize.tasksFinished", { done: doneCount, errors: errorCount })
                : skippedCount > 0
                  ? t("optimize.tasksFinishedWithSkipped", { done: doneCount, skipped: skippedCount })
                  : t("optimize.allTasksFinished", { done: doneCount })}
            </CardTitle>
          </CardHeader>

          <CardContent className="flex flex-col gap-1.5">
            {tasks.filter((task) => outcomes[task.id]).map((task) => {
              const outcome = outcomes[task.id];

              return (
                <div
                  key={task.id}
                  className="flex items-baseline justify-between gap-3 text-[12.5px]"
                >
                  <span className="flex min-w-0 items-baseline gap-2">
                    <span
                      className={cn(
                        "font-medium",
                        outcome.succeeded ? "text-foreground" : "text-destructive",
                      )}
                    >
                      {t(TASK_LABEL_KEYS[task.id])}
                    </span>
                    <span className="truncate text-muted-foreground">
                      {outcome.message}
                    </span>
                  </span>
                  <span className="shrink-0 text-[11.5px] text-muted-foreground tabular-nums">
                    {(outcome.durationMs / 1000).toFixed(1)}s
                  </span>
                </div>
              );
            })}
          </CardContent>
        </Card>
      )}

      {/* ----------------------------- Operation log ------------------------- */}
      {log.length > 0 && (
        <Card>
          <CardHeader className="flex-row items-center gap-2.5">
            <ScrollText className="size-4 text-muted-foreground" />
            <CardTitle>{t("optimize.operationLog")}</CardTitle>
            <span className="ml-auto text-[11.5px] text-muted-foreground tabular-nums">
              {log.length} {log.length === 1 ? t("optimize.entry") : t("optimize.entries")}
            </span>
          </CardHeader>

          <CardContent className="max-h-64 overflow-y-auto">
            <ol className="flex flex-col">
              {log.map((entry, index) => (
                <li
                  key={entry.key}
                  className={cn(
                    "flex items-baseline gap-3 py-1.5 font-mono text-[11.5px]",
                    index > 0 && "border-t border-border/60",
                  )}
                >
                  <span className="shrink-0 text-muted-foreground tabular-nums">
                    {TIME_FORMAT.format(entry.at)}
                  </span>
                  <span
                    className={cn(
                      "shrink-0 font-semibold",
                      entry.skipped
                        ? "text-warning"
                        : entry.succeeded
                          ? "text-success"
                          : "text-destructive",
                    )}
                  >
                    {entry.skipped
                      ? t("optimize.logSkipped")
                      : entry.succeeded
                        ? t("optimize.logOk")
                        : t("optimize.logErr")}
                  </span>
                  <span className="min-w-0 flex-1 truncate" data-selectable>
                    <span className="font-medium text-foreground">
                      {t(TASK_LABEL_KEYS[entry.taskId])}
                    </span>{" "}
                    <span className="text-muted-foreground">{entry.message}</span>
                  </span>
                  <span className="shrink-0 text-muted-foreground tabular-nums">
                    {(entry.durationMs / 1000).toFixed(1)}s
                  </span>
                </li>
              ))}
            </ol>
          </CardContent>
        </Card>
      )}
    </div>
  );
}

interface TaskCardProps {
  task: OptimizeTask;
  state: ItemState;
  outcome: OptimizeOutcome | undefined;
  disabled: boolean;
  onRun: () => void;
}

function TaskCard({ task, state, outcome, disabled, onRun }: TaskCardProps) {
  const { t } = useTranslation();
  const Icon = TASK_ICONS[task.id];
  const tone = STATE_TONE[state];
  const labelMap: Record<ItemState, string> = {
    ready: t("optimize.statusReady"),
    running: t("optimize.statusRunning"),
    done: t("optimize.statusDone"),
    error: t("optimize.statusError"),
    skipped: t("optimize.skipped"),
  };

  return (
    <Card
      className={cn(
        "transition-all duration-200",
        state === "running" && "border-primary/40",
        state === "done" && "border-success/30",
        state === "error" && "border-destructive/30",
        state === "skipped" && "border-warning/30",
      )}
    >
      <div className="flex items-center gap-3.5 px-4 py-3.5">
        <span
          className={cn(
            "flex size-10 shrink-0 items-center justify-center rounded-xl transition-colors duration-200",
            state === "done"
              ? "bg-success/12 text-success"
              : state === "error"
                ? "bg-destructive/12 text-destructive"
                : state === "skipped"
                  ? "bg-warning/12 text-warning"
                  : "bg-primary/10 text-primary",
          )}
        >
          {state === "running" ? (
            <Loader2 className="size-[19px] animate-spin" />
          ) : state === "done" ? (
            <Check className="size-[19px]" strokeWidth={2.6} />
          ) : state === "skipped" ? (
            <CircleMinus className="size-[19px]" strokeWidth={2.2} />
          ) : (
            <Icon className="size-[19px]" strokeWidth={1.9} />
          )}
        </span>

        <div className="min-w-0 flex-1">
          <div className="flex flex-wrap items-center gap-2">
            <span className="text-[13.5px] font-semibold">{t(TASK_LABEL_KEYS[task.id])}</span>
            <Badge tone={tone}>{labelMap[state]}</Badge>
            {task.risk !== "safe" && <RiskBadge risk={task.risk} />}
            {task.requiresElevation && <Badge tone="neutral">{t("optimize.needsAdmin")}</Badge>}
          </div>
          <p className="mt-0.5 truncate text-[12px] text-muted-foreground">
            {state === "skipped"
              ? t("optimize.skippedNoPeriodic")
              : outcome && state !== "running"
                ? outcome.message
                : t(TASK_DESC_KEYS[task.id])}
          </p>
        </div>

        <span className="hidden shrink-0 text-[11.5px] text-muted-foreground tabular-nums sm:block">
          ~{task.estimatedSeconds}s
        </span>

        <Button
          variant={state === "done" ? "ghost" : "outline"}
          size="sm"
          disabled={disabled || state === "running"}
          onClick={onRun}
        >
          {state === "done" || state === "error" || state === "skipped" ? (
            <RotateCcw className="size-3.5" />
          ) : (
            <Play className="size-3.5" />
          )}
          {state === "done" || state === "error" || state === "skipped"
            ? t("optimize.again")
            : t("common.run")}
        </Button>
      </div>
    </Card>
  );
}
