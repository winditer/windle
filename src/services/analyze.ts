import { call, subscribe } from "./ipc";
import type { AnalyzeResult, DiskUsage, ProgressEvent, TreeNode } from "@/types";

export const ANALYZE_PROGRESS_EVENT = "analyze://progress";

/** Capacity/usage for every mounted volume. */
export function listVolumes(): Promise<DiskUsage[]> {
  return call<DiskUsage[]>("list_volumes");
}

/** Walk `path` and build a size tree, `depth` levels deep. */
export function analyzePath(path: string, depth = 3): Promise<AnalyzeResult> {
  return call<AnalyzeResult>("analyze_path", { path, depth });
}

/** Lazily expand one directory node. */
export function expandNode(path: string): Promise<TreeNode[]> {
  return call<TreeNode[]>("expand_node", { path });
}

/** The `limit` biggest files below `path`. */
export function findLargestFiles(path: string, limit = 50): Promise<TreeNode[]> {
  return call<TreeNode[]>("find_largest_files", { path, limit });
}

/** Reveal a path in Finder. */
export function revealInFinder(path: string): Promise<void> {
  return call<void>("reveal_in_finder", { path });
}

export function onAnalyzeProgress(handler: (event: ProgressEvent) => void) {
  return subscribe<ProgressEvent>(ANALYZE_PROGRESS_EVENT, handler);
}
