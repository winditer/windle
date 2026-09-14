import { useEffect, useMemo, useRef, useState } from "react";
import {
  Boxes,
  CircleAlert,
  FolderCode,
  FolderTree,
  GitBranch,
  Loader2,
  Plus,
  Radar,
  RotateCcw,
  Sparkles,
  Trash2,
  Weight,
  X,
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
import { onPurgeProgress, purgeArtifacts, scanProjects } from "@/services/purge";
import type { ProjectArtifact, ProjectInfo, ProjectKind, PurgeScanResult, ProgressEvent } from "@/types";

/** UI metadata for each project kind: accent color + display label. */
const PROJECT_KIND_META: Record<ProjectKind, { accent: string; label: string }> = {
  node: { accent: "#5E5CE6", label: "Node.js" },
  rust: { accent: "#FF6B35", label: "Rust" },
  python: { accent: "#0A84FF", label: "Python" },
  go: { accent: "#00C7BE", label: "Go" },
  java: { accent: "#FF9500", label: "Java" },
  xcode: { accent: "#007AFF", label: "Xcode" },
  flutter: { accent: "#34C759", label: "Flutter" },
  unity: { accent: "#AF52DE", label: "Unity" },
  unknown: { accent: "#8E8E93", label: "Other" },
};

/** Default scan roots — mirrors `scanner::default_project_roots()` on the Rust side. */
const DEFAULT_SCAN_ROOTS: string[] = ["~/Documents"];

/** localStorage key for persisting custom scan roots. */
const SCAN_ROOTS_KEY = "windle:purge:scan-roots";

function loadScanRoots(): string[] {
  try {
    const stored = localStorage.getItem(SCAN_ROOTS_KEY);
    if (stored) {
      const parsed = JSON.parse(stored);
      if (Array.isArray(parsed) && parsed.every((s) => typeof s === "string")) {
        return parsed.length > 0 ? parsed : DEFAULT_SCAN_ROOTS;
      }
    }
  } catch {
    // Ignore — fall back to defaults.
  }
  return DEFAULT_SCAN_ROOTS;
}

function saveScanRoots(roots: string[]): void {
  try {
    localStorage.setItem(SCAN_ROOTS_KEY, JSON.stringify(roots));
  } catch {
    // Ignore — persistence is best-effort.
  }
}

/** One table row: an artifact plus the project it belongs to. */
interface ArtifactRow {
  project: ProjectInfo;
  artifact: ProjectArtifact;
}

type GroupMode = "size" | "type";

export function ProjectPurge() {
  const status = useAppStore((state) => state.moduleStatus.purge);
  const progress = useAppStore((state) => state.progress);
  const setProgress = useAppStore((state) => state.setProgress);
  const setModuleStatus = useAppStore((state) => state.setModuleStatus);
  const addFreedBytes = useAppStore((state) => state.addFreedBytes);
  const { t } = useTranslation();

  const [result, setResult] = useState<PurgeScanResult | null>(null);
  const [scanRoots, setScanRoots] = useState<string[]>(loadScanRoots);
  const [newPath, setNewPath] = useState("");
  const [addingPath, setAddingPath] = useState(false);
  const [pathError, setPathError] = useState<string | null>(null);
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [groupMode, setGroupMode] = useState<GroupMode>("size");
  const [confirmOpen, setConfirmOpen] = useState(false);
  const [freedBytes, setFreedBytes] = useState(0);
  const [error, setError] = useState(false);
  const cancelledRef = useRef(false);

  // Persist custom scan roots so they survive app restarts.
  useEffect(() => {
    saveScanRoots(scanRoots);
  }, [scanRoots]);

  // Roots the user configured that the backend did not scan (non-existent / not a directory).
  const invalidRoots = useMemo(() => {
    if (!result) return [];
    return scanRoots.filter((root) => !result.scannedRoots.includes(root));
  }, [result, scanRoots]);

  // Subscribe to `purge://progress` events for real-time scan/purge progress.
  useEffect(() => {
    let unlisten: (() => void) | null = null;
    let cancelled = false;
    onPurgeProgress((event: ProgressEvent) => {
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

  const scanning = status === "scanning";
  const purging = status === "running";

  const rows = useMemo<ArtifactRow[]>(() => {
    const flat = (result?.projects ?? []).flatMap((project) =>
      project.artifacts.map((artifact) => ({ project, artifact })),
    );

    return flat.sort((a, b) =>
      groupMode === "type"
        ? a.project.kind.localeCompare(b.project.kind) ||
          b.artifact.size - a.artifact.size
        : b.artifact.size - a.artifact.size,
    );
  }, [result, groupMode]);

  const selectedRows = rows.filter((row) => selected.has(row.artifact.path));
  const selectedSize = selectedRows.reduce(
    (sum, row) => sum + row.artifact.size,
    0,
  );
  const totalSize = rows.reduce((sum, row) => sum + row.artifact.size, 0);
  const allSelected = rows.length > 0 && selected.size === rows.length;

  async function startScan() {
    cancelledRef.current = false;
    setResult(null);
    setSelected(new Set());
    setFreedBytes(0);
    setError(false);

    setModuleStatus("purge", "scanning");
    try {
      const realResult = await scanProjects(scanRoots.length > 0 ? scanRoots : undefined);
      if (cancelledRef.current) return;
      setResult(realResult);
      setSelected(
        new Set(
          realResult.projects
            .filter((project) => project.hasRemote)
            .flatMap((project) => project.artifacts.map((artifact) => artifact.path)),
        ),
      );
      setModuleStatus("purge", "ready");
    } catch (err) {
      if (cancelledRef.current) return;
      console.error("[ProjectPurge] scanProjects failed:", err);
      setProgress(null);
      setModuleStatus("purge", "error");
      setError(true);
    }
  }

  async function purge() {
    setConfirmOpen(false);
    const bytes = selectedSize;
    const purgedPaths = new Set(selectedRows.map((row) => row.artifact.path));

    setModuleStatus("purge", "running");
    try {
      await purgeArtifacts([...purgedPaths]);
      setResult((current) =>
        current
          ? {
              ...current,
              projects: current.projects
                .map((project) => ({
                  ...project,
                  artifacts: project.artifacts.filter(
                    (artifact) => !purgedPaths.has(artifact.path),
                  ),
                }))
                .filter((project) => project.artifacts.length > 0)
                .map((project) => ({
                  ...project,
                  reclaimableSize: project.artifacts.reduce(
                    (sum, artifact) => sum + artifact.size,
                    0,
                  ),
                })),
              totalReclaimable: current.totalReclaimable - bytes,
            }
          : current,
      );
      setSelected(new Set());
      setFreedBytes(bytes);
      addFreedBytes(bytes);
      setModuleStatus("purge", "done");
    } catch (err) {
      console.error("[ProjectPurge] purgeArtifacts failed:", err);
      setProgress(null);
      setModuleStatus("purge", "error");
      setError(true);
    }
  }

  function handleCancel() {
    cancelledRef.current = true;
    setModuleStatus("purge", "idle");
    setProgress(null);
  }

  function toggleRow(path: string, checked: boolean) {
    setSelected((current) => {
      const next = new Set(current);
      if (checked) next.add(path);
      else next.delete(path);
      return next;
    });
  }

  function addPath() {
    const trimmed = newPath.trim();
    if (!trimmed) return;
    if (scanRoots.includes(trimmed)) {
      setPathError(t("purge.pathExists"));
      return;
    }
    setScanRoots((current) => [...current, trimmed]);
    setNewPath("");
    setAddingPath(false);
    setPathError(null);
  }

  function removePath(path: string) {
    setScanRoots((current) => current.filter((p) => p !== path));
  }

  function resetToDefaults() {
    setScanRoots(DEFAULT_SCAN_ROOTS);
    setAddingPath(false);
    setNewPath("");
    setPathError(null);
  }

  // Error state
  if (error && !scanning && !purging) {
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
      {/* ----------------------- Scan Paths Panel ------------------------- */}
      <Card>
        <CardContent className="flex flex-col gap-3 pt-5">
          <div className="flex items-center justify-between gap-3">
            <span className="text-[13px] font-semibold">
              {t("purge.customPaths")}
            </span>
            <div className="flex items-center gap-2">
              <Button
                variant="ghost"
                size="sm"
                onClick={resetToDefaults}
                disabled={scanning || purging}
              >
                <RotateCcw className="size-3.5" />
                {t("purge.resetDefaults")}
              </Button>
              {!addingPath && (
                <Button
                  variant="outline"
                  size="sm"
                  onClick={() => {
                    setAddingPath(true);
                    setPathError(null);
                  }}
                  disabled={scanning || purging}
                >
                  <Plus className="size-3.5" />
                  {t("purge.addPath")}
                </Button>
              )}
            </div>
          </div>

          {/* Path list */}
          <div className="flex flex-col gap-1.5">
            {scanRoots.map((root) => {
              const isInvalid = invalidRoots.includes(root);
              return (
                <div
                  key={root}
                  className={cn(
                    "flex items-center justify-between gap-3 rounded-lg border px-3 py-1.5",
                    isInvalid
                      ? "border-destructive/30 bg-destructive/5"
                      : "border-border bg-muted/30",
                  )}
                >
                  <span className="flex items-center gap-2 truncate font-mono text-[12px]">
                    {root}
                    {isInvalid && (
                      <span className="text-[11px] text-destructive">
                        {t("purge.pathNotFound")}
                      </span>
                    )}
                  </span>
                  <button
                    type="button"
                    onClick={() => removePath(root)}
                    disabled={scanning || purging}
                    className="text-muted-foreground transition-colors hover:text-destructive disabled:opacity-50"
                  >
                    <X className="size-3.5" />
                  </button>
                </div>
              );
            })}
            {scanRoots.length === 0 && (
              <p className="text-[12px] text-muted-foreground">
                {t("purge.enterPath")}
              </p>
            )}
          </div>

          {/* Add path input */}
          {addingPath && (
            <div className="flex items-center gap-2">
              <input
                type="text"
                value={newPath}
                onChange={(e) => {
                  setNewPath(e.target.value);
                  setPathError(null);
                }}
                onKeyDown={(e) => {
                  if (e.key === "Enter") addPath();
                  if (e.key === "Escape") {
                    setAddingPath(false);
                    setNewPath("");
                    setPathError(null);
                  }
                }}
                placeholder={t("purge.enterPath")}
                autoFocus
                disabled={scanning || purging}
                className="flex-1 rounded-lg border border-border bg-card px-3 py-1.5 font-mono text-[12px] outline-none focus:border-primary/40"
              />
              <Button
                size="sm"
                onClick={addPath}
                disabled={!newPath.trim() || scanning || purging}
              >
                <Plus className="size-3.5" />
                {t("purge.addPath")}
              </Button>
              <Button
                variant="ghost"
                size="sm"
                onClick={() => {
                  setAddingPath(false);
                  setNewPath("");
                  setPathError(null);
                }}
              >
                {t("common.cancel")}
              </Button>
            </div>
          )}
          {pathError && (
            <p className="text-[12px] text-destructive">{pathError}</p>
          )}
          {result && result.scannedRoots.length > 0 && (
            <p className="text-[11px] text-muted-foreground">
              {t("purge.scannedRoots")}: {result.scannedRoots.join(" · ")}
            </p>
          )}
        </CardContent>
      </Card>

      {/* ------------------------------- Toolbar ---------------------------- */}
      <div className="flex flex-wrap items-center justify-end gap-2">
        {scanning ? (
          <Button variant="outline" onClick={handleCancel}>
            {t("common.stop")}
          </Button>
        ) : (
          <Button
            variant="outline"
            onClick={startScan}
            disabled={purging || scanRoots.length === 0}
          >
            {result ? <RotateCcw className="size-4" /> : <Radar className="size-4" />}
            {result ? t("common.rescan") : t("purge.scan")}
          </Button>
        )}

        <Button
          variant="destructive"
          disabled={selected.size === 0 || scanning || purging}
          onClick={() => setConfirmOpen(true)}
        >
          {purging ? (
            <Loader2 className="size-4 animate-spin" />
          ) : (
            <Trash2 className="size-4" />
          )}
          {purging
            ? t("purge.purging")
            : `${t("purge.purgeSelected")}${selectedSize > 0 ? ` · ${formatBytes(selectedSize)}` : ""}`}
        </Button>
      </div>

      {/* -------------------------------- Stats ----------------------------- */}
      {result && (
        <div className="grid gap-3 sm:grid-cols-3">
          <StatTile
            icon={FolderCode}
            label={t("purge.projects")}
            value={`${result.projects.length}`}
            hint={t("purge.projectsHint", { count: rows.length })}
          />
          <StatTile
            icon={Weight}
            label={t("purge.reclaimable")}
            value={formatBytes(totalSize)}
            hint={t("purge.scannedAt", { time: formatRelativeTime(result.scannedAt).toLowerCase() })}
            accent="hsl(var(--warning))"
          />
          <StatTile
            icon={Boxes}
            label={t("purge.selectedLabel")}
            value={formatBytes(selectedSize)}
            hint={t("purge.selectedHint", { selected: selected.size, total: rows.length })}
            accent="hsl(var(--primary))"
          />
        </div>
      )}

      {/* ------------------------------- Scanning --------------------------- */}
      {(scanning || purging) && (
        <Card className="bg-card/70 backdrop-blur-xl">
          <CardContent className="flex flex-col gap-2.5 pt-5">
            <div className="flex items-center justify-between gap-4">
              <span className="flex items-center gap-2.5 text-[13px] font-medium">
                <Loader2 className="size-4 animate-spin text-primary" />
                {scanning ? t("purge.lookingForArtifacts") : t("purge.removingArtifacts")}
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
          <CardContent className="flex flex-wrap items-center justify-between gap-4 pt-5">
            <div className="flex items-center gap-3.5">
              <span className="flex size-10 items-center justify-center rounded-xl bg-success/15 text-success">
                <Sparkles className="size-5" strokeWidth={1.9} />
              </span>
              <div>
                <p className="text-[15px] font-semibold tracking-tight">
                  {t("purge.purgedResult", { bytes: formatBytes(freedBytes) })}
                </p>
                <p className="text-[12.5px] text-muted-foreground">
                  {t("purge.purgedDetail")}
                </p>
              </div>
            </div>
          </CardContent>
        </Card>
      )}

      {/* -------------------------------- Table ----------------------------- */}
      {rows.length > 0 ? (
        <Card className="overflow-hidden">
          <div className="flex items-center justify-between gap-4 border-b border-border px-4 py-2.5">
            <label className="flex cursor-pointer items-center gap-2.5">
              <Checkbox
                checked={allSelected}
                indeterminate={selected.size > 0 && !allSelected}
                onChange={(checked) =>
                  setSelected(
                    checked
                      ? new Set(rows.map((row) => row.artifact.path))
                      : new Set(),
                  )
                }
                label="Select all artifacts"
              />
              <span className="text-[13px] font-medium">{t("common.selectAll")}</span>
            </label>

            <div className="flex items-center gap-1 rounded-lg bg-muted p-0.5">
              {(
                [
                  ["size", t("purge.bySize"), Weight],
                  ["type", t("purge.byType"), FolderTree],
                ] as [GroupMode, string, typeof Weight][]
              ).map(([mode, label, Icon]) => (
                <button
                  key={mode}
                  type="button"
                  onClick={() => setGroupMode(mode)}
                  className={cn(
                    "flex items-center gap-1.5 rounded-[7px] px-2.5 py-1 text-[12px] font-medium transition-all duration-150",
                    groupMode === mode
                      ? "bg-card text-foreground shadow-sm"
                      : "text-muted-foreground hover:text-foreground",
                  )}
                >
                  <Icon className="size-3.5" />
                  {label}
                </button>
              ))}
            </div>
          </div>

          <div className="grid grid-cols-[18px_1fr_96px_150px_84px] items-center gap-3 border-b border-border bg-muted/30 px-4 py-2 text-[11px] font-semibold tracking-wide text-muted-foreground uppercase">
            <span />
            <span>{t("purge.project")}</span>
            <span>{t("common.type")}</span>
            <span>{t("purge.artifactCol")}</span>
            <span className="text-right">{t("common.size")}</span>
          </div>

          <div className="flex flex-col">
            {rows.map((row, index) => {
              const previous = rows[index - 1];
              const showGroupHeader =
                groupMode === "type" &&
                (!previous || previous.project.kind !== row.project.kind);

              return (
                <div key={row.artifact.path}>
                  {showGroupHeader && (
                    <GroupHeader
                      kind={row.project.kind}
                      count={
                        rows.filter((entry) => entry.project.kind === row.project.kind)
                          .length
                      }
                    />
                  )}
                  <ArtifactRowView
                    row={row}
                    checked={selected.has(row.artifact.path)}
                    disabled={purging}
                    onToggle={(checked) => toggleRow(row.artifact.path, checked)}
                  />
                </div>
              );
            })}
          </div>
        </Card>
      ) : (
        !scanning &&
        !purging && (
          <Card>
            <EmptyState
              icon={FolderCode}
              title={result ? t("purge.nothingLeft") : t("purge.findHeavyOutput")}
              message={
                result
                  ? t("purge.nothingLeftMsg")
                  : t("purge.findHeavyMsg")
              }
              actionLabel={result ? t("purge.scanAgain") : t("purge.scanProjects")}
              onAction={startScan}
            />
          </Card>
        )
      )}

      <ConfirmDialog
        open={confirmOpen}
        destructive
        title={t("purge.confirmPurge", { bytes: formatBytes(selectedSize) })}
        message={t("purge.confirmPurgeMsg", { count: selectedRows.length, projects: new Set(selectedRows.map((row) => row.project.id)).size })}
        confirmLabel={t("purge.purgeNow")}
        onConfirm={purge}
        onClose={() => setConfirmOpen(false)}
      >
        <div className="flex max-h-44 flex-col gap-1.5 overflow-y-auto rounded-lg bg-muted/60 p-3 text-[12px]">
          {selectedRows.map((row) => (
            <div
              key={row.artifact.path}
              className="flex items-center justify-between gap-3"
            >
              <span className="truncate">
                <span className="font-medium">{row.project.name}</span>
                <span className="text-muted-foreground">
                  {" "}
                  / {row.artifact.label}
                </span>
                {!row.project.hasRemote && (
                  <span className="ml-1.5 text-warning">{t("purge.noRemote")}</span>
                )}
              </span>
              <span className="shrink-0 text-muted-foreground tabular-nums">
                {formatBytes(row.artifact.size)}
              </span>
            </div>
          ))}
        </div>
      </ConfirmDialog>
    </div>
  );
}

function GroupHeader({ kind, count }: { kind: ProjectKind; count: number }) {
  const { t } = useTranslation();
  const meta = PROJECT_KIND_META[kind];

  return (
    <div className="flex items-center gap-2 border-b border-border bg-muted/40 px-4 py-1.5">
      <span
        className="size-2 rounded-full"
        style={{ backgroundColor: meta.accent }}
      />
      <span className="text-[11.5px] font-semibold">{meta.label}</span>
      <span className="text-[11px] text-muted-foreground tabular-nums">
        {t("purge.artifactsCount", { count })}
      </span>
    </div>
  );
}

interface ArtifactRowViewProps {
  row: ArtifactRow;
  checked: boolean;
  disabled: boolean;
  onToggle: (checked: boolean) => void;
}

function ArtifactRowView({
  row,
  checked,
  disabled,
  onToggle,
}: ArtifactRowViewProps) {
  const { t } = useTranslation();
  const { project, artifact } = row;
  const meta = PROJECT_KIND_META[project.kind];

  return (
    <label
      className={cn(
        "grid cursor-pointer grid-cols-[18px_1fr_96px_150px_84px] items-center gap-3 border-b border-border/60 px-4 py-2.5 transition-colors duration-150 last:border-0",
        checked ? "bg-primary/[0.04]" : "hover:bg-accent/40",
      )}
    >
      <Checkbox
        checked={checked}
        onChange={onToggle}
        disabled={disabled}
        label={artifact.path}
      />

      <span className="min-w-0">
        <span className="flex items-center gap-2">
          <span className="truncate text-[13px] font-semibold">
            {project.name}
          </span>
          {project.hasRemote ? (
            <Badge tone="success" className="gap-1">
              <GitBranch className="size-2.5" />
              {t("uninstall.remote")}
            </Badge>
          ) : (
            <Badge tone="warning">{t("uninstall.localOnly")}</Badge>
          )}
        </span>
        <span
          className="mt-0.5 block truncate font-mono text-[11px] text-muted-foreground"
          data-selectable
        >
          {project.path.replace(/^\/Users\/[^/]+/, "~")}
        </span>
      </span>

      <span
        className="flex w-fit items-center gap-1.5 rounded-full px-2 py-0.5 text-[11px] font-semibold"
        style={{ backgroundColor: `${meta.accent}1F`, color: meta.accent }}
      >
        <span
          className="size-1.5 rounded-full"
          style={{ backgroundColor: meta.accent }}
        />
        {meta.label}
      </span>

      <span className="min-w-0">
        <span className="block truncate font-mono text-[12px] font-medium">
          {artifact.label}
        </span>
        <span className="block text-[11px] text-muted-foreground tabular-nums">
          {artifact.fileCount.toLocaleString()} {t("common.files")}
        </span>
      </span>

      <span className="text-right text-[13px] font-semibold tabular-nums">
        {formatBytes(artifact.size)}
      </span>
    </label>
  );
}
