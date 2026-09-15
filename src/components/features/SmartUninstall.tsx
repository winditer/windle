import { useEffect, useMemo, useState } from "react";
import {
  ArrowDownWideNarrow,
  ChevronRight,
  CircleAlert,
  Clock,
  FolderOpen,
  HardDrive,
  Loader2,
  Lock,
  Package,
  Search,
  Sparkles,
  Trash2,
} from "lucide-react";
import { Button, Card, CardContent } from "@/components/ui";
import {
  Badge,
  Checkbox,
  ConfirmDialog,
  EmptyState,
  RiskBadge,
  StatTile,
} from "@/components/shared";
import { useTranslation } from "@/hooks/useTranslation";
import { cn, formatBytes, formatRelativeTime, truncatePath } from "@/lib/utils";
import { useAppStore } from "@/stores/appStore";
import { buildUninstallPlan, listApps, uninstallApp } from "@/services/uninstall";
import type { AppLeftover, InstalledApp, LeftoverKind } from "@/types";

type SortKey = "name" | "size" | "lastUsed";

/** UI-only display labels for each leftover kind. */
const LEFTOVER_LABELS: Record<LeftoverKind, string> = {
  preferences: "Preferences",
  "application-support": "Application Support",
  caches: "Caches",
  logs: "Logs",
  "saved-state": "Saved State",
  containers: "Container",
  "launch-agent": "Launch Agent",
  receipt: "Receipt",
  other: "Other",
};

/** Deterministic pastel accent per app, used for the icon placeholders. */
function appAccent(app: InstalledApp): string {
  const palette = [
    "#007AFF",
    "#5E5CE6",
    "#FF375F",
    "#FF9500",
    "#34C759",
    "#00C7BE",
    "#AF52DE",
    "#FF2D55",
  ];
  let hash = 0;
  for (const char of app.id) hash = (hash * 31 + char.charCodeAt(0)) % 9973;
  return palette[hash % palette.length];
}

