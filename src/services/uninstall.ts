import { call } from "./ipc";
import type { CleanOutcome, InstalledApp, UninstallPlan } from "@/types";

/** List every application bundle found in /Applications and ~/Applications. */
export function listApps(): Promise<InstalledApp[]> {
  return call<InstalledApp[]>("list_apps");
}

/** Collect the app bundle plus every leftover file we can attribute to it. */
export function buildUninstallPlan(appId: string): Promise<UninstallPlan> {
  return call<UninstallPlan>("build_uninstall_plan", { appId });
}

/** Execute a plan. `paths` lets the user opt out of individual leftovers. */
export function uninstallApp(
  appId: string,
  paths: string[],
): Promise<CleanOutcome> {
  return call<CleanOutcome>("uninstall_app", { appId, paths });
}

/** Find leftovers whose owning app is already gone. */
export function findOrphanedLeftovers(): Promise<UninstallPlan[]> {
  return call<UninstallPlan[]>("find_orphaned_leftovers");
}
