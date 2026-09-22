/* ---------------------------------------------------------------------------
 * Shared types. These mirror the `serde` structs exposed by the Rust side —
 * keep both in sync when changing a payload shape.
 * ------------------------------------------------------------------------- */

/** Navigation targets, one per feature module. */
export type ModuleId =
  | "dashboard"
  | "clean"
  | "uninstall"
  | "analyze"
  | "optimize"
  | "monitor"
  | "purge"
  | "installer"
  | "agent"
  | "docker";

/** Long-running scan/clean lifecycle. */
export type TaskStatus = "idle" | "scanning" | "ready" | "running" | "done" | "error";

/** The operating systems the backend can report via `platform_info`. */
export type Platform = "macos" | "windows" | "linux";

/** How risky it is to delete something. */
export type RiskLevel = "safe" | "caution" | "danger";

export interface ProgressEvent {
  /** 0–1, or null when the total is unknown. */
  progress: number | null;
  currentPath: string;
  itemsScanned: number;
  bytesFound: number;
}

/* --------------------------------- Clean --------------------------------- */

export type CleanCategoryId =
  | "user-cache"
  | "system-cache"
  | "app-logs"
  | "app-junk"
  | "browser-cache"
  | "trash"
  | "downloads"
  | "mail-attachments"
  | "xcode-derived-data"
  | "ios-backups"
  | "language-files"
  | "broken-symlinks";

/** Sub-grouping inside a clean category, e.g. "Crash Reports" within app-logs. */
export interface ItemGroup {
  /** kebab-case machine id, e.g. "crash-reports". */
  id: string;
  /** English display label, e.g. "Crash Reports". */
  label: string;
}

export interface CleanableItem {
  id: string;
  path: string;
  size: number;
  /** Unix epoch milliseconds. */
  modifiedAt: number | null;
  category: CleanCategoryId;
  risk: RiskLevel;
  /** Why this item is safe (or not) to remove. */
  description: string;
  /** Sub-group inside the category; a Rust `Option` serializes absent as null (never undefined). */
  group: ItemGroup | null;
}

export interface CleanCategory {
  id: CleanCategoryId;
  label: string;
  description: string;
  risk: RiskLevel;
  totalSize: number;
  itemCount: number;
  items: CleanableItem[];
}

export interface CleanScanResult {
  categories: CleanCategory[];
  totalSize: number;
  scannedAt: number;
  durationMs: number;
}

export interface CleanOutcome {
  removedPaths: string[];
  failedPaths: { path: string; reason: string }[];
  freedBytes: number;
}

/* ------------------------------- Uninstall ------------------------------- */

export interface InstalledApp {
  id: string;
  name: string;
  bundleId: string | null;
  version: string | null;
  path: string;
  /** Size of the .app bundle only. */
  bundleSize: number;
  iconPath: string | null;
  installedAt: number | null;
  lastUsedAt: number | null;
  /** True for App Store / system-managed apps that cannot be removed freely. */
  isSystem: boolean;
}

export type LeftoverKind =
  | "preferences"
  | "application-support"
  | "caches"
  | "logs"
  | "saved-state"
  | "containers"
  | "launch-agent"
  | "receipt"
  | "other";

export interface AppLeftover {
  path: string;
  kind: LeftoverKind;
  size: number;
  risk: RiskLevel;
}

export interface UninstallPlan {
  app: InstalledApp;
  leftovers: AppLeftover[];
  totalSize: number;
  /** Set when the removal will need an admin prompt. */
  requiresElevation: boolean;
}

/* -------------------------------- Analyzer ------------------------------- */

export interface DiskUsage {
  mountPoint: string;
  name: string;
  fileSystem: string;
  totalBytes: number;
  availableBytes: number;
  usedBytes: number;
  isRemovable: boolean;
}

export interface TreeNode {
  name: string;
  path: string;
  size: number;
  isDirectory: boolean;
  /** Undefined until the node has been expanded/scanned. */
  children?: TreeNode[];
  /** Fraction of the parent's size, 0–1. */
  share: number;
}

export interface FileTypeBreakdown {
  extension: string;
  label: string;
  size: number;
  fileCount: number;
}

export interface AnalyzeResult {
  root: TreeNode;
  largestFiles: TreeNode[];
  byType: FileTypeBreakdown[];
  scannedAt: number;
  durationMs: number;
}

/* -------------------------------- Monitor -------------------------------- */

/** Static facts about the machine, sent along with every snapshot. */
export interface HostInfo {
  hostname: string | null;
  osVersion: string | null;
  kernelVersion: string | null;
  cpuBrand: string | null;
  physicalCores: number | null;
}

