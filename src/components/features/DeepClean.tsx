import { memo, useCallback, useEffect, useMemo, useState } from "react";
import {
  CircleAlert,
  Boxes,
  ChevronDown,
  ChevronRight,
  CircleSlash,
  Database,
  FileClock,
  FlaskConical,
  Globe,
  HardDriveDownload,
  Hammer,
  Languages,
  Loader2,
  Package,
  Radar,
  RotateCcw,
  ShieldAlert,
  ShieldCheck,
  Sparkles,
  Trash2,
  type LucideIcon,
} from "lucide-react";
import {
  Button,
  Card,
  CardContent,
  Progress,
} from "@/components/ui";
import { Badge, Checkbox, ConfirmDialog, EmptyState, RiskBadge } from "@/components/shared";
import { useTranslation } from "@/hooks/useTranslation";
import type { TranslationKey } from "@/i18n/translations";
import {
  CATEGORY_LABEL_KEYS,
  localizedScanStage,
  localizedText,
  type TranslateFn,
} from "@/lib/cleanStage";
import { cn, formatBytes, formatRelativeTime, truncatePath } from "@/lib/utils";
import { useAppStore } from "@/stores/appStore";
import { cancelScan, cleanPaths, onCleanProgress, scanJunk } from "@/services/clean";
import { openFullDiskAccessSettings } from "@/services/dashboard";
import type {
  CleanCategory,
  CleanCategoryId,
  CleanableItem,
  CleanScanResult,
  ItemGroup,
  ProgressEvent,
} from "@/types";

const CATEGORY_ICONS: Record<CleanCategoryId, LucideIcon> = {
  "system-cache": Database,
  "user-cache": Boxes,
  "app-junk": Package,
  "browser-cache": Globe,
  "app-logs": FileClock,
  "xcode-derived-data": Hammer,
  "ios-backups": HardDriveDownload,
  "language-files": Languages,
  "mail-attachments": FileClock,
  downloads: HardDriveDownload,
  trash: Trash2,
  "broken-symlinks": CircleSlash,
};

/** Translation keys for localized category descriptions. */
const CATEGORY_DESC_KEYS: Record<CleanCategoryId, TranslationKey> = {
  "system-cache": "deepClean.categoryDesc.system-cache",
  "user-cache": "deepClean.categoryDesc.user-cache",
  "app-junk": "deepClean.categoryDesc.app-junk",
  "browser-cache": "deepClean.categoryDesc.browser-cache",
  "app-logs": "deepClean.categoryDesc.app-logs",
  "xcode-derived-data": "deepClean.categoryDesc.xcode-derived-data",
  "ios-backups": "deepClean.categoryDesc.ios-backups",
  "language-files": "deepClean.categoryDesc.language-files",
  "mail-attachments": "deepClean.categoryDesc.mail-attachments",
  downloads: "deepClean.categoryDesc.downloads",
  trash: "deepClean.categoryDesc.trash",
  "broken-symlinks": "deepClean.categoryDesc.broken-symlinks",
};

/**
 * Translation keys for localized group names. Group ids arrive as plain
 * strings from the backend, so unknown ids fall back to the backend label.
 */
const GROUP_LABEL_KEYS: Record<string, TranslationKey> = {
  "user-logs": "deepClean.group.user-logs",
  "crash-reports": "deepClean.group.crash-reports",
  "system-logs": "deepClean.group.system-logs",
  "sandbox-logs": "deepClean.group.sandbox-logs",
  "app-caches": "deepClean.group.app-caches",
  "http-storages": "deepClean.group.http-storages",
  "saved-state": "deepClean.group.saved-state",
  webkit: "deepClean.group.webkit",
  "sandbox-caches": "deepClean.group.sandbox-caches",
  "system-caches": "deepClean.group.system-caches",
  "temp-files": "deepClean.group.temp-files",
};

/** Localized category name (backend English label as fallback). */
function localizedCategoryLabel(
  t: TranslateFn,
  category: CleanCategory,
): string {
  return localizedText(t, CATEGORY_LABEL_KEYS[category.id], category.label);
}

/** Localized category description (backend English text as fallback). */
function localizedCategoryDesc(
  t: TranslateFn,
  category: CleanCategory,
): string {
  return localizedText(t, CATEGORY_DESC_KEYS[category.id], category.description);
}

