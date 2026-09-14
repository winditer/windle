import { call, subscribe } from "./ipc";
import type {
  CleanCategoryId,
  CleanOutcome,
  CleanScanResult,
  ProgressEvent,
} from "@/types";

/** Event name emitted by the Rust scanner while walking the filesystem. */
export const CLEAN_PROGRESS_EVENT = "clean://progress";

/** Scan the requested categories (all of them when omitted). */
export function scanJunk(
  categories?: CleanCategoryId[],
): Promise<CleanScanResult> {
  return call<CleanScanResult>("scan_junk", { categories: categories ?? null });
}

/** Delete the given paths, moving them to the trash unless `permanent`. */
export function cleanPaths(
  paths: string[],
  permanent = false,
): Promise<CleanOutcome> {
  return call<CleanOutcome>("clean_paths", { paths, permanent });
}

/** Empty the user's trash. */
export function emptyTrash(): Promise<CleanOutcome> {
  return call<CleanOutcome>("empty_trash");
}

/** Cancel an in-flight scan. */
export function cancelScan(): Promise<void> {
  return call<void>("cancel_clean_scan");
}

export function onCleanProgress(handler: (event: ProgressEvent) => void) {
  return subscribe<ProgressEvent>(CLEAN_PROGRESS_EVENT, handler);
}
