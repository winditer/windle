import { call, subscribe } from "./ipc";
import type {
  LoginItem,
  OptimizeOutcome,
  OptimizeProgress,
  OptimizeTask,
  OptimizeTaskId,
} from "@/types";

/** Event name emitted by the Rust backend while optimize tasks run. */
export const OPTIMIZE_PROGRESS_EVENT = "optimize://progress";

/** Tasks the backend knows how to run, with their risk metadata. */
export function listOptimizeTasks(): Promise<OptimizeTask[]> {
  return call<OptimizeTask[]>("list_optimize_tasks");
}

export function runOptimizeTask(
  taskId: OptimizeTaskId,
): Promise<OptimizeOutcome> {
  return call<OptimizeOutcome>("run_optimize_task", { taskId });
}

/** Run several tasks sequentially, returning one outcome per task. */
export function runOptimizeTasks(
  taskIds: OptimizeTaskId[],
): Promise<OptimizeOutcome[]> {
  return call<OptimizeOutcome[]>("run_optimize_tasks", { taskIds });
}

/** Launch agents, daemons and login items that start at boot. */
export function listLoginItems(): Promise<LoginItem[]> {
  return call<LoginItem[]>("list_login_items");
}

export function setLoginItemEnabled(
  id: string,
  enabled: boolean,
): Promise<void> {
  return call<void>("set_login_item_enabled", { id, enabled });
}

/** Clear the cached admin password so the next elevated task re-prompts. */
export function clearOptimizeAuth(): Promise<void> {
  return call<void>("clear_optimize_auth");
}

/** Subscribe to `optimize://progress` events for real-time task progress. */
export function onOptimizeProgress(
  handler: (event: OptimizeProgress) => void,
) {
  return subscribe<OptimizeProgress>(OPTIMIZE_PROGRESS_EVENT, handler);
}