/** Localized group name (backend English label as fallback). */
function localizedGroupLabel(t: TranslateFn, group: ItemGroup): string {
  const key = GROUP_LABEL_KEYS[group.id];
  return key ? localizedText(t, key, group.label) : group.label;
}

const RISK_STYLE = {
  safe: "bg-success/12 text-success",
  caution: "bg-warning/12 text-warning",
  danger: "bg-destructive/12 text-destructive",
} as const;

/** Items of one sub-group, aggregated for the expandable list. */
interface GroupView {
  group: ItemGroup;
  items: CleanableItem[];
  totalSize: number;
}

/** A category plus its grouped / ungrouped item split. */
interface CategoryView {
  category: CleanCategory;
  /** Sub-groups sorted by total size, descending. */
  groups: GroupView[];
  /** Items without a group (group === null), sorted by size, descending. */
  ungrouped: CleanableItem[];
  /** Every item path in this category (selection works at item granularity). */
  paths: string[];
}

/** Aggregate category items by `group.id`, keeping size-descending order. */
function buildCategoryView(category: CleanCategory): CategoryView {
  const byGroup = new Map<string, GroupView>();
  const ungrouped: CleanableItem[] = [];

  for (const item of category.items) {
    if (item.group) {
      let view = byGroup.get(item.group.id);
      if (!view) {
        view = { group: item.group, items: [], totalSize: 0 };
        byGroup.set(item.group.id, view);
      }
      view.items.push(item);
      view.totalSize += item.size;
    } else {
      ungrouped.push(item);
    }
  }

  const groups = [...byGroup.values()]
    .map((view) => ({
      ...view,
      items: [...view.items].sort((a, b) => b.size - a.size),
    }))
    .sort((a, b) => b.totalSize - a.totalSize);
  ungrouped.sort((a, b) => b.size - a.size);

  return {
    category,
    groups,
    ungrouped,
    paths: category.items.map((item) => item.path),
  };
}

/** Toggle a batch of item paths in the selection set. */
function applyPaths(
  set: Set<string>,
  paths: string[],
  checked: boolean,
): Set<string> {
  const next = new Set(set);
  for (const path of paths) {
    if (checked) next.add(path);
    else next.delete(path);
  }
  return next;
}

