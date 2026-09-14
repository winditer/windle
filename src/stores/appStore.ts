import { create } from "zustand";
import type {
  AppError,
  DashboardSummary,
  ModuleId,
  PermissionState,
  ProgressEvent,
  TaskStatus,
} from "@/types";

const MODULE_IDS: ModuleId[] = [
  "dashboard",
  "clean",
  "uninstall",
  "analyze",
  "optimize",
  "monitor",
  "purge",
  "installer",
  "agent",
  "docker",
];

const idleStatuses = () =>
  Object.fromEntries(MODULE_IDS.map((id) => [id, "idle" as TaskStatus])) as Record<
    ModuleId,
    TaskStatus
  >;

interface AppState {
  /* Navigation */
  activeModule: ModuleId;
  sidebarCollapsed: boolean;

  /* Cross-module task state */
  status: TaskStatus;
  progress: ProgressEvent | null;
  /**
   * Per-module lifecycle, so a module that already scanned still shows its
   * results after the user navigates away and back.
   */
  moduleStatus: Record<ModuleId, TaskStatus>;

  /* Shared data */
  summary: DashboardSummary | null;
  permissions: PermissionState;
  lastError: AppError | null;

  /** Bytes reclaimed during this session, shown in the sidebar footer. */
  freedThisSession: number;

  /* Actions */
  setActiveModule: (module: ModuleId) => void;
  toggleSidebar: () => void;
  setStatus: (status: TaskStatus) => void;
  setModuleStatus: (module: ModuleId, status: TaskStatus) => void;
  setProgress: (progress: ProgressEvent | null) => void;
  setSummary: (summary: DashboardSummary | null) => void;
  setPermissions: (permissions: PermissionState) => void;
  setError: (error: AppError | null) => void;
  addFreedBytes: (bytes: number) => void;
  resetTask: () => void;
}

export const useAppStore = create<AppState>((set) => ({
  activeModule: "dashboard",
  sidebarCollapsed: false,

  status: "idle",
  progress: null,
  moduleStatus: idleStatuses(),

  summary: null,
  permissions: { fullDiskAccess: false, adminAuthorized: false },
  lastError: null,

  freedThisSession: 0,

  // Switching modules swaps in that module's own lifecycle state; the header
  // progress bar belongs to whatever is on screen.
  setActiveModule: (activeModule) =>
    set((state) => ({
      activeModule,
      status: state.moduleStatus[activeModule],
      progress: null,
      lastError: null,
    })),

  toggleSidebar: () =>
    set((state) => ({ sidebarCollapsed: !state.sidebarCollapsed })),

  setStatus: (status) =>
    set((state) => ({
      status,
      moduleStatus: { ...state.moduleStatus, [state.activeModule]: status },
    })),

  setModuleStatus: (module, status) =>
    set((state) => ({
      moduleStatus: { ...state.moduleStatus, [module]: status },
      ...(state.activeModule === module ? { status } : {}),
    })),

  setProgress: (progress) => set({ progress }),
  setSummary: (summary) => set({ summary }),
  setPermissions: (permissions) => set({ permissions }),
  setError: (lastError) =>
    set({ lastError, ...(lastError ? { status: "error" as TaskStatus } : {}) }),

  addFreedBytes: (bytes) =>
    set((state) => ({ freedThisSession: state.freedThisSession + bytes })),

  resetTask: () =>
    set((state) => ({
      status: "idle",
      progress: null,
      lastError: null,
      moduleStatus: { ...state.moduleStatus, [state.activeModule]: "idle" },
    })),
}));

/* Selectors — keep components subscribed to the narrowest slice possible. */
export const selectActiveModule = (state: AppState) => state.activeModule;
export const selectStatus = (state: AppState) => state.status;
export const selectProgress = (state: AppState) => state.progress;
