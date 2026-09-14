import { useEffect, useMemo, useRef, useState } from "react";
import {
  Archive,
  Box,
  CircleAlert,
  Disc3,
  Download,
  FileDown,
  Loader2,
  PackageCheck,
  RotateCcw,
  Sparkles,
  Trash2,
  Wand2,
  type LucideIcon,
} from "lucide-react";
import { Button, Card, CardContent, Progress } from "@/components/ui";
import {
  Badge,
  Checkbox,
  ConfirmDialog,
  EmptyState,
  StatTile,
} from "@/components/shared";
import { useTranslation } from "@/hooks/useTranslation";
import { cn, formatBytes, formatRelativeTime } from "@/lib/utils";
import { useAppStore } from "@/stores/appStore";
import { removeInstallers, scanInstallers } from "@/services/installer";
import type { InstallerFile, InstallerKind, InstallerScanResult } from "@/types";

type FilterId = "all" | InstallerKind;

const FILTERS: [id: FilterId, label: string][] = [
  ["all", "All"],
  ["dmg", "DMG"],
  ["pkg", "PKG"],
  ["zip", "ZIP"],
  ["iso", "ISO"],
  ["app-archive", "XIP"],
];

const KIND_META: Record<InstallerKind, { icon: LucideIcon; color: string }> = {
  dmg: { icon: Disc3, color: "#0A84FF" },
  pkg: { icon: Box, color: "#FF9500" },
  zip: { icon: Archive, color: "#5E5CE6" },
  iso: { icon: Disc3, color: "#FF375F" },
  "app-archive": { icon: FileDown, color: "#00C7BE" },
};

