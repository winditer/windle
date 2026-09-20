pub mod analyze;
pub mod clean;
pub mod docker;
pub mod installer;
pub mod monitor;
pub mod optimize;
pub mod purge;
pub mod uninstall;
pub mod agent;

use serde::{Deserialize, Serialize};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

/// How risky it is to remove an item. Mirrors `RiskLevel` on the frontend.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RiskLevel {
    Safe,
    Caution,
    Danger,
}

/// Result of any delete operation. Mirrors `CleanOutcome`.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CleanOutcome {
    pub removed_paths: Vec<String>,
    pub failed_paths: Vec<FailedPath>,
    pub freed_bytes: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FailedPath {
    pub path: String,
    pub reason: String,
}

impl CleanOutcome {
    pub fn fail(&mut self, path: impl Into<String>, reason: impl Into<String>) {
        self.failed_paths.push(FailedPath {
            path: path.into(),
            reason: reason.into(),
        });
    }

    pub fn succeed(&mut self, path: impl Into<String>, bytes: u64) {
        self.removed_paths.push(path.into());
        self.freed_bytes += bytes;
    }
}

/// Aggregated numbers for the dashboard. Mirrors `DashboardSummary`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DashboardSummary {
    pub disk: Option<analyze::DiskUsage>,
    pub memory: Option<monitor::MemoryStats>,
    pub cpu: Option<monitor::CpuStats>,
    pub junk_size: u64,
    pub app_count: usize,
    pub last_clean_at: Option<u64>,
    pub total_freed_bytes: u64,
}

/// Cache slow dashboard fields (junk size, app count) for 60 seconds so the
/// 5-second poll loop doesn't re-traverse the filesystem every tick.
static JUNK_CACHE: Mutex<Option<(u64, u64)>> = Mutex::new(None); // (size, timestamp_secs)
static APP_COUNT_CACHE: Mutex<Option<(u64, u64)>> = Mutex::new(None); // (count, timestamp_secs)
const CACHE_TTL_SECS: u64 = 60;

/// Current Unix timestamp in seconds.
fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Return the cached junk size if fresh, otherwise recompute and cache.
/// Falls through to fresh computation on mutex poisoning.
fn cached_junk_size() -> u64 {
    let now = now_secs();
    if let Ok(cache) = JUNK_CACHE.lock() {
        if let Some((size, ts)) = *cache {
            if now.saturating_sub(ts) < CACHE_TTL_SECS {
                return size;
            }
        }
    }
    let size = clean::quick_junk_estimate();
    if let Ok(mut cache) = JUNK_CACHE.lock() {
        *cache = Some((size, now));
    }
    size
}

/// Invalidate the junk size cache so the next dashboard summary re-scans.
/// Called after `clean_paths` deletes junk files so the frontend sees the
/// fresh (post-cleanup) value instead of the stale 60 s TTL cache.
pub fn invalidate_junk_cache() {
    if let Ok(mut cache) = JUNK_CACHE.lock() {
        *cache = None;
    }
}

/// Return the cached app count if fresh, otherwise recompute and cache.
/// Falls through to fresh computation on mutex poisoning.
fn cached_app_count() -> usize {
    let now = now_secs();
    if let Ok(cache) = APP_COUNT_CACHE.lock() {
        if let Some((count, ts)) = *cache {
            if now.saturating_sub(ts) < CACHE_TTL_SECS {
                return count as usize;
            }
        }
    }
    let count = uninstall::installed_app_count();
    if let Ok(mut cache) = APP_COUNT_CACHE.lock() {
        *cache = Some((count as u64, now));
    }
    count
}

/// Everything the dashboard needs in one round-trip.
#[tauri::command]
pub async fn get_dashboard_summary() -> crate::utils::Result<DashboardSummary> {
    let snapshot = monitor::sample_system();
    let history = crate::utils::history::load();

    Ok(DashboardSummary {
        disk: analyze::boot_volume(),
        memory: Some(snapshot.memory),
        cpu: Some(snapshot.cpu),
        junk_size: cached_junk_size(),
        app_count: cached_app_count(),
        last_clean_at: history.last_clean_at(),
        total_freed_bytes: history.total_freed_bytes,
    })
}

/// Whether Full Disk Access and an admin session are available.
#[tauri::command]
pub async fn check_permissions() -> crate::utils::Result<crate::utils::permissions::PermissionState>
{
    Ok(crate::utils::permissions::state())
}

