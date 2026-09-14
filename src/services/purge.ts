import { call, subscribe } from "./ipc";
import type { CleanOutcome, ProgressEvent, PurgeScanResult } from "@/types";

export const PURGE_PROGRESS_EVENT = "purge://progress";

/** Look for build artifacts under the given roots (defaults to ~/Documents). */
export function scanProjects(roots?: string[]): Promise<PurgeScanResult> {
  return call<PurgeScanResult>("scan_projects", roots ? { roots } : undefined);
}

/** Delete the selected artifact directories. */
export function purgeArtifacts(paths: string[]): Promise<CleanOutcome> {
  return call<CleanOutcome>("purge_artifacts", { paths });
}

export function onPurgeProgress(handler: (event: ProgressEvent) => void) {
  return subscribe<ProgressEvent>(PURGE_PROGRESS_EVENT, handler);
}
