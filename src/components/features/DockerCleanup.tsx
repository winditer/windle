import {
  useEffect,
  useMemo,
  useRef,
  useState,
  type ReactNode,
} from "react";
import {
  Box,
  CircleAlert,
  Database,
  Hammer,
  Image,
  Layers,
  Loader2,
  Play,
  RotateCcw,
  Ship,
  Trash2,
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
import { formatBytes, formatRelativeTime } from "@/lib/utils";
import { useAppStore } from "@/stores/appStore";
import {
  cleanDocker,
  getDockerStatus,
  onDockerProgress,
  scanDocker,
  startDockerDesktop,
} from "@/services/docker";
import type { TranslationKey } from "@/i18n/translations";
import type {
  CleanOutcome,
  DockerCleanRequest,
  DockerScanReport,
  DockerStatus,
  ProgressEvent,
} from "@/types";

/** Build-cache window quick picks: UI key → prune `until` + hours. */
const CACHE_WINDOWS = [
  { key: "all", until: "0s", hours: 0 },
  { key: "72h", until: "72h", hours: 72 },
  { key: "7d", until: "168h", hours: 168 },
  { key: "30d", until: "720h", hours: 720 },
] as const;

type CacheWindowKey = (typeof CACHE_WINDOWS)[number]["key"];

/** Category filter tabs for the reclaimable list. */
type CategoryTab = "all" | "images" | "containers" | "volumes" | "cache";

const CACHE_WINDOW_LABEL: Record<CacheWindowKey, TranslationKey> = {
  all: "docker.cacheWindow.all",
  "72h": "docker.cacheWindow.72h",
  "7d": "docker.cacheWindow.7d",
  "30d": "docker.cacheWindow.30d",
};

function windowOf(key: CacheWindowKey) {
  return CACHE_WINDOWS.find((w) => w.key === key) ?? CACHE_WINDOWS[0];
}

function toggle<T>(set: Set<T>, id: T, enabled: boolean): Set<T> {
  const next = new Set(set);
  if (enabled) {
    next.add(id);
  } else {
    next.delete(id);
  }
  return next;
}

function GroupCard({
  title,
  count,
  bytes,
  checked,
  indeterminate,
  onToggle,
  disabled,
  danger,
  children,
}: {
  title: string;
  count: number;
  bytes: number;
  checked: boolean;
  indeterminate?: boolean;
  onToggle: (checked: boolean) => void;
  /** Header without a checkbox (informational groups, e.g. blocked images). */
  disabled?: boolean;
  /** Red badge for high-risk groups (volumes). */
  danger?: boolean;
  children: ReactNode;
}) {
  return (
    <Card className="bg-card/70 backdrop-blur-xl">
      <CardContent className="flex flex-col gap-2 pt-5">
        <div className="flex items-center justify-between gap-3">
          {disabled ? (
            <span className="text-sm font-medium">{title}</span>
          ) : (
            <div className="flex items-center gap-2.5">
              <Checkbox
                checked={checked}
                indeterminate={indeterminate}
                onChange={onToggle}
                label={title}
              />
              {/* Checkbox 的 label 仅作 aria-label，可见标题需显式渲染 */}
              <span
                className="cursor-pointer select-none text-sm font-medium"
                onClick={() => onToggle(!checked)}
              >
                {title}
              </span>
            </div>
          )}
          <Badge tone={danger ? "danger" : "neutral"}>
            {count} · {formatBytes(bytes)}
          </Badge>
        </div>
        {children}
      </CardContent>
    </Card>
  );
}

function ItemRow({
  checked,
  onToggle,
  disabled,
  title,
  subtitle,
  meta,
  right,
}: {
  checked?: boolean;
  onToggle?: (checked: boolean) => void;
  disabled?: boolean;
  title: string;
  subtitle?: string;
  meta?: ReactNode;
  right?: ReactNode;
}) {
  return (
    <div className="flex items-center gap-3 border-t border-border py-2 first:border-t-0">
      {onToggle ? (
        <Checkbox
          checked={checked ?? false}
          onChange={onToggle}
          disabled={disabled}
        />
      ) : null}
      <div className="min-w-0 flex-1">
        <div className="truncate text-[13px] font-medium">{title}</div>
        {subtitle ? (
          <div className="truncate text-xs text-muted-foreground">{subtitle}</div>
        ) : null}
      </div>
      {meta}
      {right}
    </div>
  );
}

export function DockerCleanup() {
  const status = useAppStore((state) => state.moduleStatus.docker);
  const progress = useAppStore((state) => state.progress);
  const setProgress = useAppStore((state) => state.setProgress);
  const setModuleStatus = useAppStore((state) => state.setModuleStatus);
  const addFreedBytes = useAppStore((state) => state.addFreedBytes);
  const { t } = useTranslation();

  const [dockerStatus, setDockerStatus] = useState<DockerStatus | null>(null);
  const [detecting, setDetecting] = useState(true);
  const [starting, setStarting] = useState(false);
  const [startTimedOut, setStartTimedOut] = useState(false);
  const [report, setReport] = useState<DockerScanReport | null>(null);
  const [selectedImages, setSelectedImages] = useState<Set<string>>(new Set());
  const [selectedContainers, setSelectedContainers] = useState<Set<string>>(
    new Set(),
  );
  const [selectedVolumes, setSelectedVolumes] = useState<Set<string>>(new Set());
  const [cacheEnabled, setCacheEnabled] = useState(true);
  const [cacheWindow, setCacheWindow] = useState<CacheWindowKey>("all");
  const [activeTab, setActiveTab] = useState<CategoryTab>("all");
  const [confirmOpen, setConfirmOpen] = useState(false);
  const [outcome, setOutcome] = useState<CleanOutcome | null>(null);
  const pollRef = useRef<ReturnType<typeof setInterval> | null>(null);

  // Subscribe to `docker://progress` so the progress bar tracks real work.
  useEffect(() => {
    let unlisten: (() => void) | null = null;
    let cancelled = false;
    onDockerProgress((event: ProgressEvent) => {
      if (cancelled) return;
      setProgress({
        progress: event.progress,
        currentPath: event.currentPath,
        itemsScanned: event.itemsScanned,
        bytesFound: event.bytesFound,
      });
    })
      .then((fn) => {
        if (cancelled) {
          fn();
        } else {
          unlisten = fn;
        }
      })
      .catch(() => {});
    return () => {
      cancelled = true;
      if (unlisten) unlisten();
    };
  }, [setProgress]);

  function stopPolling() {
    if (pollRef.current) {
      clearInterval(pollRef.current);
      pollRef.current = null;
    }
  }

  async function detect() {
    setDetecting(true);
    setStartTimedOut(false);
    try {
      const next = await getDockerStatus();
      setDockerStatus(next);
      if (next.state === "ready") {
        await startScan();
      }
    } catch {
      setDockerStatus(null);
    } finally {
      setDetecting(false);
    }
  }

  // Detect on mount; clear any launch poll on unmount.
  useEffect(() => {
    void detect();
    return stopPolling;
  }, []);

  async function startScan() {
    setOutcome(null);
    setModuleStatus("docker", "scanning");
    try {
      const next = await scanDocker();
      setReport(next);
      // 默认勾选：悬空镜像 + 已停止容器 + 构建缓存；未使用镜像/卷不勾
      setSelectedImages(
        new Set(
          next.images.filter((i) => i.kind === "dangling").map((i) => i.fullId),
        ),
      );
      setSelectedContainers(new Set(next.stoppedContainers.map((c) => c.id)));
      setSelectedVolumes(new Set());
      setCacheEnabled(true);
      setCacheWindow("all");
      setActiveTab("all");
      setModuleStatus("docker", "ready");
    } catch {
      setModuleStatus("docker", "error");
    }
  }

  async function handleStartDocker() {
    setStarting(true);
    setStartTimedOut(false);
    try {
      await startDockerDesktop();
    } catch {
      setStarting(false);
      return;
    }
    const deadline = Date.now() + 90_000;
    pollRef.current = setInterval(() => {
      if (Date.now() > deadline) {
        stopPolling();
        setStarting(false);
        setStartTimedOut(true);
        return;
      }
      void getDockerStatus()
        .then((next) => {
          if (next.state === "ready") {
            stopPolling();
            setDockerStatus(next);
            setStarting(false);
            void startScan();
          }
        })
        .catch(() => {
          /* daemon 未就绪，继续轮询 */
        });
    }, 2000);
  }

  const scanning = status === "scanning";
  const cleaning = status === "running";
  const done = status === "done" && outcome != null;

  const images = report?.images ?? [];
  const dangling = images.filter((i) => i.kind === "dangling");
  const unusedImages = images.filter((i) => i.kind === "unused");
  const blockedImages = images.filter(
    (i) => i.kind === "blocked-by-stopped-container",
  );
  const stoppedContainers = report?.stoppedContainers ?? [];
  const unusedVolumes = report?.unusedVolumes ?? [];

  const windowCache = useMemo(() => {
    const w = windowOf(cacheWindow);
    const now = Date.now();
    return (report?.buildCache ?? []).filter(
      (b) =>
        !b.inUse &&
        (w.hours === 0 ||
          b.lastUsedAt == null ||
          now - b.lastUsedAt >= w.hours * 3_600_000),
    );
  }, [report, cacheWindow]);

  const selectedBytes =
    images
      .filter((i) => selectedImages.has(i.fullId))
      .reduce((sum, i) => sum + i.sizeBytes, 0) +
    stoppedContainers
      .filter((c) => selectedContainers.has(c.id))
      .reduce((sum, c) => sum + c.sizeBytes, 0) +
    unusedVolumes
      .filter((v) => selectedVolumes.has(v.name))
      .reduce((sum, v) => sum + v.sizeBytes, 0) +
    (cacheEnabled
      ? windowCache.reduce((sum, b) => sum + b.sizeBytes, 0)
      : 0);

  const selectedCount =
    selectedImages.size +
    selectedContainers.size +
    selectedVolumes.size +
    (cacheEnabled ? windowCache.length : 0);

  const request: DockerCleanRequest = {
    imageIds: [...selectedImages],
    containerIds: [...selectedContainers],
    volumeNames: [...selectedVolumes],
    buildCacheUntil:
      cacheEnabled && windowCache.length > 0
        ? windowOf(cacheWindow).until
        : null,
  };

  function confirmClean() {
    setConfirmOpen(false);
    void runClean();
  }

  async function runClean() {
    setModuleStatus("docker", "running");
    try {
      const result = await cleanDocker(request);
      setOutcome(result);
      addFreedBytes(result.freedBytes);
      setModuleStatus("docker", "done");
    } catch {
      setModuleStatus("docker", "error");
    }
  }

  /* ---- 检测中 ---- */
  if (detecting) {
    return (
      <div className="flex flex-1 items-center justify-center">
        <div className="flex items-center gap-3 text-muted-foreground">
          <Loader2 className="size-5 animate-spin" />
          <span className="text-sm">{t("docker.detecting")}</span>
        </div>
      </div>
    );
  }

  /* ---- 未安装 ---- */
  if (dockerStatus?.state === "not-installed") {
    return (
      <EmptyState
        icon={Ship}
        title={t("docker.notInstalled.title")}
        message={t("docker.notInstalled.message")}
      />
    );
  }

  /* ---- 未运行：一键启动（含轮询 90s 超时兜底） ---- */
  if (dockerStatus?.state !== "ready") {
    return (
      <Card className="bg-card/70 backdrop-blur-xl">
        <CardContent className="flex flex-col items-center gap-4 py-10 text-center">
          <div className="flex size-14 items-center justify-center rounded-2xl bg-primary/10 text-primary">
            <Ship className="size-7" />
          </div>
          <div className="space-y-1">
            <h2 className="text-base font-semibold">
              {t("docker.notRunning.title")}
            </h2>
            <p className="max-w-sm text-[13px] text-muted-foreground">
              {t("docker.notRunning.message")}
            </p>
          </div>
          {startTimedOut ? (
            <p className="text-[13px] text-warning">{t("docker.startTimeout")}</p>
          ) : null}
          <Button onClick={handleStartDocker} disabled={starting}>
            {starting ? <Loader2 className="animate-spin" /> : <Play />}
            {starting ? t("docker.starting") : t("docker.startDocker")}
          </Button>
        </CardContent>
      </Card>
    );
  }

  /* ---- 扫描中 ---- */
  if (scanning) {
    return (
      <Card className="bg-card/70 backdrop-blur-xl">
        <CardContent className="flex flex-col gap-3 pt-5">
          <div className="flex items-center justify-between text-sm">
            <span className="flex items-center gap-2">
              <Loader2 className="size-4 animate-spin text-primary" />
              {t("docker.scanning")}
            </span>
            {dockerStatus?.engineVersion ? (
              <Badge tone="neutral">
                {t("docker.engineVersion", { version: dockerStatus.engineVersion })}
              </Badge>
            ) : null}
          </div>
          <Progress
            value={progress?.progress != null ? progress.progress * 100 : null}
          />
        </CardContent>
      </Card>
    );
  }

  /* ---- 出错 ---- */
  if (status === "error") {
    return (
      <EmptyState
        icon={CircleAlert}
        title={t("docker.scanFailed")}
        message={t("docker.scanFailedMsg")}
        actionLabel={t("common.retry")}
        onAction={startScan}
      />
    );
  }

  /* ---- 清理完成 ---- */
  if (done && outcome) {
    return (
      <Card className="bg-card/70 backdrop-blur-xl">
        <CardContent className="flex flex-col gap-4 pt-5">
          <div className="flex items-center justify-between gap-4">
            <div className="space-y-1">
              <h2 className="text-base font-semibold">{t("docker.allClean")}</h2>
              <p className="text-[13px] text-muted-foreground">
                {t("docker.freedResult", {
                  bytes: formatBytes(outcome.freedBytes),
                })}{" · "}
                {t("docker.removedCount", { count: outcome.removedPaths.length })}
              </p>
              {outcome.failedPaths.length > 0 ? (
                <p className="text-[13px] text-warning">
                  {t("docker.failedCount", { count: outcome.failedPaths.length })}
                </p>
              ) : null}
            </div>
            <Button variant="outline" onClick={() => void startScan()}>
              <RotateCcw />
              {t("docker.rescan")}
            </Button>
          </div>
          {outcome.failedPaths.length > 0 ? (
            <div className="flex flex-col gap-1 rounded-lg border border-border p-3">
              {outcome.failedPaths.slice(0, 10).map((f) => (
                <div
                  key={f.path}
                  className="flex items-center justify-between gap-4 text-xs"
                >
                  <span className="truncate font-mono">{f.path}</span>
                  <span className="shrink-0 text-muted-foreground">{f.reason}</span>
                </div>
              ))}
            </div>
          ) : null}
        </CardContent>
      </Card>
    );
  }

  /* ---- 列表勾选主视图 ---- */
  const nothingReclaimable =
    dangling.length === 0 &&
    unusedImages.length === 0 &&
    stoppedContainers.length === 0 &&
    unusedVolumes.length === 0 &&
    windowCache.length === 0;

  if (nothingReclaimable || !report) {
    return (
      <EmptyState
        icon={Ship}
        title={t("docker.empty.title")}
        message={t("docker.empty.message")}
        actionLabel={t("docker.rescan")}
        onAction={() => void startScan()}
      />
    );
  }

  /* ---- 可清理分类统计（五类明细）与页签筛选 ---- */
  const imagesTabCount = dangling.length + unusedImages.length;

  const reclaimCategories = [
    {
      icon: Layers,
      label: t("docker.danglingImages"),
      count: dangling.length,
      bytes: dangling.reduce((sum, i) => sum + i.sizeBytes, 0),
      accent: "hsl(var(--primary))",
    },
    {
      icon: Image,
      label: t("docker.unusedImages"),
      count: unusedImages.length,
      bytes: unusedImages.reduce((sum, i) => sum + i.sizeBytes, 0),
      accent: "hsl(var(--primary))",
    },
    {
      icon: Box,
      label: t("docker.stoppedContainers"),
      count: stoppedContainers.length,
      bytes: stoppedContainers.reduce((sum, c) => sum + c.sizeBytes, 0),
      accent: "hsl(var(--success))",
    },
    {
      icon: Database,
      label: t("docker.unusedVolumes"),
      count: unusedVolumes.length,
      bytes: unusedVolumes.reduce((sum, v) => sum + v.sizeBytes, 0),
      accent: "hsl(var(--warning))",
    },
    {
      icon: Hammer,
      label: t("docker.buildCache"),
      count: windowCache.length,
      bytes: windowCache.reduce((sum, b) => sum + b.sizeBytes, 0),
      accent: "hsl(var(--destructive))",
    },
  ];

  const totalReclaimable = reclaimCategories.reduce(
    (sum, c) => sum + c.bytes,
    0,
  );
  const totalReclaimableCount = reclaimCategories.reduce(
    (sum, c) => sum + c.count,
    0,
  );

  const categoryTabs: { key: CategoryTab; label: string; count: number }[] = [
    { key: "all", label: t("docker.tabAll"), count: totalReclaimableCount },
    { key: "images", label: t("docker.images"), count: imagesTabCount },
    {
      key: "containers",
      label: t("docker.containers"),
      count: stoppedContainers.length,
    },
    { key: "volumes", label: t("docker.volumes"), count: unusedVolumes.length },
    { key: "cache", label: t("docker.buildCache"), count: windowCache.length },
  ];

  const showImages = activeTab === "all" || activeTab === "images";
  const showContainers = activeTab === "all" || activeTab === "containers";
  const showVolumes = activeTab === "all" || activeTab === "volumes";
  const showCache = activeTab === "all" || activeTab === "cache";

  const activeTabEmpty =
    (activeTab === "images" &&
      imagesTabCount === 0 &&
      blockedImages.length === 0) ||
    (activeTab === "containers" && stoppedContainers.length === 0) ||
    (activeTab === "volumes" && unusedVolumes.length === 0) ||
    (activeTab === "cache" && report.buildCache.length === 0);

  function toggleImageGroup(list: typeof images, checked: boolean) {
    setSelectedImages((prev) => {
      const next = new Set(prev);
      for (const img of list) {
        if (checked) {
          next.add(img.fullId);
        } else {
          next.delete(img.fullId);
        }
      }
      return next;
    });
  }

  return (
    <div className="flex flex-col gap-4">
      {/* 可清理分类统计（五格）+ 实时汇总 */}
      <div className="flex items-center justify-between">
        <h3 className="text-sm font-semibold">{t("docker.reclaimTitle")}</h3>
        <span className="text-[13px] text-muted-foreground">
          {t("docker.reclaimSummary", {
            total: formatBytes(totalReclaimable),
            selected: formatBytes(selectedBytes),
          })}
        </span>
      </div>
      <div className="grid grid-cols-2 gap-3 sm:grid-cols-3 xl:grid-cols-5">
        {reclaimCategories.map((category) => (
          <StatTile
            key={category.label}
            icon={category.icon}
            label={category.label}
            value={formatBytes(category.bytes)}
            hint={t("docker.categoryCount", { count: category.count })}
            accent={category.accent}
          />
        ))}
      </div>

      {/* 类型页签筛选 */}
      <div className="flex flex-wrap items-center gap-2">
        {categoryTabs.map((tab) => (
          <Button
            key={tab.key}
            size="sm"
            variant={activeTab === tab.key ? "default" : "outline"}
            onClick={() => setActiveTab(tab.key)}
          >
            {tab.label}
            <span className="ml-1.5 tabular-nums opacity-70">{tab.count}</span>
          </Button>
        ))}
      </div>

      {/* 1. 悬空镜像（默认勾选） */}
      {showImages && dangling.length > 0 ? (
        <GroupCard
          title={t("docker.danglingImages")}
          count={dangling.length}
          bytes={dangling.reduce((sum, i) => sum + i.sizeBytes, 0)}
          checked={dangling.every((i) => selectedImages.has(i.fullId))}
          indeterminate={
            dangling.some((i) => selectedImages.has(i.fullId)) &&
            !dangling.every((i) => selectedImages.has(i.fullId))
          }
          onToggle={(checked) => toggleImageGroup(dangling, checked)}
        >
          {dangling.map((image) => (
            <ItemRow
              key={image.fullId}
              checked={selectedImages.has(image.fullId)}
              onToggle={(checked) =>
                setSelectedImages((prev) => toggle(prev, image.fullId, checked))
              }
              title={t("docker.untaggedImage", { id: image.id })}
              subtitle={formatRelativeTime(image.createdAt)}
              right={<Badge tone="neutral">{formatBytes(image.sizeBytes)}</Badge>}
            />
          ))}
        </GroupCard>
      ) : null}

      {/* 2. 未使用镜像（默认不勾） */}
      {showImages && unusedImages.length > 0 ? (
        <GroupCard
          title={t("docker.unusedImages")}
          count={unusedImages.length}
          bytes={unusedImages.reduce((sum, i) => sum + i.sizeBytes, 0)}
          checked={unusedImages.every((i) => selectedImages.has(i.fullId))}
          indeterminate={
            unusedImages.some((i) => selectedImages.has(i.fullId)) &&
            !unusedImages.every((i) => selectedImages.has(i.fullId))
          }
          onToggle={(checked) => toggleImageGroup(unusedImages, checked)}
        >
          {unusedImages.map((image) => (
            <ItemRow
              key={image.fullId}
              checked={selectedImages.has(image.fullId)}
              onToggle={(checked) =>
                setSelectedImages((prev) => toggle(prev, image.fullId, checked))
              }
              title={image.repoTags[0] ?? image.id}
              subtitle={`${image.id} · ${formatRelativeTime(image.createdAt)}`}
              right={<Badge tone="neutral">{formatBytes(image.sizeBytes)}</Badge>}
            />
          ))}
        </GroupCard>
      ) : null}

      {/* 3. 已停止容器（默认勾选） */}
      {showContainers && stoppedContainers.length > 0 ? (
        <GroupCard
          title={t("docker.stoppedContainers")}
          count={stoppedContainers.length}
          bytes={stoppedContainers.reduce((sum, c) => sum + c.sizeBytes, 0)}
          checked={stoppedContainers.every((c) => selectedContainers.has(c.id))}
          indeterminate={
            stoppedContainers.some((c) => selectedContainers.has(c.id)) &&
            !stoppedContainers.every((c) => selectedContainers.has(c.id))
          }
          onToggle={(checked) =>
            setSelectedContainers((prev) => {
              const next = new Set(prev);
              for (const c of stoppedContainers) {
                if (checked) {
                  next.add(c.id);
                } else {
                  next.delete(c.id);
                }
              }
              return next;
            })
          }
        >
          {stoppedContainers.map((c) => (
            <ItemRow
              key={c.id}
              checked={selectedContainers.has(c.id)}
              onToggle={(checked) =>
                setSelectedContainers((prev) => toggle(prev, c.id, checked))
              }
              title={c.name}
              subtitle={`${c.imageId} · ${c.statusText}`}
              right={<Badge tone="neutral">{formatBytes(c.sizeBytes)}</Badge>}
            />
          ))}
        </GroupCard>
      ) : null}

      {/* 4. 未使用卷（默认不勾，高危） */}
      {showVolumes && unusedVolumes.length > 0 ? (
        <GroupCard
          title={t("docker.unusedVolumes")}
          count={unusedVolumes.length}
          bytes={unusedVolumes.reduce((sum, v) => sum + v.sizeBytes, 0)}
          checked={unusedVolumes.every((v) => selectedVolumes.has(v.name))}
          indeterminate={
            unusedVolumes.some((v) => selectedVolumes.has(v.name)) &&
            !unusedVolumes.every((v) => selectedVolumes.has(v.name))
          }
          danger
          onToggle={(checked) =>
            setSelectedVolumes((prev) => {
              const next = new Set(prev);
              for (const v of unusedVolumes) {
                if (checked) {
                  next.add(v.name);
                } else {
                  next.delete(v.name);
                }
              }
              return next;
            })
          }
        >
          {unusedVolumes.map((v) => (
            <ItemRow
              key={v.name}
              checked={selectedVolumes.has(v.name)}
              onToggle={(checked) =>
                setSelectedVolumes((prev) => toggle(prev, v.name, checked))
              }
              title={v.name}
              subtitle={`${v.driver}${
                v.createdAt != null ? ` · ${formatRelativeTime(v.createdAt)}` : ""
              }`}
              meta={
                v.anonymous ? (
                  <Badge tone="warning">{t("docker.anonymousVolume")}</Badge>
                ) : undefined
              }
              right={<Badge tone="danger">{formatBytes(v.sizeBytes)}</Badge>}
            />
          ))}
        </GroupCard>
      ) : null}

      {/* 5. 构建缓存（默认勾选，按时间窗口） */}
      {showCache && report.buildCache.length > 0 ? (
        <GroupCard
          title={t("docker.buildCache")}
          count={windowCache.length}
          bytes={windowCache.reduce((sum, b) => sum + b.sizeBytes, 0)}
          checked={cacheEnabled && windowCache.length > 0}
          onToggle={setCacheEnabled}
        >
          <div className="flex flex-wrap items-center gap-2 border-t border-border py-2 first:border-t-0">
            {CACHE_WINDOWS.map((w) => (
              <Button
                key={w.key}
                size="sm"
                variant={cacheWindow === w.key ? "default" : "outline"}
                onClick={() => setCacheWindow(w.key)}
              >
                {t(CACHE_WINDOW_LABEL[w.key])}
              </Button>
            ))}
          </div>
          {windowCache.map((entry) => (
            <ItemRow
              key={entry.id}
              title={entry.description ?? entry.cacheType}
              subtitle={`${entry.cacheType} · ${
                entry.lastUsedAt != null
                  ? formatRelativeTime(entry.lastUsedAt)
                  : t("docker.neverUsed")
              }`}
              right={<Badge tone="neutral">{formatBytes(entry.sizeBytes)}</Badge>}
            />
          ))}
          {report.buildCache
            .filter((b) => b.inUse)
            .map((entry) => (
              <ItemRow
                key={entry.id}
                checked={false}
                disabled
                onToggle={() => {}}
                title={entry.description ?? entry.cacheType}
                subtitle={`${entry.cacheType} · ${t("docker.inUseHint")}`}
                right={<Badge tone="success">{formatBytes(entry.sizeBytes)}</Badge>}
              />
            ))}
        </GroupCard>
      ) : null}

      {/* 被停止容器引用的镜像（展示但禁选） */}
      {showImages && blockedImages.length > 0 ? (
        <GroupCard
          title={t("docker.blockedImages")}
          count={blockedImages.length}
          bytes={blockedImages.reduce((sum, i) => sum + i.sizeBytes, 0)}
          checked={false}
          disabled
          onToggle={() => {}}
        >
          {blockedImages.map((image) => (
            <ItemRow
              key={image.fullId}
              checked={false}
              disabled
              onToggle={() => {}}
              title={image.repoTags[0] ?? image.id}
              subtitle={t("docker.blockedHint")}
              right={<Badge tone="warning">{formatBytes(image.sizeBytes)}</Badge>}
            />
          ))}
        </GroupCard>
      ) : null}

      {/* 页签下该分类无数据 */}
      {activeTab !== "all" && activeTabEmpty ? (
        <div className="flex items-center justify-center rounded-lg border border-dashed border-border py-8 text-sm text-muted-foreground">
          {t("docker.emptyCategory")}
        </div>
      ) : null}

      {/* 底部汇总条 */}
      <Card className="sticky bottom-0 bg-card/70 backdrop-blur-xl">
        <CardContent className="flex flex-col gap-3 pt-5">
          {cleaning ? (
            <Progress
              value={progress?.progress != null ? progress.progress * 100 : null}
            />
          ) : null}
          <div className="flex items-center justify-between gap-3">
            <span className="text-[13px] text-muted-foreground">
              {t("docker.selectedSummary", {
                count: selectedCount,
                bytes: formatBytes(selectedBytes),
              })}
            </span>
            <div className="flex items-center gap-2">
              <Button
                variant="outline"
                onClick={() => void startScan()}
                disabled={cleaning}
              >
                <RotateCcw />
                {t("docker.rescan")}
              </Button>
              <Button
                variant="destructive"
                disabled={cleaning || selectedCount === 0}
                onClick={() => setConfirmOpen(true)}
              >
                {cleaning ? <Loader2 className="animate-spin" /> : <Trash2 />}
                {cleaning ? t("docker.cleaning") : t("docker.clean")}
              </Button>
            </div>
          </div>
        </CardContent>
      </Card>

      <ConfirmDialog
        open={confirmOpen}
        destructive
        busy={cleaning}
        title={t("docker.confirmTitle")}
        message={
          selectedVolumes.size > 0
            ? `${t("docker.confirmMessage")} ${t("docker.volumeWarning")}`
            : t("docker.confirmMessage")
        }
        confirmLabel={t("docker.clean")}
        onConfirm={confirmClean}
        onClose={() => setConfirmOpen(false)}
      />
    </div>
  );
}