export function DeepClean() {
  const status = useAppStore((state) => state.moduleStatus.clean);
  const progress = useAppStore((state) => state.progress);
  const setProgress = useAppStore((state) => state.setProgress);
  const setModuleStatus = useAppStore((state) => state.setModuleStatus);
  const addFreedBytes = useAppStore((state) => state.addFreedBytes);
  const { t, lang } = useTranslation();
  const locale = lang === "zh-CN" ? "zh-CN" : "en-US";

  const [result, setResult] = useState<CleanScanResult | null>(null);
  /** Selected item paths — the selection granularity is a single item. */
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [expanded, setExpanded] = useState<Set<CleanCategoryId>>(new Set());
  const [confirmOpen, setConfirmOpen] = useState(false);
  const [freedBytes, setFreedBytes] = useState(0);
  /** Dry run mode reports what *would* go without touching a single file. */
  const [dryRun, setDryRun] = useState(false);
  const [preview, setPreview] = useState<{ bytes: number; files: number } | null>(
    null,
  );
  const [error, setError] = useState(false);
  /** Items the last clean could not remove (NeedsElevation etc.). */
  const [failedCount, setFailedCount] = useState(0);

  // Subscribe to `clean://progress` events so the progress bar tracks real scans.
  useEffect(() => {
    let unlisten: (() => void) | null = null;
    let cancelled = false;
    onCleanProgress((event: ProgressEvent) => {
      if (cancelled) return;
      setProgress({
        progress: event.progress,
        currentPath: event.currentPath,
        itemsScanned: event.itemsScanned,
        bytesFound: event.bytesFound,
      });
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
  }, [setProgress]);

  const categories = result?.categories ?? [];

  const categoryViews = useMemo(
    () => categories.map(buildCategoryView),
    [categories],
  );

  const allItems = useMemo(
    () => categories.flatMap((category) => category.items),
    [categories],
  );
  const totalItems = allItems.length;

  const selectedItems = useMemo(
    () => allItems.filter((item) => selected.has(item.path)),
    [allItems, selected],
  );
  const selectedFiles = selectedItems.length;
  const selectedSize = selectedItems.reduce(
    (sum, item) => sum + item.size,
    0,
  );

  /** Categories that have at least one selected item. */
  const selectedCategoryViews = useMemo(
    () =>
      categoryViews.filter((view) =>
        view.paths.some((path) => selected.has(path)),
      ),
    [categoryViews, selected],
  );

  const scanning = status === "scanning";
  const cleaning = status === "running";

  async function startScan() {
    setResult(null);
    setSelected(new Set());
    setExpanded(new Set());
    setFreedBytes(0);
    setPreview(null);
    setError(false);
    setFailedCount(0);

    setModuleStatus("clean", "scanning");
    try {
      const realResult = await scanJunk();
      setResult(realResult);
      // Default-check only items that are safe at BOTH the category and the
      // item level — the same item-level filter as QuickCleanTab. Item-level
      // Caution entries (root-owned system logs / diagnostic archives) stay
      // unchecked by default; the user can still tick them manually.
      setSelected(
        new Set(
          realResult.categories
            .filter((category) => category.risk === "safe")
            .flatMap((category) =>
              category.items
                .filter((item) => item.risk === "safe")
                .map((item) => item.path),
            ),
        ),
      );
      setModuleStatus("clean", "ready");
    } catch (err) {
      console.error("[DeepClean] scanJunk failed:", err);
      setProgress(null);
      setModuleStatus("clean", "error");
      setError(true);
    }
  }

  async function runClean() {
    setConfirmOpen(false);
    const bytes = selectedSize;
    const files = selectedFiles;
    const paths = Array.from(selected);

    if (dryRun) {
      // Dry run: just report what would be removed without touching files.
      setPreview({ bytes, files });
      return;
    }

    setModuleStatus("clean", "running");
    setPreview(null);
    setFailedCount(0);
    try {
      const outcome = await cleanPaths(paths);
      const freed = outcome.freedBytes;
      setFreedBytes(freed);
      setFailedCount(outcome.failedPaths.length);
      addFreedBytes(freed);

      const failedPathSet = new Set(
        outcome.failedPaths.map((f) => f.path),
      );
      const cleanedPathSet = new Set(paths);

      setResult((current) => {
        if (!current) return current;

        // Deduct from the header total with the same snapshot sizes the
        // category cards re-total from, so the two always agree — `freed`
        // reflects on-disk bytes at deletion time, which drifts whenever
        // logs grow between the scan and the clean.
        const removedSnapshotBytes = current.categories
          .flatMap((category) => category.items)
          .filter(
            (item) =>
              cleanedPathSet.has(item.path) && !failedPathSet.has(item.path),
          )
          .reduce((sum, item) => sum + item.size, 0);

        return {
          ...current,
          categories: current.categories
            .map((category) => {
              // Keep items that were not part of this run, plus any that
              // failed deletion — everything else is gone for good.
              const remainingItems = category.items.filter(
                (item) =>
                  !cleanedPathSet.has(item.path) ||
                  failedPathSet.has(item.path),
              );
              if (remainingItems.length === 0) return null;
              const remainingSize = remainingItems.reduce(
                (sum, item) => sum + item.size,
                0,
              );
              return {
                ...category,
                items: remainingItems,
                totalSize: remainingSize,
                itemCount: remainingItems.length,
              };
            })
            .filter((c): c is CleanCategory => c !== null),
          totalSize: Math.max(0, current.totalSize - removedSnapshotBytes),
        };
      });
      setSelected(new Set());
      setModuleStatus("clean", "done");
    } catch (err) {
      console.error("[DeepClean] cleanPaths failed:", err);
      setProgress(null);
      setModuleStatus("clean", "error");
      setError(true);
    }
  }

  function handleCancel() {
    cancelScan().catch(() => {});
    setModuleStatus("clean", "idle");
    setProgress(null);
  }

  /** Toggle a batch of item paths (single item, group or whole category).
   *  Stable reference (functional state update) so memoized rows skip
   *  re-rendering when only one row is ticked. */
  const togglePaths = useCallback((paths: string[], checked: boolean) => {
    setSelected((current) => applyPaths(current, paths, checked));
  }, []);

  function toggleExpanded(id: CleanCategoryId) {
    setExpanded((current) => {
      const next = new Set(current);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  }

  const allSelected = totalItems > 0 && selectedFiles === totalItems;
  const someSelected = selectedFiles > 0 && !allSelected;

  // Error state
  if (error && !scanning && !cleaning) {
    return (
      <Card>
        <EmptyState
          icon={CircleAlert}
          title={t("common.scanFailed")}
          message={t("common.scanFailedMsg")}
          actionLabel={t("common.retry")}
          onAction={startScan}
        />
      </Card>
    );
  }

  return (
    <div className="flex flex-col gap-4">
      {/* -------------------------------- Header ---------------------------- */}
      <div className="flex flex-wrap items-end justify-between gap-4">
        <div>
          <p className="text-[11px] font-medium tracking-wide text-muted-foreground uppercase">
            {t("deepClean.cleanableNow")}
          </p>
          <p className="text-[28px] leading-tight font-semibold tracking-tight tabular-nums">
            {formatBytes(result?.totalSize ?? 0)}
          </p>
          <p className="mt-0.5 text-[12.5px] text-muted-foreground">
            {result
              ? `${t("deepClean.categoryCount", { count: categories.length })} · ${formatRelativeTime(result.scannedAt, locale).toLowerCase()}`
              : t("deepClean.runScanHint")}
          </p>
        </div>

        <div className="flex items-center gap-2">
          <button
            type="button"
            role="switch"
            aria-checked={dryRun}
            onClick={() => setDryRun((value) => !value)}
            disabled={scanning || cleaning}
            title={t("deepClean.dryRunTooltip")}
            className={cn(
              "flex items-center gap-2 rounded-lg border px-2.5 py-[7px] text-[12.5px] font-medium transition-all duration-200 disabled:opacity-50",
              dryRun
                ? "border-primary/40 bg-primary/10 text-primary"
                : "border-border bg-card text-muted-foreground hover:text-foreground",
            )}
          >
            <FlaskConical className="size-4" />
            {t("deepClean.dryRun")}
          </button>

          {scanning ? (
            <Button variant="outline" onClick={handleCancel}>
              {t("common.stop")}
            </Button>
          ) : (
            <Button variant="outline" onClick={startScan} disabled={cleaning}>
              {result ? (
                <RotateCcw className="size-4" />
              ) : (
                <Radar className="size-4" />
              )}
              {result ? t("common.rescan") : t("common.scan")}
            </Button>
          )}

          <Button
            variant={dryRun ? "default" : "destructive"}
            disabled={selectedFiles === 0 || scanning || cleaning}
            onClick={() => setConfirmOpen(true)}
          >
            {cleaning ? (
              <Loader2 className="size-4 animate-spin" />
            ) : dryRun ? (
              <FlaskConical className="size-4" />
            ) : (
              <Trash2 className="size-4" />
            )}
            {cleaning
              ? dryRun
                ? t("deepClean.previewing")
                : t("deepClean.cleaningBtn")
              : `${dryRun ? t("deepClean.previewSelected") : t("deepClean.cleanSelectedBtn")}${selectedSize > 0 ? ` · ${formatBytes(selectedSize)}` : ""}`}
          </Button>
        </div>
      </div>

      {/* ------------------------------- Scanning --------------------------- */}
      {scanning && (
        <Card className="bg-card/70 backdrop-blur-xl">
          <CardContent className="flex flex-col gap-3 pt-5">
            <div className="flex items-center justify-between gap-4">
              <div className="flex items-center gap-2.5">
                <Loader2 className="size-4 animate-spin text-primary" />
                <span className="text-[13px] font-medium">
                  {t("deepClean.scanningHint")}
                </span>
              </div>
              <span className="text-[13px] font-semibold tabular-nums">
                {formatBytes(progress?.bytesFound ?? 0)} {t("deepClean.found")}
              </span>
            </div>

            <Progress
              value={progress?.progress != null ? progress.progress * 100 : null}
            />

            <p className="truncate font-mono text-[11px] text-muted-foreground">
              {localizedScanStage(t, progress?.currentPath)}
            </p>
            <p className="text-[11.5px] text-muted-foreground tabular-nums">
              {(progress?.itemsScanned ?? 0).toLocaleString()} {t("deepClean.itemsInspected")}
            </p>
          </CardContent>
        </Card>
      )}

      {/* ------------------------------ Dry run ----------------------------- */}
      {preview && !cleaning && (
        <Card className="border-primary/30 bg-primary/8">
          <CardContent className="flex flex-wrap items-center justify-between gap-4 pt-5">
            <div className="flex items-center gap-3.5">
              <span className="flex size-10 items-center justify-center rounded-xl bg-primary/15 text-primary">
                <FlaskConical className="size-5" strokeWidth={1.9} />
              </span>
              <div>
                <p className="text-[15px] font-semibold tracking-tight">
                  {t("deepClean.dryRunResult", { bytes: formatBytes(preview.bytes) })}
                </p>
                <p className="text-[12.5px] text-muted-foreground tabular-nums">
                  {t("deepClean.dryRunDetail", {
                    files: preview.files.toLocaleString(),
                    categories: selectedCategoryViews.length,
                  })}
                </p>
              </div>
            </div>
            <Button
              variant="destructive"
              onClick={() => {
                setDryRun(false);
                setConfirmOpen(true);
              }}
            >
              <Trash2 className="size-4" />
              {t("deepClean.cleanForReal")}
            </Button>
          </CardContent>
        </Card>
      )}

      {/* -------------------------------- Result ---------------------------- */}
      {status === "done" && (freedBytes > 0 || failedCount > 0) && (
        <Card className="border-success/30 bg-success/8">
          <CardContent className="flex flex-wrap items-center justify-between gap-4 pt-5">
            <div className="flex items-center gap-3.5">
              <span className="flex size-10 items-center justify-center rounded-xl bg-success/15 text-success">
                <Sparkles className="size-5" strokeWidth={1.9} />
              </span>
              <div>
                <p className="text-[15px] font-semibold tracking-tight">
                  {t("deepClean.freedResult", { bytes: formatBytes(freedBytes) })}
                </p>
                <p className="text-[12.5px] text-muted-foreground">
                  {t("deepClean.freedDetail")}
                </p>
                {failedCount > 0 && (
                  <p className="mt-1 text-[12px] text-warning">
                    {t("deepClean.failedItems", {
                      count: failedCount.toLocaleString(),
                    })}
                  </p>
                )}
              </div>
            </div>
            <Button variant="outline" onClick={startScan}>
              <RotateCcw className="size-4" />
              {t("deepClean.scanAgain")}
            </Button>
          </CardContent>
        </Card>
      )}

      {/* ------------------------------ Categories -------------------------- */}
      {categories.length > 0 && (
        <>
          <div className="flex items-center justify-between gap-4 rounded-xl border border-border bg-card/60 px-4 py-2.5 backdrop-blur-xl">
            <label className="flex cursor-pointer items-center gap-2.5">
              <Checkbox
                checked={allSelected}
                indeterminate={someSelected}
                onChange={(checked) =>
                  togglePaths(
                    allItems.map((item) => item.path),
                    checked,
                  )
                }
                label={t("common.selectAll")}
              />
              <span className="text-[13px] font-medium">{t("common.selectAll")}</span>
            </label>

            <span className="text-[12.5px] text-muted-foreground tabular-nums">
              {t("deepClean.selectedCount", { selected: selectedFiles, total: totalItems })}
              <span className="font-semibold text-foreground">
                {formatBytes(selectedSize)}
              </span>
            </span>
          </div>

          <div className="flex flex-col gap-2.5">
            {categoryViews.map((view) => (
              <CategoryCard
                key={view.category.id}
                view={view}
                selected={selected}
                expanded={expanded.has(view.category.id)}
                disabled={cleaning}
                onTogglePaths={togglePaths}
                onExpand={() => toggleExpanded(view.category.id)}
              />
            ))}
          </div>
        </>
      )}

      {/* -------------------------------- Empty ----------------------------- */}
      {!scanning && categories.length === 0 && status !== "done" && (
        <Card>
          <EmptyState
            icon={Radar}
            title={t("deepClean.nothingScanned")}
            message={t("deepClean.emptyMessage")}
            actionLabel={t("deepClean.startScan")}
            onAction={startScan}
          />
        </Card>
      )}

      {!scanning && categories.length === 0 && status === "done" && (
        <Card>
          <EmptyState
            icon={ShieldCheck}
            title={t("deepClean.allClean")}
            message={t("deepClean.allCleanMessage")}
            actionLabel={t("deepClean.scanAgain")}
            onAction={startScan}
          />
        </Card>
      )}

      <ConfirmDialog
        open={confirmOpen}
        destructive={!dryRun}
        title={
          dryRun
            ? t("deepClean.confirmPreview", { bytes: formatBytes(selectedSize) })
            : t("deepClean.confirmRemove", { bytes: formatBytes(selectedSize) })
        }
        message={
          dryRun
            ? t("deepClean.confirmPreviewMsg", {
                files: selectedFiles.toLocaleString(),
                categories: selectedCategoryViews.length,
              })
            : t("deepClean.confirmRemoveMsg", {
                files: selectedFiles.toLocaleString(),
                categories: selectedCategoryViews.length,
              })
        }
        confirmLabel={dryRun ? t("deepClean.dryRun") : t("deepClean.cleanSelected")}
        cancelLabel={t("common.cancel")}
        onConfirm={runClean}
        onClose={() => setConfirmOpen(false)}
      >
        {/* Category → group breakdown of what is about to be cleaned. */}
        <ul className="flex flex-col gap-2 rounded-lg bg-muted/60 p-3">
          {selectedCategoryViews.map((view) => {
            const categoryBytes = view.category.items
              .filter((item) => selected.has(item.path))
              .reduce((sum, item) => sum + item.size, 0);
            const groupRows = view.groups
              .map((group) => {
                const chosen = group.items.filter((item) =>
                  selected.has(item.path),
                );
                return {
                  group: group.group,
                  count: chosen.length,
                  bytes: chosen.reduce((sum, item) => sum + item.size, 0),
                };
              })
              .filter((row) => row.count > 0);

            return (
              <li key={view.category.id} className="flex flex-col gap-1">
                <div className="flex items-center justify-between gap-3 text-[12.5px]">
                  <span className="flex min-w-0 items-center gap-2">
                    <span className="truncate">
                      {localizedCategoryLabel(t, view.category)}
                    </span>
                    {view.category.risk !== "safe" && (
                      <RiskBadge risk={view.category.risk} />
                    )}
                  </span>
                  <span className="shrink-0 text-muted-foreground tabular-nums">
                    {formatBytes(categoryBytes)}
                  </span>
                </div>
                {groupRows.map((row) => (
                  <div
                    key={row.group.id}
                    className="ml-4 flex items-center justify-between gap-3 text-[11.5px] text-muted-foreground"
                  >
                    <span className="truncate">
                      {localizedGroupLabel(t, row.group)}
                    </span>
                    <span className="shrink-0 tabular-nums">
                      {row.count.toLocaleString()} · {formatBytes(row.bytes)}
                    </span>
                  </div>
                ))}
              </li>
            );
          })}
        </ul>
      </ConfirmDialog>
    </div>
  );
}

interface CategoryCardProps {
  view: CategoryView;
  selected: Set<string>;
  expanded: boolean;
  disabled: boolean;
  onTogglePaths: (paths: string[], checked: boolean) => void;
  onExpand: () => void;
}

function CategoryCard({
  view,
  selected,
  expanded,
  disabled,
  onTogglePaths,
  onExpand,
}: CategoryCardProps) {
  const { category, groups, ungrouped, paths } = view;
  const Icon = CATEGORY_ICONS[category.id];
  const { t } = useTranslation();

  const checked = paths.length > 0 && paths.every((path) => selected.has(path));
  const indeterminate =
    !checked && paths.some((path) => selected.has(path));

  return (
    <Card
      className={cn(
        "overflow-hidden transition-all duration-200",
        checked ? "border-primary/40 bg-primary/[0.04]" : "hover:border-border",
      )}
    >
      <div className="flex items-center gap-3.5 px-4 py-3.5">
        <Checkbox
          checked={checked}
          indeterminate={indeterminate}
          onChange={(next) => onTogglePaths(paths, next)}
          disabled={disabled}
          label={t("deepClean.selectCategoryAria", {
            name: localizedCategoryLabel(t, category),
          })}
        />

        <span
          className={cn(
            "flex size-10 shrink-0 items-center justify-center rounded-xl",
            RISK_STYLE[category.risk],
          )}
        >
          <Icon className="size-[19px]" strokeWidth={1.9} />
        </span>

        <div className="flex min-w-0 flex-1 items-center gap-3">
          <div className="min-w-0 flex-1">
            <div className="flex items-center gap-2">
              {/* Checkbox 的 label 仅作 aria-label，可见标题需显式渲染 */}
              <span
                className="cursor-pointer select-none truncate text-[13.5px] font-semibold"
                onClick={() => {
                  if (!disabled) onTogglePaths(paths, !checked);
                }}
              >
                {localizedCategoryLabel(t, category)}
              </span>
              <RiskBadge risk={category.risk} />
            </div>
            <p className="mt-0.5 truncate text-[12px] text-muted-foreground">
              {localizedCategoryDesc(t, category)}
            </p>
          </div>

          <div className="shrink-0 text-right">
            <span className="block text-[15px] font-semibold tabular-nums">
              {formatBytes(category.totalSize)}
            </span>
            <span className="block text-[11.5px] text-muted-foreground tabular-nums">
              {category.itemCount.toLocaleString()} {t("common.files")}
            </span>
          </div>
        </div>

        <button
          type="button"
          onClick={onExpand}
          disabled={disabled}
          aria-label={t("deepClean.expandCategoryAria", {
            name: localizedCategoryLabel(t, category),
          })}
          className="flex size-7 shrink-0 cursor-pointer items-center justify-center rounded-md text-muted-foreground transition-colors hover:bg-muted hover:text-foreground"
        >
          <ChevronRight
            className={cn(
              "size-4 transition-transform duration-200",
              expanded && "rotate-90",
            )}
          />
        </button>
      </div>

      {expanded && (
        <div className="animate-fade-in border-t border-border bg-muted/30">
          {groups.map((group) => (
            <GroupBlock
              key={group.group.id}
              group={group}
              selected={selected}
              disabled={disabled}
              onTogglePaths={onTogglePaths}
            />
          ))}
          {ungrouped.map((item) => (
            <CleanableItemRow
              key={item.id}
              item={item}
              checked={selected.has(item.path)}
              disabled={disabled}
              onTogglePaths={onTogglePaths}
              className="first:border-t-0"
            />
          ))}
        </div>
      )}
    </Card>
  );
}

/** Rows rendered per group before the "show all" expander kicks in. */
const GROUP_RENDER_LIMIT = 50;

/** Open System Settings on the Full Disk Access pane (mirrors the Layout
 *  banner's handler, so both entry points behave identically). */
async function openFdaSettings() {
  try {
    await openFullDiskAccessSettings();
  } catch (err) {
    console.error("[DeepClean] Failed to open System Settings:", err);
  }
}

function GroupBlock({
  group,
  selected,
  disabled,
  onTogglePaths,
}: {
  group: GroupView;
  selected: Set<string>;
  disabled: boolean;
  onTogglePaths: (paths: string[], checked: boolean) => void;
}) {
  const { t } = useTranslation();
  // Without Full Disk Access, read_dir on /private/var/log fails silently,
  // so that whole source just vanishes from the system-logs group. Surface
  // the gap right where those items would have been listed. The selector
  // returns a boolean, and CleanableItemRow stays memoized — this only
  // re-renders GroupBlock itself.
  const missingFda = useAppStore(
    (state) => !state.permissions.fullDiskAccess,
  );
  const showFdaHint =
    missingFda &&
    group.group.id === "system-logs" &&
    !group.items.some((item) => item.path.startsWith("/private/var/log"));

  // Large groups (system-logs can list ~1000 archived files) render a slice
  // first; the expander swaps in the full list on demand. The slice resets
  // whenever a fresh scan delivers new item references.
  const [showAll, setShowAll] = useState(false);
  useEffect(() => {
    setShowAll(false);
  }, [group.items]);

  const paths = group.items.map((item) => item.path);
  const checked = paths.every((path) => selected.has(path));
  const indeterminate =
    !checked && paths.some((path) => selected.has(path));
  const visibleItems = showAll
    ? group.items
    : group.items.slice(0, GROUP_RENDER_LIMIT);

  return (
    <div className="border-t border-border/60 first:border-t-0">
      <div className="flex items-center justify-between gap-3 px-4 py-2">
        <div className="flex min-w-0 items-center gap-2.5">
          <Checkbox
            checked={checked}
            indeterminate={indeterminate}
            onChange={(next) => onTogglePaths(paths, next)}
            disabled={disabled}
            label={t("deepClean.selectGroupAria", {
              name: localizedGroupLabel(t, group.group),
            })}
          />
          {/* Checkbox 的 label 仅作 aria-label，可见标题需显式渲染 */}
          <span
            className="cursor-pointer select-none truncate text-[12.5px] font-semibold text-foreground/90"
            onClick={() => {
              if (!disabled) onTogglePaths(paths, !checked);
            }}
          >
            {localizedGroupLabel(t, group.group)}
          </span>
        </div>
        <Badge tone="neutral">
          {group.items.length} · {formatBytes(group.totalSize)}
        </Badge>
      </div>

      {showFdaHint && (
        <div className="flex items-center justify-between gap-3 border-t border-warning/25 bg-warning/8 px-4 py-2 pl-11">
          <span className="flex min-w-0 items-center gap-2 text-[12px] font-medium text-warning">
            <ShieldAlert className="size-3.5 shrink-0" />
            {t("deepClean.fdaHint")}
          </span>
          <Button
            variant="outline"
            size="sm"
            className="h-6 shrink-0 px-2.5 text-[11.5px]"
            onClick={() => void openFdaSettings()}
          >
            {t("permission.openSettings")}
          </Button>
        </div>
      )}

      {visibleItems.map((item) => (
        <CleanableItemRow
          key={item.id}
          item={item}
          checked={selected.has(item.path)}
          disabled={disabled}
          onTogglePaths={onTogglePaths}
          className="pl-11"
        />
      ))}
      {!showAll && group.items.length > GROUP_RENDER_LIMIT && (
        <button
          type="button"
          onClick={() => setShowAll(true)}
          className="flex w-full cursor-pointer items-center gap-1.5 border-t border-border/60 px-4 py-2 pl-11 text-[12px] font-medium text-primary transition-colors hover:bg-muted/60"
        >
          <ChevronDown className="size-3.5" />
          {t("deepClean.showAllItems", {
            count: group.items.length.toLocaleString(),
          })}
        </button>
      )}
    </div>
  );
}

/** Memoized (with a stable `onTogglePaths`) so ticking one row re-renders
 *  only that row instead of every row in a 900+ item group. */
const CleanableItemRow = memo(function CleanableItemRow({
  item,
  checked,
  disabled,
  onTogglePaths,
  className,
}: {
  item: CleanableItem;
  checked: boolean;
  disabled: boolean;
  onTogglePaths: (paths: string[], checked: boolean) => void;
  className?: string;
}) {
  const { t, lang } = useTranslation();
  const locale = lang === "zh-CN" ? "zh-CN" : "en-US";

  return (
    <div
      className={cn(
        "flex items-center gap-3 border-t border-border/60 px-4 py-2",
        className,
      )}
    >
      <Checkbox
        checked={checked}
        onChange={(next) => onTogglePaths([item.path], next)}
        disabled={disabled}
        label={t("deepClean.selectItemAria", { name: item.path })}
      />

      <div className="min-w-0 flex-1">
        <p
          className="truncate font-mono text-[11.5px] text-foreground/80"
          data-selectable
        >
          {truncatePath(item.path, 4)}
        </p>
        <p className="truncate text-[11.5px] text-muted-foreground">
          {item.description}
        </p>
      </div>

      <div className="flex shrink-0 items-center gap-3">
        <Badge tone="neutral">{formatRelativeTime(item.modifiedAt, locale)}</Badge>
        <span className="w-16 text-right text-[12.5px] font-medium tabular-nums">
          {formatBytes(item.size)}
        </span>
      </div>
    </div>
  );
});
