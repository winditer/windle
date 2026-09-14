import { call, subscribe } from "./ipc";
import type {
  CleanOutcome,
  DockerCleanRequest,
  DockerScanReport,
  DockerStatus,
  ProgressEvent,
} from "@/types";

/** Event name emitted by the Rust side while a clean run is in flight. */
export const DOCKER_PROGRESS_EVENT = "docker://progress";

/** Probe Docker Desktop install/running state. */
export function getDockerStatus(): Promise<DockerStatus> {
  return call<DockerStatus>("docker_status");
}

/** Launch Docker Desktop; readiness is polled via `getDockerStatus`. */
export function startDockerDesktop(): Promise<void> {
  return call<void>("docker_start_desktop");
}

/** Scan images / containers / volumes / build cache. */
export function scanDocker(): Promise<DockerScanReport> {
  return call<DockerScanReport>("docker_scan");
}

/** Delete the validated selection. */
export function cleanDocker(
  request: DockerCleanRequest,
): Promise<CleanOutcome> {
  return call<CleanOutcome>("docker_clean", { request });
}

export function onDockerProgress(handler: (event: ProgressEvent) => void) {
  return subscribe<ProgressEvent>(DOCKER_PROGRESS_EVENT, handler);
}
