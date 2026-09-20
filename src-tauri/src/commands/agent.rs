//! AI Agent Cleanup — caches, logs and runtime data left behind by AI tools.

use std::path::PathBuf;

use serde::Serialize;

use super::CleanOutcome;
use super::RiskLevel;
use crate::scanner::walker;
use crate::utils::fs_ops::{self, RemoveMode};
use crate::utils::{history, permissions, Result};

/// AI agent data below this size is not worth listing.
const MIN_ITEM_SIZE: u64 = 4_096;

/// Subdirectory names that must never be deleted, even if they appear
/// inside a targeted AI cleanup path. This is a defense-in-depth safety net.
const PROTECTED_SUBDIRS: &[&str] = &[
    "projects",
    "index",
    "repowiki",
    "extension",
    "atlas",
    "model",
    "experts",
    "broker",
    "db",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum AiTool {
    Trae,
    Qoder,
    Codex,
    Real,
    Yuanbao,
    Openclaw,
    Comate,
    #[serde(rename = "opencode")]
    OpenCode,
    CcSwitch,
    ClaudeCode,
    GeminiCli,
    Omega,
    Dsh,
    Other,
}

struct AiPathSpec {
    tool: AiTool,
    path: &'static str,
    data_type: &'static str,
    risk: RiskLevel,
    description: &'static str,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AiDataItem {
    pub id: String,
    pub path: String,
    pub tool: AiTool,
    pub data_type: String,
    pub size: u64,
    pub risk: RiskLevel,
    pub description: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AiToolGroup {
    pub tool: AiTool,
    pub total_size: u64,
    pub items: Vec<AiDataItem>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AiAgentScanResult {
    pub groups: Vec<AiToolGroup>,
    pub total_size: u64,
    pub safe_size: u64,
    pub caution_size: u64,
    pub scanned_at: u64,
}

const AI_PATHS: &[AiPathSpec] = &[
    // Trae
    AiPathSpec { tool: AiTool::Trae, path: "%APP_SUPPORT%/Trae/logs", data_type: "logs", risk: RiskLevel::Safe, description: "Trae log files" },
    AiPathSpec { tool: AiTool::Trae, path: "%APP_SUPPORT%/Trae/Partitions", data_type: "cache", risk: RiskLevel::Safe, description: "Trae webview cache" },
    AiPathSpec { tool: AiTool::Trae, path: "%APP_SUPPORT%/Trae/WebStorage", data_type: "cache", risk: RiskLevel::Safe, description: "Trae web storage cache" },
    AiPathSpec { tool: AiTool::Trae, path: "%APP_SUPPORT%/Trae/CachedExtensionVSIXs", data_type: "cache", risk: RiskLevel::Safe, description: "Trae extension package cache" },
    AiPathSpec { tool: AiTool::Trae, path: "%APP_SUPPORT%/Trae/ModularData/ai-agent", data_type: "index", risk: RiskLevel::Caution, description: "Trae AI agent index (rebuildable)" },
    AiPathSpec { tool: AiTool::Trae, path: "%APP_SUPPORT%/Trae/User/globalStorage/.ckg", data_type: "index", risk: RiskLevel::Caution, description: "Trae code knowledge graph (rebuildable)" },
    AiPathSpec { tool: AiTool::Trae, path: "~/.trae/extensions", data_type: "extensions", risk: RiskLevel::Caution, description: "Trae installed extensions" },
    // Qoder
    AiPathSpec { tool: AiTool::Qoder, path: "%APP_SUPPORT%/Qoder/SharedClientCache/cache", data_type: "cache", risk: RiskLevel::Caution, description: "Qoder shared client cache (conversation history in 'db/' is preserved)" },
    AiPathSpec { tool: AiTool::Qoder, path: "%APP_SUPPORT%/Qoder/SharedClientCache/tmp", data_type: "cache", risk: RiskLevel::Safe, description: "Qoder shared client temp files" },
    AiPathSpec { tool: AiTool::Qoder, path: "%APP_SUPPORT%/Qoder/SharedClientCache/logs", data_type: "logs", risk: RiskLevel::Safe, description: "Qoder shared client logs" },
    AiPathSpec { tool: AiTool::Qoder, path: "%APP_SUPPORT%/Qoder/CachedData", data_type: "cache", risk: RiskLevel::Safe, description: "Qoder cached data" },
    AiPathSpec { tool: AiTool::Qoder, path: "%APP_SUPPORT%/Qoder/logs", data_type: "logs", risk: RiskLevel::Safe, description: "Qoder log files" },
    AiPathSpec { tool: AiTool::Qoder, path: "%APP_SUPPORT%/Qoder/GPUCache", data_type: "cache", risk: RiskLevel::Safe, description: "Qoder GPU cache" },
    AiPathSpec { tool: AiTool::Qoder, path: "~/.qoder/logs", data_type: "logs", risk: RiskLevel::Safe, description: "Qoder CLI logs" },
    AiPathSpec { tool: AiTool::Qoder, path: "~/.qoder/tmp", data_type: "cache", risk: RiskLevel::Safe, description: "Qoder temp files" },
    // Codex (OpenAI)
    AiPathSpec { tool: AiTool::Codex, path: "~/.codex/logs_2.sqlite", data_type: "logs", risk: RiskLevel::Safe, description: "Codex telemetry log database" },
    AiPathSpec { tool: AiTool::Codex, path: "~/.codex/computer-use", data_type: "cache", risk: RiskLevel::Safe, description: "Codex computer-use screenshots" },
    AiPathSpec { tool: AiTool::Codex, path: "~/.codex/sessions", data_type: "sessions", risk: RiskLevel::Caution, description: "Codex session history" },
    AiPathSpec { tool: AiTool::Codex, path: "~/.codex/archived_sessions", data_type: "sessions", risk: RiskLevel::Caution, description: "Codex archived sessions" },
    AiPathSpec { tool: AiTool::Codex, path: "~/.codex/plugins", data_type: "extensions", risk: RiskLevel::Caution, description: "Codex plugins" },
    AiPathSpec { tool: AiTool::Codex, path: "%APP_SUPPORT%/Codex/component_crx_cache", data_type: "cache", risk: RiskLevel::Safe, description: "Codex component cache" },
    AiPathSpec { tool: AiTool::Codex, path: "%APP_SUPPORT%/Codex/GraphiteDawnCache", data_type: "cache", risk: RiskLevel::Safe, description: "Codex graphics cache" },
    // Real
    AiPathSpec { tool: AiTool::Real, path: "~/.real/.bin", data_type: "runtime", risk: RiskLevel::Caution, description: "Real runtime binaries (re-downloadable)" },
    AiPathSpec { tool: AiTool::Real, path: "~/.real/legacy-archive", data_type: "cache", risk: RiskLevel::Safe, description: "Real legacy archive" },
    // Openclaw
    AiPathSpec { tool: AiTool::Openclaw, path: "~/.openclaw-autoclaw/skills", data_type: "cache", risk: RiskLevel::Caution, description: "Openclaw skills cache" },
    // Comate
    AiPathSpec { tool: AiTool::Comate, path: "~/.comate-engine/bin", data_type: "runtime", risk: RiskLevel::Caution, description: "Comate engine binary" },
    // OpenCode
    AiPathSpec { tool: AiTool::OpenCode, path: "~/.opencode/bin", data_type: "runtime", risk: RiskLevel::Caution, description: "OpenCode binary" },
    // cc-switch
    AiPathSpec { tool: AiTool::CcSwitch, path: "~/.cc-switch/backups", data_type: "cache", risk: RiskLevel::Safe, description: "cc-switch old backups" },
    // Claude Code
    AiPathSpec { tool: AiTool::ClaudeCode, path: "~/.claude/telemetry", data_type: "logs", risk: RiskLevel::Safe, description: "Claude Code telemetry" },
    AiPathSpec { tool: AiTool::ClaudeCode, path: "~/.claude/cache", data_type: "cache", risk: RiskLevel::Safe, description: "Claude Code cache" },
    // Gemini CLI
    AiPathSpec { tool: AiTool::GeminiCli, path: "~/.gemini", data_type: "cache", risk: RiskLevel::Safe, description: "Gemini CLI data" },
    // Omega
    AiPathSpec { tool: AiTool::Omega, path: "~/.omega/logs", data_type: "logs", risk: RiskLevel::Safe, description: "Omega log files" },
    // DeepSeek harness (DSH) — credentials, settings, sticky-notes and
    // profile configs under ~/.dsh are intentionally NOT listed; only
    // rebuildable / re-installable data is cleanable.
    AiPathSpec { tool: AiTool::Dsh, path: "~/.dsh/sessions", data_type: "sessions", risk: RiskLevel::Caution, description: "DSH session history" },
    AiPathSpec { tool: AiTool::Dsh, path: "~/.dsh/attachments", data_type: "sessions", risk: RiskLevel::Caution, description: "DSH session attachments" },
    AiPathSpec { tool: AiTool::Dsh, path: "~/.dsh/storages", data_type: "cache", risk: RiskLevel::Caution, description: "DSH workspace state (rebuildable)" },
    AiPathSpec { tool: AiTool::Dsh, path: "~/.dsh/cache", data_type: "cache", risk: RiskLevel::Safe, description: "DSH cache" },
    AiPathSpec { tool: AiTool::Dsh, path: "~/.dsh/profiles/web/node_modules", data_type: "runtime", risk: RiskLevel::Caution, description: "DSH web profile dependencies (re-installable)" },
    AiPathSpec { tool: AiTool::Dsh, path: "~/.dsh/profiles/desktop/node_modules", data_type: "runtime", risk: RiskLevel::Caution, description: "DSH desktop profile dependencies (re-installable)" },
    AiPathSpec { tool: AiTool::Dsh, path: "%APP_SUPPORT%/DSH Desktop/Cache", data_type: "cache", risk: RiskLevel::Safe, description: "DSH Desktop HTTP cache" },
    AiPathSpec { tool: AiTool::Dsh, path: "%APP_SUPPORT%/DSH Desktop/Code Cache", data_type: "cache", risk: RiskLevel::Safe, description: "DSH Desktop code cache" },
    AiPathSpec { tool: AiTool::Dsh, path: "%APP_SUPPORT%/DSH Desktop/GPUCache", data_type: "cache", risk: RiskLevel::Safe, description: "DSH Desktop GPU cache" },
    AiPathSpec { tool: AiTool::Dsh, path: "%APP_SUPPORT%/DSH Desktop/DawnWebGPUCache", data_type: "cache", risk: RiskLevel::Safe, description: "DSH Desktop WebGPU cache" },
    AiPathSpec { tool: AiTool::Dsh, path: "%APP_SUPPORT%/DSH Desktop/DawnGraphiteCache", data_type: "cache", risk: RiskLevel::Safe, description: "DSH Desktop Graphite cache" },
];

/// The token the path table uses for the per-user application data directory.
/// Electron apps keep the same internal layout on both platforms, so a single
/// table describes them everywhere.
const APP_SUPPORT_TOKEN: &str = "%APP_SUPPORT%";

#[cfg(target_os = "macos")]
const APP_SUPPORT_BASE: &str = "~/Library/Application Support";

#[cfg(target_os = "windows")]
const APP_SUPPORT_BASE: &str = "%APPDATA%";

/// Expand one table entry: `%APP_SUPPORT%` for the application data directory,
/// `~` for the home directory, `%NAME%` for an environment variable.
fn expand_path(path: &str) -> PathBuf {
    permissions::expand(&path.replace(APP_SUPPORT_TOKEN, APP_SUPPORT_BASE))
}

/// True when `path` matches one of the known AI agent locations. Used as the
/// delete guard, mirroring `installer::classify`.
fn is_known_ai_path(path: &str) -> bool {
    let expanded = expand_path(path);
    AI_PATHS
        .iter()
        .any(|spec| expand_path(spec.path) == expanded)
}

/// Scan for AI agent caches, logs and runtime data.
#[tauri::command]
pub async fn scan_ai_agents() -> Result<AiAgentScanResult> {
    // Collect specs whose expanded path actually exists on disk.
    let existing: Vec<&AiPathSpec> = AI_PATHS
        .iter()
        .filter(|spec| expand_path(spec.path).exists())
        .collect();

    let paths: Vec<PathBuf> = existing
        .iter()
        .map(|spec| expand_path(spec.path))
        .collect();

    // Measure all existing paths in parallel, preserving order.
    let sizes = walker::sizes_of(&paths);

    let mut items: Vec<AiDataItem> = Vec::new();
    for (spec, size) in existing.into_iter().zip(sizes) {
        if size < MIN_ITEM_SIZE {
            continue;
        }

        let path = expand_path(spec.path);
        let path_string = path.to_string_lossy().into_owned();

        items.push(AiDataItem {
            id: path_string.clone(),
            path: path_string,
            tool: spec.tool,
            data_type: spec.data_type.to_string(),
            size,
            risk: spec.risk,
            description: spec.description.to_string(),
        });
    }

    // Group by tool. `AiTool` does not derive `Ord`/`Hash`, so a linear scan
    // keeps the grouping simple and dependency-free.
    let mut groups: Vec<AiToolGroup> = Vec::new();
    for item in items {
        if let Some(group) = groups.iter_mut().find(|g| g.tool == item.tool) {
            group.items.push(item);
        } else {
            groups.push(AiToolGroup {
                tool: item.tool,
                total_size: 0,
                items: vec![item],
            });
        }
    }

    // Sort items within each group by size descending, then tally.
    let mut total_size = 0u64;
    let mut safe_size = 0u64;
    let mut caution_size = 0u64;

    for group in &mut groups {
        group.items.sort_by(|a, b| b.size.cmp(&a.size));
        group.total_size = group.items.iter().map(|item| item.size).sum();

        for item in &group.items {
            total_size += item.size;
            match item.risk {
                RiskLevel::Safe => safe_size += item.size,
                RiskLevel::Caution => caution_size += item.size,
                RiskLevel::Danger => {}
            }
        }
    }

    // Biggest tool first.
    groups.sort_by(|a, b| b.total_size.cmp(&a.total_size));

    Ok(AiAgentScanResult {
        groups,
        total_size,
        safe_size,
        caution_size,
        scanned_at: super::clean::now_millis(),
    })
}

/// Remove the selected AI agent data, trashing them by default.
#[tauri::command]
pub async fn remove_ai_data(paths: Vec<String>, permanent: bool) -> Result<CleanOutcome> {
    let mode = RemoveMode::from_permanent(permanent);
    let mut outcome = CleanOutcome::default();

    for path in paths {
        let target = PathBuf::from(&path);

        // Only ever remove paths that match a known AI agent location.
        if !is_known_ai_path(&path) {
            outcome.fail(path, "not a known AI agent path");
            continue;
        }

        // Defense-in-depth: when a directory contains protected
        // subdirectories (e.g., conversation history, knowledge graphs),
        // preserve them and only remove the non-protected children —
        // selective deletion instead of refusing entirely. This frees cache
        // space without losing irreplaceable user data.
        if target.is_dir() {
            match std::fs::read_dir(&target) {
                Ok(entries) => {
                    let mut children: Vec<std::fs::DirEntry> = Vec::new();
                    let mut aborted = false;
                    for res in entries {
                        match res {
                            Ok(e) => children.push(e),
                            Err(_) => {
                                aborted = true;
                                break;
                            }
                        }
                    }
                    if aborted {
                        outcome.fail(path, "refused: directory enumeration incomplete");
                        continue;
                    }

                    let has_protected = children.iter().any(|entry| {
                        match entry.file_name().to_str() {
                            Some(name) => PROTECTED_SUBDIRS.contains(&name),
                            None => false,
                        }
                    });

                    if has_protected {
                        let mut removed_any = false;
                        let mut had_removable = false;
                        let mut protected_names: Vec<String> = Vec::new();

                        for entry in &children {
                            if let Some(name) = entry.file_name().to_str() {
                                if PROTECTED_SUBDIRS.contains(&name) {
                                    protected_names.push(name.to_string());
                                    continue;
                                }
                            }

                            had_removable = true;

                            let child = entry.path();
                            match fs_ops::remove(&child, mode) {
                                Ok(bytes) => {
                                    outcome.succeed(
                                        child.to_string_lossy().into_owned(),
                                        bytes,
                                    );
                                    removed_any = true;
                                }
                                Err(error) => {
                                    outcome.fail(
                                        child.to_string_lossy().into_owned(),
                                        error.to_string(),
                                    );
                                }
                            }
                        }

                        if !removed_any {
                            if had_removable {
                                outcome.fail(
                                    path,
                                    "partial: all removable children failed (see child errors)",
                                );
                            } else {
                                outcome.fail(
                                    path,
                                    format!(
                                        "skipped: all subdirectories are protected ({})",
                                        protected_names.join(", ")
                                    ),
                                );
                            }
                        }

                        continue;
                    }
                }
                Err(error) => {
                    outcome.fail(path, format!("refused: cannot enumerate dir ({error})"));
                    continue;
                }
            }
        }

        match fs_ops::remove(&target, mode) {
            Ok(freed) => outcome.succeed(path, freed),
            Err(error) => outcome.fail(path, error.to_string()),
        }
    }

    history::record_outcome(history::Operation::Agent, &outcome);

    Ok(outcome)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_app_support_token_resolves_to_the_platform_data_dir() {
        assert_eq!(
            expand_path("%APP_SUPPORT%/Trae/logs"),
            permissions::expand(APP_SUPPORT_BASE).join("Trae/logs")
        );
    }

    #[test]
    fn every_table_entry_is_recognised_by_the_guard() {
        for spec in AI_PATHS {
            let expanded = expand_path(spec.path);

            assert!(expanded.is_absolute(), "{} must be absolute", spec.path);
            assert!(
                is_known_ai_path(&expanded.to_string_lossy()),
                "{} must be recognised",
                spec.path
            );
        }
    }

    #[test]
    fn removal_refuses_non_ai_paths() {
        let outcome = tauri::async_runtime::block_on(remove_ai_data(
            vec!["/tmp/notes.txt".to_string()],
            false,
        ))
        .expect("per-path failures are reported in the outcome");

        assert_eq!(outcome.failed_paths.len(), 1);
        assert_eq!(outcome.freed_bytes, 0);
    }
}
