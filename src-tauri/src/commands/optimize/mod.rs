//! System Optimize — maintenance tasks and startup items.
//!
//! Everything the frontend sees lives here: the task types, the catalogue
//! command and the progress event. The tasks themselves and the startup-item
//! backends live in [`macos`] and [`windows`], which expose the same small
//! surface. Elevation is what forces the split: macOS asks `sudo` for a
//! password it can cache, while Windows can only start a second, elevated copy
//! of the process, and the two flows have nothing in common.

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter};

use super::RiskLevel;
use crate::utils::{Result, WindleError};

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "windows")]
mod windows;

#[cfg(target_os = "macos")]
use macos as platform;
#[cfg(target_os = "windows")]
use windows as platform;

pub const PROGRESS_EVENT: &str = "optimize://progress";

/// Failure text that means "this needed administrator rights", so the UI can
/// offer to retry with an admin prompt instead of showing a raw shell error.
#[cfg(target_os = "macos")]
const ELEVATION_HINTS: [&str; 6] = [
    "operation not permitted",
    "permission denied",
    "must be run as root",
    "must be root",
    "requires root",
    "only root",
];
#[cfg(target_os = "windows")]
const ELEVATION_HINTS: [&str; 4] = [
    "access is denied",
    "access denied",
    "requires elevation",
    "requires administrator",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum OptimizeTaskId {
    PurgeMemory,
    FlushDns,
    // macOS: the maintenance tasks Spotlight, Launch Services and the Dock own.
    #[cfg(target_os = "macos")]
    RebuildSpotlight,
    #[cfg(target_os = "macos")]
    RebuildLaunchServices,
    #[cfg(target_os = "macos")]
    ResetDock,
    #[cfg(target_os = "macos")]
    ClearQuicklook,
    #[cfg(target_os = "macos")]
    RunMaintenanceScripts,
    #[cfg(target_os = "macos")]
    VerifyDisk,
    // Windows: the caches and services that stand in for them there.
    #[cfg(target_os = "windows")]
    ClearTempFiles,
    #[cfg(target_os = "windows")]
    ClearUpdateCache,
    #[cfg(target_os = "windows")]
    RebuildSearchIndex,
    #[cfg(target_os = "windows")]
    ResetIconCache,
    #[cfg(target_os = "windows")]
    VerifySystemFiles,
    #[cfg(target_os = "windows")]
    RepairSystemImage,
    #[cfg(target_os = "windows")]
    OptimizeSystemDrive,
    #[cfg(target_os = "windows")]
    CheckSystemDrive,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OptimizeTask {
    pub id: OptimizeTaskId,
    pub label: String,
    pub description: String,
    pub risk: RiskLevel,
    pub requires_elevation: bool,
    pub estimated_seconds: u32,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum LoginItemKind {
    #[cfg(target_os = "macos")]
    LaunchAgent,
    #[cfg(target_os = "macos")]
    LaunchDaemon,
    /// Classic login items, which only System Settings may edit.
    #[cfg(target_os = "macos")]
    LoginItem,
    /// A value under one of the `…\CurrentVersion\Run` registry keys.
    #[cfg(target_os = "windows")]
    RunKey,
    /// A shortcut in a Startup folder.
    #[cfg(target_os = "windows")]
    StartupFolder,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LoginItem {
    pub id: String,
    pub label: String,
    pub path: String,
    pub kind: LoginItemKind,
    pub enabled: bool,
    pub is_system: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OptimizeOutcome {
    pub task_id: OptimizeTaskId,
    pub succeeded: bool,
    pub message: String,
    pub duration_ms: u64,
}

/// Where a task is up to, streamed while a batch runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TaskStatus {
    Pending,
    Running,
    Done,
    Error,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OptimizeProgress {
    pub task_id: OptimizeTaskId,
    pub status: TaskStatus,
    pub index: usize,
    pub total: usize,
    pub message: Option<String>,
}

/// Static task catalogue, including the risk metadata the UI shows.
#[tauri::command]
pub async fn list_optimize_tasks() -> Result<Vec<OptimizeTask>> {
    Ok(platform::catalogue())
}

/// Run one maintenance task.
#[tauri::command]
pub async fn run_optimize_task(app: AppHandle, task_id: OptimizeTaskId) -> Result<OptimizeOutcome> {
    // A platform runner reports exactly one outcome per requested id.
    let mut outcomes = platform::run_tasks(&app, vec![task_id]).await;
    Ok(outcomes.remove(0))
}

/// Run several tasks in order, one outcome per task, in the order requested.
///
/// Each platform decides how to keep the admin prompts down to one: macOS
/// verifies the password once up front and then reuses it, Windows hands the
/// whole batch to a single elevated helper process.
#[tauri::command]
pub async fn run_optimize_tasks(
    app: AppHandle,
    task_ids: Vec<OptimizeTaskId>,
) -> Result<Vec<OptimizeOutcome>> {
    Ok(platform::run_tasks(&app, task_ids).await)
}

/// Launch agents, daemons and login items that run at startup.
#[tauri::command]
pub async fn list_login_items() -> Result<Vec<LoginItem>> {
    Ok(platform::startup_items())
}

/// Enable or disable a startup item.
#[tauri::command]
pub async fn set_login_item_enabled(id: String, enabled: bool) -> Result<()> {
    // Look the item up rather than trusting the caller: it gives us the label
    // and the location, and refuses ids that are not startup items at all.
    let item = platform::startup_items()
        .into_iter()
        .find(|item| item.id == id)
        .ok_or_else(|| WindleError::NotFound(id))?;

    if item.is_system {
        return Err(WindleError::Protected(item.path));
    }

    platform::set_login_item_enabled(&item, enabled)
}

/// Clear the cached admin password so the next elevated task re-prompts.
#[tauri::command]
pub fn clear_optimize_auth() {
    platform::clear_auth();
}

/// Run the elevated half of an optimize batch when this process was started
/// for it, returning the process exit code. An ordinary launch returns `None`.
pub fn elevated_entrypoint() -> Option<i32> {
    #[cfg(target_os = "windows")]
    {
        windows::elevated_entrypoint(std::env::args())
    }
    #[cfg(not(target_os = "windows"))]
    {
        None
    }
}

/// Run a task off the async runtime (some of these block for a minute) and
/// report its progress along the way.
async fn execute_with(
    app: &AppHandle,
    task_id: OptimizeTaskId,
    index: usize,
    total: usize,
    task: impl FnOnce(OptimizeTaskId) -> Result<String> + Send + 'static,
) -> OptimizeOutcome {
    emit(app, task_id, TaskStatus::Running, index, total, None);

    let started = std::time::Instant::now();

    let result = match tauri::async_runtime::spawn_blocking(move || task(task_id)).await {
        Ok(result) => result,
        Err(error) => Err(WindleError::Command {
            command: "optimize".into(),
            message: error.to_string(),
        }),
    };

    let succeeded = result.is_ok();
    let message = match result {
        Ok(message) => message,
        Err(error) => error.to_string(),
    };

    emit(
        app,
        task_id,
        if succeeded {
            TaskStatus::Done
        } else {
            TaskStatus::Error
        },
        index,
        total,
        Some(message.clone()),
    );

    OptimizeOutcome {
        task_id,
        succeeded,
        message,
        duration_ms: started.elapsed().as_millis() as u64,
    }
}

fn emit(
    app: &AppHandle,
    task_id: OptimizeTaskId,
    status: TaskStatus,
    index: usize,
    total: usize,
    message: Option<String>,
) {
    let _ = app.emit(
        PROGRESS_EVENT,
        OptimizeProgress {
            task_id,
            status,
            index,
            total,
            message,
        },
    );
}

/// Turn a "you are not an administrator" shell failure into an error the UI can
/// act on, and a missing binary into something clearer than `os error 2`.
fn escalate(command: &str, error: WindleError) -> WindleError {
    if let WindleError::Io(io_error) = &error {
        if io_error.kind() == std::io::ErrorKind::NotFound {
            return WindleError::Command {
                command: tool_name(command).to_string(),
                message: "not available on this system".into(),
            };
        }
    }

    let text = error.to_string().to_lowercase();
    if ELEVATION_HINTS.iter().any(|hint| text.contains(hint)) {
        return WindleError::NeedsElevation(tool_name(command).to_string());
    }

    error
}

/// A one-line version of an error, for folding into a longer message.
///
/// Command output runs to many lines, so only the first survives. For a failed
/// command the tool's name is dropped as well: callers already prefix the note
/// with it, and "`net` skipped (`net` failed: …)" reads poorly.
fn summarise(error: &WindleError) -> String {
    let text = match error {
        WindleError::Command { message, .. } => message.as_str(),
        other => return one_line(&other.to_string()),
    };

    one_line(text)
}

fn one_line(text: &str) -> String {
    match text.lines().next() {
        Some(line) if !line.trim().is_empty() => line.trim().to_string(),
        _ => "failed".to_string(),
    }
}

fn last_meaningful_line(output: &str) -> Option<String> {
    output
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .next_back()
        .map(str::to_string)
}

/// `.../Support/lsregister` reads better as `lsregister` in a message.
fn tool_name(command: &str) -> &str {
    command.rsplit(['/', '\\']).next().unwrap_or(command)
}

/// The wire name of a task id (`purge-memory`), the same spelling the frontend
/// sends back and the elevated helper receives on its command line.
#[cfg(target_os = "windows")]
fn slug(task_id: OptimizeTaskId) -> String {
    serde_json::to_string(&task_id)
        .unwrap_or_default()
        .trim_matches('"')
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_permission_failure_asks_for_elevation() {
        #[cfg(target_os = "windows")]
        let message = "Access is denied.";
        #[cfg(not(target_os = "windows"))]
        let message = "sh: /etc/periodic: Operation not permitted";

        let error = WindleError::Command {
            command: "periodic".into(),
            message: message.into(),
        };

        assert!(matches!(
            escalate("periodic", error),
            WindleError::NeedsElevation(_)
        ));
    }

    #[test]
    fn a_missing_binary_reports_itself_by_name() {
        let error = WindleError::Io(std::io::Error::from(std::io::ErrorKind::NotFound));

        match escalate(r"C:\Windows\System32\sfc.exe", error) {
            WindleError::Command { command, message } => {
                assert_eq!(command, "sfc.exe");
                assert!(message.contains("not available"));
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn an_ordinary_failure_is_left_alone() {
        let error = WindleError::Command {
            command: "diskutil".into(),
            message: "could not be unmounted".into(),
        };

        assert!(matches!(
            escalate("diskutil", error),
            WindleError::Command { .. }
        ));
    }

    #[test]
    fn tool_names_lose_their_directory() {
        assert_eq!(tool_name("/bin/sh"), "sh");
        assert_eq!(tool_name(r"C:\Windows\System32\sfc.exe"), "sfc.exe");
        assert_eq!(tool_name("ipconfig"), "ipconfig");
    }

    #[test]
    fn summaries_use_the_first_line() {
        let error = WindleError::Command {
            command: "x".into(),
            message: "first line\nsecond line".into(),
        };
        assert_eq!(summarise(&error), "first line");

        // The tool name the caller already printed does not come back.
        assert!(!summarise(&error).contains("`x`"));

        let denied = WindleError::AccessDenied("/private/var/db/x".into());
        assert!(summarise(&denied).starts_with("access denied: /private/var/db/x"));
    }

    #[test]
    fn an_empty_command_message_still_summarises() {
        let error = WindleError::Command {
            command: "x".into(),
            message: "   \n".into(),
        };
        assert_eq!(summarise(&error), "failed");
    }

    #[test]
    fn the_last_line_of_output_becomes_the_message() {
        assert_eq!(last_meaningful_line("a\n\nb\n  \n"), Some("b".to_string()));
        assert_eq!(last_meaningful_line("   "), None);
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn slugs_match_the_wire_names() {
        assert_eq!(slug(OptimizeTaskId::PurgeMemory), "purge-memory");
        assert_eq!(slug(OptimizeTaskId::FlushDns), "flush-dns");
    }
}