export function InstallerCleanup() {
  const status = useAppStore((state) => state.moduleStatus.installer);
  const progress = useAppStore((state) => state.progress);
  const setModuleStatus = useAppStore((state) => state.setModuleStatus);
  const addFreedBytes = useAppStore((state) => state.addFreedBytes);
  const setProgress = useAppStore((state) => state.setProgress);
  const { t } = useTranslation();

  const [result, setResult] = useState<InstallerScanResult | null>(null);
  const [filter, setFilter] = useState<FilterId>("all");
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [confirmOpen, setConfirmOpen] = useState(false);
  const [freedBytes, setFreedBytes] = useState(0);
  const [error, setError] = useState(false);
  const cancelledRef = useRef(false);

  const scanning = status === "scanning";
  const deleting = status === "running";

  const installers = result?.installers ?? [];

  const visible = useMemo(
    () =>
      installers
        .filter((file) => filter === "all" || file.kind === filter)
        .sort((a, b) => b.size - a.size),
    [installers, filter],
  );

  const selectedFiles = installers.filter((file) => selected.has(file.id));
  const selectedSize = selectedFiles.reduce((sum, file) => sum + file.size, 0);
  const redundant = installers.filter((file) => file.isRedundant);
  const allVisibleSelected =
    visible.length > 0 && visible.every((file) => selected.has(file.id));

  const startScanRef = useRef<() => Promise<void>>(async () => {});

  startScanRef.current = async () => {
    cancelledRef.current = false;
    setResult(null);
    setSelected(new Set());
    setFreedBytes(0);
    setError(false);
    setModuleStatus("installer", "scanning");

    try {
      const real = await scanInstallers();
      if (cancelledRef.current) return;
      setResult(real);
      setSelected(
        new Set(
          real.installers
            .filter((file) => file.isRedundant)
            .map((file) => file.id),
        ),
      );
      setModuleStatus("installer", "ready");
    } catch (err) {
      if (cancelledRef.current) return;
      console.error("[InstallerCleanup] scanInstallers failed:", err);
      setProgress(null);
      setModuleStatus("installer", "error");
      setError(true);
    }
  };

  // Downloads and Desktop are cheap to walk, so this module scans on arrival.
  useEffect(() => {
    if (status === "idle") void startScanRef.current();
  }, [status]);

  async function deleteSelected() {
    setConfirmOpen(false);
    const removedIds = new Set(selectedFiles.map((file) => file.id));
    const paths = selectedFiles.map((file) => file.path);

    setModuleStatus("installer", "running");
    try {
      const outcome = await removeInstallers(paths);
      const freed = outcome.freedBytes;

      setResult((current) =>
        current
          ? {
              ...current,
              installers: current.installers.filter(
                (file) => !removedIds.has(file.id),
              ),
              totalSize: Math.max(0, current.totalSize - freed),
              redundantSize: current.installers
                .filter((file) => !removedIds.has(file.id) && file.isRedundant)
                .reduce((sum, file) => sum + file.size, 0),
            }
          : current,
      );
      setSelected(new Set());
      setFreedBytes(freed);
      addFreedBytes(freed);
      setModuleStatus("installer", "done");
    } catch (err) {
      console.error("[InstallerCleanup] removeInstallers failed:", err);
      setProgress(null);
      setModuleStatus("installer", "error");
      setError(true);
    }
  }

  function handleCancel() {
    cancelledRef.current = true;
    setModuleStatus("installer", "idle");
    setProgress(null);
  }

  function toggle(id: string, checked: boolean) {
    setSelected((current) => {
      const next = new Set(current);
      if (checked) next.add(id);
      else next.delete(id);
      return next;
    });
  }

  // Error state
  if (error && !scanning && !deleting) {
    return (
      <Card>
        <EmptyState
          icon={CircleAlert}
          title={t("common.scanFailed")}
          message={t("common.scanFailedMsg")}
          actionLabel={t("common.retry")}
          onAction={() => void startScanRef.current()}
        />
      </Card>
    );
  }

  return (
    <div className="flex flex-col gap-4">
      {/* -------------------------------- Stats ----------------------------- */}
      <div className="grid gap-3 sm:grid-cols-3">
        <StatTile
          icon={Download}
          label={t("installer.installersFound")}
          value={`${installers.length}`}
          hint={t("installer.installersFoundHint")}
        />
        <StatTile
          icon={PackageCheck}
          label={t("installer.alreadyInstalled")}
          value={formatBytes(redundant.reduce((sum, file) => sum + file.size, 0))}
          hint={t("installer.alreadyInstalledHint", { count: redundant.length })}
          accent="hsl(var(--warning))"
        />
        <StatTile
          icon={Trash2}
          label={t("installer.selectedLabel")}
          value={formatBytes(selectedSize)}
          hint={t("installer.selectedHint", { selected: selected.size, total: installers.length })}
          accent="hsl(var(--primary))"
        />
      </div>

      {/* ------------------------------- Toolbar ---------------------------- */}
      <div className="flex flex-wrap items-center justify-between gap-3">
        <div className="flex items-center gap-1 rounded-lg bg-muted p-0.5">
          {FILTERS.map(([id, label]) => {
            const count =
              id === "all"
                ? installers.length
                : installers.filter((file) => file.kind === id).length;

            return (
              <button
                key={id}
                type="button"
                onClick={() => setFilter(id)}
                disabled={count === 0 && id !== "all"}
                className={cn(
                  "flex items-center gap-1.5 rounded-[7px] px-2.5 py-1 text-[12px] font-medium transition-all duration-150 disabled:opacity-40",
                  filter === id
                    ? "bg-card text-foreground shadow-sm"
                    : "text-muted-foreground hover:text-foreground",
                )}
              >
                {id === "all" ? t("installer.filterAll") : label}
                <span className="text-[10.5px] text-muted-foreground tabular-nums">
                  {count}
                </span>
              </button>
            );
          })}
        </div>

        <div className="flex items-center gap-2">
          {redundant.length > 0 && (
            <Button
              variant="ghost"
              onClick={() =>
                setSelected(new Set(redundant.map((file) => file.id)))
              }
              disabled={scanning || deleting}
            >
              <Wand2 className="size-4" />
              {t("installer.selectRedundant")}
            </Button>
          )}

          {scanning ? (
            <Button variant="outline" onClick={handleCancel}>
              {t("common.stop")}
            </Button>
          ) : (
            <Button
              variant="outline"
              onClick={() => void startScanRef.current()}
              disabled={deleting}
            >
              <RotateCcw className="size-4" />
              {t("common.rescan")}
            </Button>
          )}

          <Button
            variant="destructive"
            disabled={selected.size === 0 || scanning || deleting}
            onClick={() => setConfirmOpen(true)}
          >
            {deleting ? (
              <Loader2 className="size-4 animate-spin" />
            ) : (
              <Trash2 className="size-4" />
            )}
            {deleting
              ? t("installer.deleting")
              : `${t("installer.deleteSelected")}${selectedSize > 0 ? ` · ${formatBytes(selectedSize)}` : ""}`}
          </Button>
        </div>
      </div>

      {/* ------------------------------- Progress --------------------------- */}
      {(scanning || deleting) && (
        <Card className="bg-card/70 backdrop-blur-xl">
          <CardContent className="flex flex-col gap-2.5 pt-5">
            <div className="flex items-center justify-between gap-4">
              <span className="flex items-center gap-2.5 text-[13px] font-medium">
                <Loader2 className="size-4 animate-spin text-primary" />
                {scanning
                  ? t("installer.scanning")
                  : t("installer.deleting")}
              </span>
              <span className="text-[13px] font-semibold tabular-nums">
                {formatBytes(progress?.bytesFound ?? 0)}
              </span>
            </div>
            <Progress
              value={progress?.progress != null ? progress.progress * 100 : null}
            />
            <p className="truncate font-mono text-[11px] text-muted-foreground">
              {progress?.currentPath ?? ""}
            </p>
          </CardContent>
        </Card>
      )}

      {/* -------------------------------- Result ---------------------------- */}
      {status === "done" && freedBytes > 0 && (
        <Card className="border-success/30 bg-success/8">
          <CardContent className="flex items-center gap-3.5 pt-5">
            <span className="flex size-10 items-center justify-center rounded-xl bg-success/15 text-success">
              <Sparkles className="size-5" strokeWidth={1.9} />
            </span>
            <div>
              <p className="text-[15px] font-semibold tracking-tight">
                {t("installer.freedResult", { bytes: formatBytes(freedBytes) })}
              </p>
              <p className="text-[12.5px] text-muted-foreground">
                {t("installer.freedDetail")}
              </p>
            </div>
          </CardContent>
        </Card>
      )}

      {/* --------------------------------- List ----------------------------- */}
      {visible.length > 0 ? (
        <Card className="overflow-hidden">
          <div className="flex items-center justify-between gap-4 border-b border-border px-4 py-2.5">
            <label className="flex cursor-pointer items-center gap-2.5">
              <Checkbox
                checked={allVisibleSelected}
                indeterminate={
                  !allVisibleSelected &&
                  visible.some((file) => selected.has(file.id))
                }
                onChange={(checked) =>
                  setSelected((current) => {
                    const next = new Set(current);
                    for (const file of visible) {
                      if (checked) next.add(file.id);
                      else next.delete(file.id);
                    }
                    return next;
                  })
                }
                label={t("installer.selectVisible")}
              />
              <span className="text-[13px] font-medium">
                {t("common.selectAll")}{filter !== "all" ? ` ${t("installer.selectAllVisible")}` : ""}
              </span>
            </label>

            <span className="text-[12.5px] text-muted-foreground tabular-nums">
              {visible.length} {t("common.files")} ·{" "}
              <span className="font-semibold text-foreground">
                {formatBytes(visible.reduce((sum, file) => sum + file.size, 0))}
              </span>
            </span>
          </div>

          <div className="flex flex-col">
            {visible.map((file) => (
              <InstallerRow
                key={file.id}
                file={file}
                checked={selected.has(file.id)}
                disabled={deleting}
                onToggle={(checked) => toggle(file.id, checked)}
              />
            ))}
          </div>
        </Card>
      ) : (
        !scanning && (
          <Card>
            <EmptyState
              icon={Download}
              title={
                installers.length === 0
                  ? t("installer.noInstallersLeft")
                  : t("installer.noFilterMatch", { filter: filter.toUpperCase() })
              }
              message={
                installers.length === 0
                  ? t("installer.noInstallersLeftMsg")
                  : t("installer.noFilterMatchMsg")
              }
              actionLabel={installers.length === 0 ? t("purge.scanAgain") : t("installer.showAll")}
              onAction={() =>
                installers.length === 0
                  ? void startScanRef.current()
                  : setFilter("all")
              }
            />
          </Card>
        )
      )}

      <ConfirmDialog
        open={confirmOpen}
        destructive
        title={t("installer.confirmDelete", { count: selectedFiles.length })}
        message={t("installer.confirmDeleteMsg", { bytes: formatBytes(selectedSize) })}
        confirmLabel={t("installer.deleteNow")}
        onConfirm={deleteSelected}
        onClose={() => setConfirmOpen(false)}
      >
        <div className="flex max-h-44 flex-col gap-1.5 overflow-y-auto rounded-lg bg-muted/60 p-3 text-[12px]">
          {selectedFiles.map((file) => (
            <div key={file.id} className="flex items-center justify-between gap-3">
              <span className="truncate font-mono" data-selectable>
                {file.name}
              </span>
              <span className="shrink-0 text-muted-foreground tabular-nums">
                {formatBytes(file.size)}
              </span>
            </div>
          ))}
        </div>
      </ConfirmDialog>
    </div>
  );
}