export interface CpuStats {
  /** Overall usage 0–1. */
  usage: number;
  perCore: number[];
  /** 1/5/15 minute load averages. */
  loadAverage: [number, number, number];
  temperatureC: number | null;
  fanSpeedRpm: number | null;
}

export interface MemoryStats {
  totalBytes: number;
  usedBytes: number;
  availableBytes: number;
  /** macOS memory pressure, 0–1. */
  pressure: number;
  swapUsedBytes: number;
  swapTotalBytes: number;
}

export interface NetworkStats {
  interfaceName: string;
  rxBytesPerSec: number;
  txBytesPerSec: number;
  totalRxBytes: number;
  totalTxBytes: number;
}

export interface DiskIoStats {
  readBytesPerSec: number;
  writeBytesPerSec: number;
}

export interface ProcessInfo {
  pid: number;
  name: string;
  cpuUsage: number;
  memoryBytes: number;
  user: string | null;
  command: string;
}

export interface BatteryStats {
  /** 0–1. */
  level: number;
  isCharging: boolean;
  cycleCount: number | null;
  healthPercent: number | null;
  timeRemainingMinutes: number | null;
}

export interface SystemSnapshot {
  timestamp: number;
  cpu: CpuStats;
  memory: MemoryStats;
  network: NetworkStats[];
  diskIo: DiskIoStats;
  topProcesses: ProcessInfo[];
  battery: BatteryStats | null;
  uptimeSeconds: number;
  host: HostInfo;
}

/** Single point in a rolling chart series. */
export interface MetricPoint {
  timestamp: number;
  value: number;
}

/* ----------------------------- Desktop widget ---------------------------- */

/** Which side of the ball the hover panel opened on. */
export type WidgetPlacement = "above" | "below";

/**
 * Which edge of the window the ball sits on once the panel is open. The panel
 * is wider than the ball, so it has to reach out to one side.
 */
export type WidgetSide = "left" | "right";

/**
 * The floating widget's own reading. Narrower than `SystemSnapshot` on purpose:
 * the process table is the expensive half of a sample and the ball shows none
 * of it.
 */
export interface WidgetSnapshot {
  /** 0–1. */
  cpuUsage: number;
  temperatureC: number | null;
  fanSpeedRpm: number | null;
  memoryUsedBytes: number;
  memoryTotalBytes: number;
  networkRxBytesPerSec: number;
  networkTxBytesPerSec: number;
  diskUsedBytes: number;
  diskTotalBytes: number;
}

/* -------------------------------- Optimize ------------------------------- */

/** Runtime status of an optimize task, streamed via `optimize://progress`. */
export type OptimizeTaskStatus = "pending" | "running" | "done" | "error";

/** Progress payload emitted while a batch of optimize tasks is running. */
export interface OptimizeProgress {
  taskId: OptimizeTaskId;
  status: OptimizeTaskStatus;
  index: number;
  total: number;
  message: string | null;
}

export type OptimizeTaskId =
  | "purge-memory"
  | "flush-dns"
  // macOS-only tasks.
  | "rebuild-spotlight"
  | "rebuild-launch-services"
  | "reset-dock"
  | "clear-quicklook"
  | "run-maintenance-scripts"
  | "verify-disk"
  // Windows-only tasks.
  | "clear-temp-files"
  | "clear-update-cache"
  | "rebuild-search-index"
  | "reset-icon-cache"
  | "verify-system-files"
  | "repair-system-image"
  | "optimize-system-drive"
  | "check-system-drive";

export interface OptimizeTask {
  id: OptimizeTaskId;
  label: string;
  description: string;
  risk: RiskLevel;
  requiresElevation: boolean;
  /** Rough duration hint for the UI. */
  estimatedSeconds: number;
}

export interface LoginItem {
  id: string;
  label: string;
  path: string;
  kind:
    | "launch-agent"
    | "launch-daemon"
    | "login-item"
    | "run-key"
    | "startup-folder";
  enabled: boolean;
  isSystem: boolean;
}

export interface OptimizeOutcome {
  taskId: OptimizeTaskId;
  succeeded: boolean;
  message: string;
  durationMs: number;
}

/* --------------------------------- Purge --------------------------------- */

export type ProjectKind =
  | "node"
  | "rust"
  | "python"
  | "go"
  | "java"
  | "xcode"
  | "flutter"
  | "unity"
  | "unknown";

export interface ProjectArtifact {
  path: string;
  /** node_modules, target, .venv, build, … */
  label: string;
  size: number;
  fileCount: number;
}

export interface ProjectInfo {
  id: string;
  name: string;
  path: string;
  kind: ProjectKind;
  artifacts: ProjectArtifact[];
  reclaimableSize: number;
  lastModifiedAt: number | null;
  /** True when the folder is a git repo with a remote, i.e. easy to restore. */
  hasRemote: boolean;
}