export function SmartUninstall() {
  const setModuleStatus = useAppStore((state) => state.setModuleStatus);
  const addFreedBytes = useAppStore((state) => state.addFreedBytes);
  const status = useAppStore((state) => state.moduleStatus.uninstall);
  const { t } = useTranslation();

  const sortLabels: Record<SortKey, string> = {
    size: t("uninstall.sortBySize"),
    name: t("uninstall.sortByName"),
    lastUsed: t("uninstall.sortByDate"),
  };

  const [query, setQuery] = useState("");
  const [sortKey, setSortKey] = useState<SortKey>("size");
  const [expandedId, setExpandedId] = useState<string | null>(null);
  const [removedIds, setRemovedIds] = useState<Set<string>>(new Set());
  const [deselected, setDeselected] = useState<Record<string, Set<string>>>({});
  const [pendingApp, setPendingApp] = useState<InstalledApp | null>(null);
  const [lastFreed, setLastFreed] = useState<{
    name: string;
    bytes: number;
    failed: number;
    reason: string;
  } | null>(null);
  const [appList, setAppList] = useState<InstalledApp[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState(false);
  /** Leftover plans keyed by app id, fetched lazily when an app is expanded. */
  const [plans, setPlans] = useState<Record<string, AppLeftover[]>>({});

  // Fetch the real app list from the backend on mount.
  useEffect(() => {
    let cancelled = false;
    setLoading(true);
    setError(false);
    listApps()
      .then((apps) => {
        if (cancelled) return;
        setAppList(apps);
        setLoading(false);
      })
      .catch((err) => {
        console.error("[SmartUninstall] listApps failed:", err);
        if (cancelled) return;
        setError(true);
        setLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, []);

  function refetchApps() {
    setLoading(true);
    setError(false);
    listApps()
      .then((apps) => {
        setAppList(apps);
        setLoading(false);
      })
      .catch((err) => {
        console.error("[SmartUninstall] listApps failed:", err);
        setError(true);
        setLoading(false);
      });
  }

  // Fetch leftovers for an app when it's expanded.
  async function expandApp(app: InstalledApp) {
    const newId = expandedId === app.id ? null : app.id;
    setExpandedId(newId);

    if (newId && !plans[newId]) {
      try {
        const plan = await buildUninstallPlan(app.id);
        setPlans((current) => ({ ...current, [app.id]: plan.leftovers }));
      } catch (err) {
        console.error("[SmartUninstall] buildUninstallPlan failed:", err);
        setPlans((current) => ({ ...current, [app.id]: [] }));
      }
    }
  }

  const apps = useMemo(() => {
    const needle = query.trim().toLowerCase();

    return appList.filter((app) => !removedIds.has(app.id))
      .filter(
        (app) =>
          !needle ||
          app.name.toLowerCase().includes(needle) ||
          (app.bundleId ?? "").toLowerCase().includes(needle),
      )
      .sort((a, b) => {
        if (sortKey === "name") return a.name.localeCompare(b.name);
        if (sortKey === "lastUsed") return (b.lastUsedAt ?? 0) - (a.lastUsedAt ?? 0);
        return b.bundleSize - a.bundleSize;
      });
  }, [query, sortKey, removedIds, appList]);

  const leftoversFor = (app: InstalledApp) => plans[app.id] ?? [];

  const keptLeftovers = (app: InstalledApp) => {
    const skipped = deselected[app.id] ?? new Set<string>();
    return leftoversFor(app).filter((leftover) => !skipped.has(leftover.path));
  };

  const planSize = (app: InstalledApp) =>
    app.bundleSize +
    keptLeftovers(app).reduce((sum, leftover) => sum + leftover.size, 0);

  const totalLeftoverSize = apps.reduce(
    (sum, app) =>
      sum + leftoversFor(app).reduce((inner, leftover) => inner + leftover.size, 0),
    0,
  );
  const totalBundleSize = apps.reduce((sum, app) => sum + app.bundleSize, 0);

  function toggleLeftover(app: InstalledApp, path: string, checked: boolean) {
    setDeselected((current) => {
      const skipped = new Set(current[app.id] ?? []);
      if (checked) skipped.delete(path);
      else skipped.add(path);
      return { ...current, [app.id]: skipped };
    });
  }

  async function confirmUninstall() {
    const app = pendingApp;
    if (!app) return;

    // The bundle always goes; the checkboxes only opt leftovers out.
    const paths = [
      app.path,
      ...keptLeftovers(app).map((leftover) => leftover.path),
    ];
    setPendingApp(null);
    setModuleStatus("uninstall", "running");

    try {
      const outcome = await uninstallApp(app.id, paths);
      const bundleRemoved = outcome.removedPaths.includes(app.path);
      if (bundleRemoved) {
        setRemovedIds((current) => new Set(current).add(app.id));
      }
      setExpandedId(null);
      // The plan is stale now — a retry must not re-offer paths already gone.
      setPlans((current) => {
        if (!(app.id in current)) return current;
        const next = { ...current };
        delete next[app.id];
        return next;
      });
      addFreedBytes(outcome.freedBytes);
      setLastFreed({
        name: app.name,
        bytes: outcome.freedBytes,
        failed: outcome.failedPaths.length,
        reason: outcome.failedPaths[0]?.reason ?? "",
      });
      setModuleStatus("uninstall", bundleRemoved ? "done" : "error");
    } catch (err) {
      console.error("[SmartUninstall] uninstallApp failed:", err);
      setModuleStatus("uninstall", "error");
    }
  }

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

  if (error) {
    return (
      <Card>
        <EmptyState
          icon={CircleAlert}
          title={t("common.loadFailed")}
          message={t("common.loadFailedMsg")}
          actionLabel={t("common.retry")}
          onAction={refetchApps}
        />
      </Card>
    );
  }

  return (
    <div className="flex flex-col gap-4">
      {/* -------------------------------- Stats ----------------------------- */}
      <div className="grid gap-3 sm:grid-cols-3">
        <StatTile
          icon={Package}
          label={t("uninstall.applications")}
          value={`${apps.length}`}
          hint={`${appList.filter((app) => app.isSystem).length} ${t("uninstall.managedByMacOS")}`}
        />
        <StatTile
          icon={HardDrive}
          label={t("uninstall.appBundles")}
          value={formatBytes(totalBundleSize)}
          hint={t("uninstall.appBundlesHint")}
        />
        <StatTile
          icon={Sparkles}
          label={t("uninstall.reclaimableLeftovers")}
          value={formatBytes(totalLeftoverSize)}
          hint={t("uninstall.reclaimableLeftoversHint")}
          accent="hsl(var(--warning))"
        />
      </div>

      {/* ------------------------------- Toolbar ---------------------------- */}
      <div className="flex flex-wrap items-center gap-2.5">
        <div className="relative min-w-[220px] flex-1">
          <Search className="pointer-events-none absolute top-1/2 left-3 size-4 -translate-y-1/2 text-muted-foreground" />
          <input
            value={query}
            onChange={(event) => setQuery(event.target.value)}
            placeholder={t("uninstall.searchPlaceholder")}
            className="h-9 w-full rounded-lg border border-border bg-card/70 pr-3 pl-9 text-[13px] backdrop-blur-xl outline-none transition-all duration-200 placeholder:text-muted-foreground focus:border-primary/50 focus:ring-2 focus:ring-ring/25"
          />
        </div>

        <div className="relative">
          <ArrowDownWideNarrow className="pointer-events-none absolute top-1/2 left-3 size-4 -translate-y-1/2 text-muted-foreground" />
          <select
            value={sortKey}
            onChange={(event) => setSortKey(event.target.value as SortKey)}
            className="h-9 appearance-none rounded-lg border border-border bg-card/70 pr-8 pl-9 text-[13px] font-medium backdrop-blur-xl outline-none transition-all duration-200 focus:border-primary/50 focus:ring-2 focus:ring-ring/25"
          >
            {(Object.keys(sortLabels) as SortKey[]).map((key) => (
              <option key={key} value={key}>
                {sortLabels[key]}
              </option>
            ))}
          </select>
          <ChevronRight className="pointer-events-none absolute top-1/2 right-2.5 size-3.5 -translate-y-1/2 rotate-90 text-muted-foreground" />
        </div>
      </div>

      {/* ------------------------------- Outcome ---------------------------- */}
      {lastFreed && (
        <Card
          className={cn(
            "border-success/30 bg-success/8",
            lastFreed.failed > 0 && "border-warning/40 bg-warning/8",
          )}
        >
          <CardContent className="flex items-center gap-3.5 pt-5">
            <span
              className={cn(
                "flex size-10 items-center justify-center rounded-xl bg-success/15 text-success",
                lastFreed.failed > 0 && "bg-warning/15 text-warning",
              )}
            >
              <Sparkles className="size-5" strokeWidth={1.9} />
            </span>
            <div>
              <p className="text-[14px] font-semibold tracking-tight">
                {lastFreed.failed > 0
                  ? t("uninstall.partial", {
                      name: lastFreed.name,
                      failed: lastFreed.failed,
                    })
                  : t("uninstall.removed", {
                      name: lastFreed.name,
                      bytes: formatBytes(lastFreed.bytes),
                    })}
              </p>
              <p className="text-[12.5px] text-muted-foreground">
                {lastFreed.failed > 0
                  ? lastFreed.reason
                  : t("uninstall.removedDetail")}
              </p>
            </div>
          </CardContent>
        </Card>
      )}

      {/* --------------------------------- List ----------------------------- */}
      {apps.length === 0 ? (
        <Card>
          <EmptyState
            icon={Search}
            title={t("uninstall.noMatchingApps")}
            message={t("uninstall.noMatchingAppsMsg")}
            actionLabel={t("uninstall.clearSearch")}
            onAction={() => setQuery("")}
          />
        </Card>
      ) : (
        <div className="flex flex-col gap-2">
          {apps.map((app) => (
            <AppRow
              key={app.id}
              app={app}
              leftovers={leftoversFor(app)}
              skipped={deselected[app.id] ?? new Set()}
              expanded={expandedId === app.id}
              planSize={planSize(app)}
              busy={status === "running"}
              onExpand={() => expandApp(app)}
              onToggleLeftover={(path, checked) =>
                toggleLeftover(app, path, checked)
              }
              onUninstall={() => setPendingApp(app)}
            />
          ))}
        </div>
      )}

      <ConfirmDialog
        open={pendingApp !== null}
        destructive
        title={pendingApp ? t("uninstall.confirmTitle") + ` ${pendingApp.name}?` : ""}
        message={
          pendingApp
            ? t("uninstall.confirmMsg", { bytes: formatBytes(planSize(pendingApp)), count: keptLeftovers(pendingApp).length })
            : ""
        }
        confirmLabel={t("uninstall.uninstall")}
        cancelLabel={t("common.cancel")}
        busy={status === "running"}
        onConfirm={confirmUninstall}
        onClose={() => setPendingApp(null)}
      >
        <div className="flex flex-col gap-1.5 rounded-lg bg-muted/60 p-3 text-[12px]">
          <div className="flex items-center justify-between gap-3">
            <span className="truncate font-mono" data-selectable>
              {pendingApp?.path}
            </span>
            <span className="shrink-0 text-muted-foreground tabular-nums">
              {pendingApp ? formatBytes(pendingApp.bundleSize) : ""}
            </span>
          </div>
          {pendingApp &&
            keptLeftovers(pendingApp).map((leftover) => (
              <div
                key={leftover.path}
                className="flex items-center justify-between gap-3 text-muted-foreground"
              >
                <span className="truncate font-mono" data-selectable>
                  {truncatePath(leftover.path, 3)}
                </span>
                <span className="shrink-0 tabular-nums">
                  {formatBytes(leftover.size)}
                </span>
              </div>
            ))}
        </div>
      </ConfirmDialog>
    </div>
  );
}

interface AppRowProps {
  app: InstalledApp;
  leftovers: AppLeftover[];
  skipped: Set<string>;
  expanded: boolean;
  planSize: number;
  busy: boolean;
  onExpand: () => void;
  onToggleLeftover: (path: string, checked: boolean) => void;
  onUninstall: () => void;
}

function AppRow({
  app,
  leftovers,
  skipped,
  expanded,
  planSize,
  busy,
  onExpand,
  onToggleLeftover,
  onUninstall,
}: AppRowProps) {
  const { t } = useTranslation();
  const accent = appAccent(app);
  const leftoverSize = leftovers.reduce((sum, leftover) => sum + leftover.size, 0);
  const loadingLeftovers = expanded && leftovers.length === 0;

  return (
    <Card
      className={cn(
        "overflow-hidden transition-all duration-200",
        expanded && "border-primary/40 shadow-[0_4px_24px_rgb(0_0_0_/_0.08)]",
      )}
    >
      <button
        type="button"
        onClick={onExpand}
        className="flex w-full items-center gap-3.5 px-4 py-3 text-left transition-colors duration-150 hover:bg-accent/40"
      >
        <span
          className="flex size-10 shrink-0 items-center justify-center rounded-[11px] text-[15px] font-semibold text-white shadow-sm"
          style={{
            background: `linear-gradient(135deg, ${accent}, ${accent}B0)`,
          }}
        >
          {app.name.slice(0, 1).toUpperCase()}
        </span>

        <span className="min-w-0 flex-1">
          <span className="flex items-center gap-2">
            <span className="truncate text-[13.5px] font-semibold">{app.name}</span>
            {app.version && (
              <span className="shrink-0 text-[11.5px] text-muted-foreground tabular-nums">
                {app.version}
              </span>
            )}
            {app.isSystem && (
              <Badge tone="neutral" className="gap-1">
                <Lock className="size-2.5" />
                {t("uninstall.systemApp")}
              </Badge>
            )}
          </span>
          <span className="mt-0.5 flex items-center gap-3 text-[11.5px] text-muted-foreground">
            <span className="flex items-center gap-1">
              <Clock className="size-3" />
              {app.lastUsedAt
                ? `${t("uninstall.used")} ${formatRelativeTime(app.lastUsedAt).toLowerCase()}`
                : t("uninstall.neverUsed")}
            </span>
            <span className="truncate font-mono">{app.bundleId ?? app.path}</span>
          </span>
        </span>

        <span className="shrink-0 text-right">
          <span className="block text-[15px] font-semibold tabular-nums">
            {formatBytes(app.bundleSize)}
          </span>
          {leftoverSize > 0 && (
            <span className="block text-[11.5px] text-muted-foreground tabular-nums">
              +{formatBytes(leftoverSize)} {t("uninstall.leftovers")}
            </span>
          )}
        </span>

        <ChevronRight
          className={cn(
            "size-4 shrink-0 text-muted-foreground transition-transform duration-200",
            expanded && "rotate-90",
          )}
        />
      </button>

      {expanded && (
        <div className="animate-fade-in border-t border-border bg-muted/25">
          <div className="flex flex-wrap items-center justify-between gap-3 px-4 py-3">
            <div className="flex items-center gap-2 text-[12px] text-muted-foreground">
              <FolderOpen className="size-3.5" />
              <span className="truncate font-mono" data-selectable>
                {app.path}
              </span>
            </div>

            <div className="flex items-center gap-3">
              <span className="text-[12.5px] text-muted-foreground">
                {t("uninstall.removing")}{" "}
                <span className="font-semibold text-foreground tabular-nums">
                  {formatBytes(planSize)}
                </span>
              </span>
              <Button
                variant="destructive"
                size="sm"
                disabled={app.isSystem || busy}
                onClick={onUninstall}
                title={
                  app.isSystem ? t("uninstall.systemAppTooltip") : undefined
                }
              >
                <Trash2 className="size-3.5" />
                {t("uninstall.uninstall")}
              </Button>
            </div>
          </div>

          {loadingLeftovers ? (
            <div className="flex items-center gap-2 px-4 pb-3 text-[12px] text-muted-foreground">
              <Loader2 className="size-3.5 animate-spin" />
              {t("common.loading")}
            </div>
          ) : leftovers.length > 0 ? (
            <div className="px-4 pb-3">
              <p className="mb-1.5 text-[11px] font-medium tracking-wide text-muted-foreground uppercase">
                {t("uninstall.relatedFiles")} ({leftovers.length})
              </p>

              <div className="flex flex-col">
                {leftovers.map((leftover) => (
                  <label
                    key={leftover.path}
                    className="flex cursor-pointer items-center gap-3 border-b border-border/60 py-2 last:border-0"
                  >
                    <Checkbox
                      checked={!skipped.has(leftover.path)}
                      onChange={(checked) => onToggleLeftover(leftover.path, checked)}
                      label={leftover.path}
                    />

                    <span className="min-w-0 flex-1">
                      <span className="flex items-center gap-2">
                        <span className="text-[12.5px] font-medium">
                          {LEFTOVER_LABELS[leftover.kind]}
                        </span>
                        <RiskBadge risk={leftover.risk} />
                      </span>
                      <span
                        className="block truncate font-mono text-[11px] text-muted-foreground"
                        data-selectable
                      >
                        {truncatePath(leftover.path, 4)}
                      </span>
                    </span>

                    <span className="shrink-0 text-[12.5px] font-medium tabular-nums">
                      {formatBytes(leftover.size)}
                    </span>
                  </label>
                ))}
              </div>
            </div>
          ) : (
            <div className="px-4 pb-3 text-[12px] text-muted-foreground">
              {t("uninstall.noMatchingAppsMsg")}
            </div>
          )}
        </div>
      )}
    </Card>
  );
}