interface InstallerRowProps {
  file: InstallerFile;
  checked: boolean;
  disabled: boolean;
  onToggle: (checked: boolean) => void;
}

function InstallerRow({ file, checked, disabled, onToggle }: InstallerRowProps) {
  const { t } = useTranslation();
  const meta = KIND_META[file.kind];
  const Icon = meta.icon;
  const folder = file.path.split("/").slice(-2, -1)[0] ?? "";

  return (
    <label
      className={cn(
        "flex cursor-pointer items-center gap-3.5 border-b border-border/60 px-4 py-2.5 transition-colors duration-150 last:border-0",
        checked ? "bg-primary/[0.04]" : "hover:bg-accent/40",
      )}
    >
      <Checkbox
        checked={checked}
        onChange={onToggle}
        disabled={disabled}
        label={file.name}
      />

      <span
        className="flex size-9 shrink-0 items-center justify-center rounded-lg"
        style={{ backgroundColor: `${meta.color}1F`, color: meta.color }}
      >
        <Icon className="size-[18px]" strokeWidth={1.9} />
      </span>

      <span className="min-w-0 flex-1">
        <span className="flex items-center gap-2">
          <span className="truncate text-[13px] font-semibold">{file.name}</span>
          {file.isRedundant && (
            <Badge tone="warning" className="gap-1">
              <PackageCheck className="size-2.5" />
              {t("installer.matchedAppInstalled", { app: file.matchedApp ?? "" })}
            </Badge>
          )}
        </span>
        <span className="mt-0.5 flex items-center gap-2 text-[11.5px] text-muted-foreground">
          <span className="uppercase">{file.kind.replace("app-archive", "xip")}</span>
          <span>·</span>
          <span>{folder}</span>
          <span>·</span>
          <span>{t("installer.downloadedAt", { time: formatRelativeTime(file.createdAt).toLowerCase() })}</span>
        </span>
      </span>

      <span className="shrink-0 text-right text-[13.5px] font-semibold tabular-nums">
        {formatBytes(file.size)}
      </span>
    </label>
  );
}