export interface PurgeScanResult {
  projects: ProjectInfo[];
  totalReclaimable: number;
  scannedAt: number;
  /** The roots that were actually scanned (after validation), in `~/` form. */
  scannedRoots: string[];
}

/* ------------------------------- Installer ------------------------------- */

export type InstallerKind = "dmg" | "pkg" | "zip" | "iso" | "app-archive";

export interface InstallerFile {
  id: string;
  path: string;
  name: string;
  kind: InstallerKind;
  size: number;
  createdAt: number | null;
  /** Matched installed app, when we could pair them up. */
  matchedApp: string | null;
  /** Installer is for an app that is already installed. */
  isRedundant: boolean;
}

export interface InstallerScanResult {
  installers: InstallerFile[];
  totalSize: number;
  redundantSize: number;
  scannedAt: number;
}

/* ------------------------------- Dashboard ------------------------------- */

export interface DashboardSummary {
  disk: DiskUsage | null;
  memory: MemoryStats | null;
  cpu: CpuStats | null;
  junkSize: number;
  appCount: number;
  lastCleanAt: number | null;
  totalFreedBytes: number;
}

/* --------------------------------- Agent -------------------------------- */

export type AiTool =
  | "trae"
  | "qoder"
  | "codex"
  | "real"
  | "yuanbao"
  | "openclaw"
  | "comate"
  | "opencode"
  | "cc-switch"
  | "claude-code"
  | "gemini-cli"
  | "omega"
  | "dsh"
  | "other";

export interface AiDataItem {
  id: string;
  path: string;
  tool: AiTool;
  dataType: string;
  size: number;
  risk: RiskLevel;
  description: string;
}

export interface AiToolGroup {
  tool: AiTool;
  totalSize: number;
  items: AiDataItem[];
}

export interface AiAgentScanResult {
  groups: AiToolGroup[];
  totalSize: number;
  safeSize: number;
  cautionSize: number;
  scannedAt: number;
}

/* --------------------------------- Misc ---------------------------------- */

export interface PermissionState {
  /** Full Disk Access — required to read most system caches. */
  fullDiskAccess: boolean;
  /** Whether we already hold an admin authorization session. */
  adminAuthorized: boolean;
}

export interface AppError {
  code: string;
  message: string;
  path?: string;
}

/* --------------------------------- Docker -------------------------------- */

/** Docker Desktop install/running state, from `docker_status`. */
export type DockerEnvState = "not-installed" | "not-running" | "ready";

/** Why an image is (not) cleanable. Mirrors Rust `ImageKind` (kebab-case). */
export type DockerImageKind =
  | "dangling"
  | "unused"
  | "in-use"
  | "blocked-by-stopped-container";

export interface DockerCategoryUsage {
  totalCount: number;
  activeCount: number;
  totalBytes: number;
  reclaimableBytes: number;
}

export interface DockerStatus {
  state: DockerEnvState;
  engineVersion: string | null;
  socketPath: string | null;
}

export interface DockerImageInfo {
  /** 12-char display id; use `fullId` for deletion. */
  id: string;
  fullId: string;
  repoTags: string[];
  sizeBytes: number;
  createdAt: number;
  kind: DockerImageKind;
}

export interface DockerContainerInfo {
  id: string;
  name: string;
  imageId: string;
  state: string;
  exitCode: number | null;
  statusText: string;
  sizeBytes: number;
}

export interface DockerVolumeInfo {
  name: string;
  driver: string;
  anonymous: boolean;
  sizeBytes: number;
  inUse: boolean;
  createdAt: number | null;
}

export interface DockerBuildCacheInfo {
  id: string;
  cacheType: string;
  sizeBytes: number;
  inUse: boolean;
  lastUsedAt: number | null;
  description: string | null;
}

export interface DockerOverview {
  images: DockerCategoryUsage;
  containers: DockerCategoryUsage;
  volumes: DockerCategoryUsage;
  buildCache: DockerCategoryUsage;
}

export interface DockerScanReport {
  overview: DockerOverview;
  images: DockerImageInfo[];
  stoppedContainers: DockerContainerInfo[];
  unusedVolumes: DockerVolumeInfo[];
  buildCache: DockerBuildCacheInfo[];
  scannedAt: number;
}

/** Sent to `docker_clean`; ids must come from a prior `scanDocker` report. */
export interface DockerCleanRequest {
  imageIds: string[];
  containerIds: string[];
  volumeNames: string[];
  /** "0s" | "72h" | "168h" | "720h"; null skips build cache. */
  buildCacheUntil: string | null;
}