/// Open System Settings straight on the Full Disk Access pane.
#[tauri::command]
pub async fn open_full_disk_access_settings() -> crate::utils::Result<()> {
    crate::utils::permissions::request_permissions()
}

/// The operating system the backend is running on, so the frontend can adapt
/// window chrome and wording without duplicating the cfg logic.
#[tauri::command]
pub async fn platform_info() -> &'static str {
    std::env::consts::OS
}

/// Reveal a path in Finder.
#[cfg(target_os = "macos")]
#[tauri::command]
pub async fn reveal_in_finder(path: String) -> crate::utils::Result<()> {
    let target = std::path::PathBuf::from(&path);
    if !target.exists() {
        return Err(crate::utils::WindleError::NotFound(path));
    }

    run_tool("open", &["-R", &path]).map(|_| ())
}

/// Reveal a path in File Explorer, with the item selected. Explorer exits
/// non-zero even on success, so only a failed launch is an error here — the
/// window opens on its own.
#[cfg(target_os = "windows")]
#[tauri::command]
pub async fn reveal_in_finder(path: String) -> crate::utils::Result<()> {
    let target = std::path::PathBuf::from(&path);
    if !target.exists() {
        return Err(crate::utils::WindleError::NotFound(path));
    }

    std::process::Command::new("explorer")
        // The quotes keep paths with spaces together: Explorer parses the
        // switch itself rather than through `argv`.
        .arg(format!("/select,\"{path}\""))
        .spawn()
        .map(|_| ())
        .map_err(|e| crate::utils::WindleError::Command {
            command: "explorer".into(),
            message: e.to_string(),
        })
}

/// Run a system utility and return its stdout, mapping a non-zero exit to an
/// error the frontend can display.
pub(crate) fn run_tool(command: &str, args: &[&str]) -> crate::utils::Result<String> {
    let output = std::process::Command::new(command).args(args).output()?;

    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        let message = String::from_utf8_lossy(&output.stderr).trim().to_string();
        Err(crate::utils::WindleError::Command {
            command: command.to_string(),
            message: if message.is_empty() {
                format!("exited with {}", output.status)
            } else {
                message
            },
        })
    }
}

/// [`run_tool`], but feeds `input` to the process on stdin. Used to pipe plist
/// output from one tool into `plutil`.
#[cfg(target_os = "macos")]
pub(crate) fn run_tool_stdin(
    command: &str,
    args: &[&str],
    input: &str,
) -> crate::utils::Result<String> {
    use std::io::Write;
    use std::process::{Command, Stdio};

    let mut child = Command::new(command)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;

    // Dropping stdin closes the pipe, which the child needs in order to finish.
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(input.as_bytes())?;
    }

    let output = child.wait_with_output()?;

    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        Err(crate::utils::WindleError::Command {
            command: command.to_string(),
            message: String::from_utf8_lossy(&output.stderr).trim().to_string(),
        })
    }
}

/// Read a plist (binary or XML) as JSON.
///
/// macOS ships most `Info.plist` files in the binary format, so instead of
/// pulling in a plist parser we let `plutil` normalise them for us.
#[cfg(target_os = "macos")]
pub(crate) fn read_plist(path: &std::path::Path) -> crate::utils::Result<serde_json::Value> {
    if !path.exists() {
        return Err(crate::utils::WindleError::NotFound(
            path.to_string_lossy().into_owned(),
        ));
    }

    let json = run_tool(
        "plutil",
        &["-convert", "json", "-o", "-", "--", &path.to_string_lossy()],
    )?;

    serde_json::from_str(&json).map_err(|error| crate::utils::WindleError::Command {
        command: "plutil".into(),
        message: format!("{}: {error}", path.display()),
    })
}

/// Convert plist text (as produced by `hdiutil -plist`) into JSON.
#[cfg(target_os = "macos")]
pub(crate) fn plist_text_to_json(plist: &str) -> crate::utils::Result<serde_json::Value> {
    let json = run_tool_stdin("plutil", &["-convert", "json", "-o", "-", "--", "-"], plist)?;

    serde_json::from_str(&json).map_err(|error| crate::utils::WindleError::Command {
        command: "plutil".into(),
        message: error.to_string(),
    })
}

/// Shorthand for pulling a string out of a parsed plist.
#[cfg(target_os = "macos")]
pub(crate) fn plist_string(value: &serde_json::Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
        .filter(|found| !found.is_empty())
}

