import { call } from "./ipc";
import type { AiAgentScanResult, CleanOutcome } from "@/types";

/** Scan known AI agent tool data paths and report sizes. */
export function scanAiAgents(): Promise<AiAgentScanResult> {
  return call<AiAgentScanResult>("scan_ai_agents");
}

/** Remove the selected AI agent data, trashing by default. */
export function removeAiData(
  paths: string[],
  permanent = false,
): Promise<CleanOutcome> {
  return call<CleanOutcome>("remove_ai_data", { paths, permanent });
}
