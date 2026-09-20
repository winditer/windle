//! The macOS task set — Spotlight, Launch Services, the Dock — and the `sudo`
//! elevation flow that caches the admin password so a whole batch only ever
//! shows one dialog.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use tauri::AppHandle;

use super::super::{plist_string, read_plist, run_tool, RiskLevel};
use super::{
    emit, escalate, execute_with, summarise, tool_name, LoginItem, LoginItemKind, OptimizeOutcome,
    OptimizeTask, OptimizeTaskId, TaskStatus,
};
use crate::utils::{format, permissions, Result, WindleError};

/// `lsregister` is not on `PATH`, it lives inside the LaunchServices framework.
const LSREGISTER: &str = "/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister";

/// The `periodic` binary that runs the daily/weekly/monthly maintenance
/// scripts. Apple removed the whole periodic framework in macOS 15
/// Sequoia, so on newer systems this path no longer exists.
const PERIODIC_BINARY: &str = "/usr/sbin/periodic";

/// Reported when `periodic` is gone and the maintenance-scripts task
/// skips itself. The frontend matches on this exact text, so it is a
/// cross-platform contract and must not be reworded.
const PERIODIC_SKIP_MESSAGE: &str =
    "Skipped: maintenance scripts are not available on this macOS version";

/// Cached admin password for batched elevated execution. The password is kept
/// in memory for 15 minutes after the first successful authorization so that
/// "Optimize All" only shows ONE password dialog instead of one per task.
static CACHED_PASSWORD: Mutex<Option<String>> = Mutex::new(None);
static LAST_AUTH_TIME: AtomicU64 = AtomicU64::new(0);
const AUTH_CACHE_SECS: u64 = 15 * 60;

/// One shell invocation belonging to a task.
struct Step {
    command: &'static str,
    args: Vec<&'static str>,
    /// Optional steps are nice-to-haves: a failure is reported but does not
    /// fail the task (for example the `killall` that needs root after a
    /// cache flush that already succeeded).
    required: bool,
    /// When `true` the step is run through `osascript … with administrator
    /// privileges`, which shows the system's native password dialog. Used for
    /// commands like `purge`, `mdutil`, `periodic` and `lsregister` that only
    /// succeed as root.
    elevated: bool,
}

fn step(command: &'static str, args: &[&'static str]) -> Step {
    Step {
        command,
        args: args.to_vec(),
        required: true,
        elevated: false,
    }
}

fn optional(command: &'static str, args: &[&'static str]) -> Step {
    Step {
        command,
        args: args.to_vec(),
        required: false,
        elevated: false,
    }
}

/// A required step that runs with macOS administrator privileges.
fn step_elevated(command: &'static str, args: &[&'static str]) -> Step {
    Step {
        command,
        args: args.to_vec(),
        required: true,
        elevated: true,
    }
}

/// An optional step that runs with macOS administrator privileges.
fn optional_elevated(command: &'static str, args: &[&'static str]) -> Step {
    Step {
        command,
        args: args.to_vec(),
        required: false,
        elevated: true,
    }
}

/// Run several tasks in order, one outcome per task. When multiple tasks
/// need elevation, the admin password is verified once up front (via a
/// trivial `true` command) so only ONE password dialog appears for the
/// entire run. The password is then cached for 15 minutes and reused by
/// every subsequent `run_elevated` call.
pub async fn run_tasks(app: &AppHandle, task_ids: Vec<OptimizeTaskId>) -> Vec<OptimizeOutcome> {
    let total = task_ids.len();

    // Everything queued is announced up front so the UI can render the whole
    // run before the first command starts.
    for (index, task_id) in task_ids.iter().enumerate() {
        emit(app, *task_id, TaskStatus::Pending, index, total, None);
    }

    // Pre-warm the auth cache: if any task needs elevation, verify the
    // admin password once so only ONE dialog appears for the whole run.
    if task_ids.iter().any(|id| needs_elevation(*id)) {
        let _ = tauri::async_runtime::spawn_blocking(|| {
            run_elevated_batch(&["true".to_string()])
        })
        .await;
        // If the pre-warm failed (user cancelled or wrong password), the
        // first elevated task will re-prompt. The error is not fatal here.
    }

    let mut outcomes = Vec::with_capacity(total);
    for (index, task_id) in task_ids.into_iter().enumerate() {
        outcomes.push(execute_with(app, task_id, index, total, run_task).await);
    }

    outcomes
}

/// Enable or disable a startup item.
pub fn set_login_item_enabled(item: &LoginItem, enabled: bool) -> Result<()> {
    let domain = match item.kind {
        LoginItemKind::LaunchAgent => format!("gui/{}", permissions::current_uid()),
        LoginItemKind::LaunchDaemon => {
            if !permissions::is_root() {
                return Err(WindleError::NeedsElevation(format!(
                    "changing the {} daemon",
                    item.label
                )));
            }
            "system".to_string()
        }
        // Classic login items live in a private database that only System
        // Settings may edit, so there is nothing safe to toggle here.
        LoginItemKind::LoginItem => {
            return Err(WindleError::Command {
                command: "launchctl".into(),
                message: format!(
                    "\"{}\" is a Login Item; remove it in System Settings › General › Login Items",
                    item.label
                ),
            })
        }
    };

    let action = if enabled { "enable" } else { "disable" };
    let target = format!("{domain}/{}", item.label);

    run_tool("launchctl", &[action, &target])
        .map(|_| ())
        .map_err(|error| escalate("launchctl", error))
}

/// Clear the cached admin password so the next elevated task re-prompts.
pub fn clear_auth() {
    *CACHED_PASSWORD.lock().unwrap() = None;
    LAST_AUTH_TIME.store(0, Ordering::Relaxed);
}

/// The shell work behind each task. Building the step list is split out so
/// tests can inspect which steps are elevated without running anything.
fn task_steps(task_id: OptimizeTaskId) -> Vec<Step> {
    match task_id {
        OptimizeTaskId::FlushDns => vec![
            step("dscacheutil", &["-flushcache"]),
            // Restarting the responder is what actually drops in-flight
            // answers, but it needs root.
            optional("killall", &["-HUP", "mDNSResponder"]),
        ],
        // Memory purge is handled in-process via `purge_memory_pressure()`,
        // which uses an allocation-pressure technique that needs no root.
        OptimizeTaskId::PurgeMemory => vec![],
        OptimizeTaskId::RebuildSpotlight => vec![
            // `-E` erases and rebuilds; indexing then continues in the
            // background, which is why the estimate is only a hint.
            step_elevated("mdutil", &["-E", "/"]),
            optional_elevated("mdutil", &["-i", "on", "/"]),
        ],
        // `lsregister` is not on `PATH`; running through `sudo` with the full
        // framework path resolves the binary and runs with the root it needs.
        OptimizeTaskId::RebuildLaunchServices => vec![step_elevated(
            LSREGISTER,
            &[
                "-r",
                "-domain",
                "local",
                "-domain",
                "system",
                "-domain",
                "user",
            ],
        )],
        // The Dock process also owns Mission Control and Launchpad, so a
        // single restart reloads all three.
        OptimizeTaskId::ResetDock => vec![step("killall", &["Dock"])],
        OptimizeTaskId::ClearQuicklook => vec![
            step("qlmanage", &["-r", "cache"]),
            optional("qlmanage", &["-r"]),
            // Font caches are the other half of "stale previews": a corrupt
            // one shows the wrong glyphs everywhere.
            optional("atsutil", &["databases", "-removeUser"]),
        ],
        // `periodic` is in `/usr/sbin` and needs root to write its logs; using
        // the full path works around the minimal PATH inside `sudo -S sh -c`.
        OptimizeTaskId::RunMaintenanceScripts => {
            vec![step_elevated(PERIODIC_BINARY, &["daily", "weekly", "monthly"])]
        }
        // Read-only First Aid: it reports problems but never writes.
        OptimizeTaskId::VerifyDisk => vec![step("diskutil", &["verifyVolume", "/"])],
    }
}

/// Whether a task will actually prompt for the admin password. The
/// maintenance-scripts task is the exception on macOS 15+: its `periodic`
/// binary was removed by Apple, so the task skips itself before ever
/// reaching the password dialog and must not trigger the batch pre-warm.
fn needs_elevation(task_id: OptimizeTaskId) -> bool {
    needs_elevation_inner(task_id, periodic_available())
}

/// Availability-injectable core of `needs_elevation`, so tests can cover
/// both branches on any host.
fn needs_elevation_inner(task_id: OptimizeTaskId, periodic_available: bool) -> bool {
    // Memory purge uses in-process allocation pressure — no admin needed.
    if task_id == OptimizeTaskId::PurgeMemory {
        return false;
    }
    if task_id == OptimizeTaskId::RunMaintenanceScripts && !periodic_available {
        return false;
    }
    task_steps(task_id).iter().any(|s| s.elevated)
}

/// Whether the `periodic` framework still exists on this system.
fn periodic_available() -> bool {
    tool_exists(PERIODIC_BINARY)
}

/// Whether the binary at `path` is present. Split out so tests can point
/// the check at a path that is guaranteed to be missing.
fn tool_exists(path: &str) -> bool {
    Path::new(path).exists()
}

fn run_task(task_id: OptimizeTaskId) -> Result<String> {
    run_task_inner(task_id, periodic_available())
}

/// Free memory by allocating a large block, forcing the OS to purge caches
/// and compress inactive pages, then freeing the block. This works without
/// root privileges — the same technique used by Tencent Lemon Cleanup and
/// other memory cleaners.
///
/// Returns the number of bytes freed (difference in available memory before
/// and after), or 0 if there was too little memory to bother.
fn purge_memory_pressure() -> Result<u64> {
    use std::alloc::{alloc, dealloc, Layout};
    use sysinfo::System;

    // Get available memory before the purge.
    let mut sys = System::new();
    sys.refresh_memory();
    let available = sys.available_memory(); // in bytes

    // Target: 85% of available memory, capped at 8GB to avoid excessive
    // allocation on machines with very large RAM.
    let target = std::cmp::min(
        (available as f64 * 0.85) as usize,
        8 * 1024 * 1024 * 1024,
    );

    if target < 64 * 1024 * 1024 {
        // Less than 64MB available — not worth the effort.
        return Ok(0);
    }

    // Allocate in 256MB chunks to handle allocation failures gracefully.
    let chunk_size = 256 * 1024 * 1024; // 256MB
    let mut chunks: Vec<(*mut u8, Layout)> = Vec::new();
    let mut allocated = 0usize;

    while allocated < target {
        let this_chunk = std::cmp::min(chunk_size, target - allocated);
        let layout = match Layout::from_size_align(this_chunk, 4096) {
            Ok(l) => l,
            Err(_) => break,
        };

        unsafe {
            let ptr = alloc(layout);
            if ptr.is_null() {
                // Can't allocate more — proceed to free what we have.
                break;
            }

            // Touch every page to force it resident in physical RAM. This is
            // what actually creates memory pressure: the OS must evict clean
            // file-cache pages and compress/swap inactive anonymous pages to
            // make room for our allocation.
            let mut offset = 0;
            while offset < this_chunk {
                std::ptr::write_volatile(ptr.add(offset), 1u8);
                offset += 4096; // page size
            }

            chunks.push((ptr, layout));
        }
        allocated += this_chunk;
    }

    // Free all chunks — the OS reclaims the purged cache and now has more
    // free memory than before.
    for (ptr, layout) in chunks {
        unsafe {
            dealloc(ptr, layout);
        }
    }

    // Brief pause to let the OS settle and reclaim memory.
    std::thread::sleep(std::time::Duration::from_millis(100));

    // Measure freed memory.
    sys.refresh_memory();
    let new_available = sys.available_memory();
    let freed = if new_available > available {
        new_available - available
    } else {
        0
    };

    Ok(freed)
}

/// Availability-injectable core of `run_task`, so tests can cover both
/// branches on any host.
fn run_task_inner(task_id: OptimizeTaskId, periodic_available: bool) -> Result<String> {
    // Memory purge uses an in-process allocation-pressure technique that
    // works without root privileges, so it bypasses the shell-step flow.
    if task_id == OptimizeTaskId::PurgeMemory {
        let freed = purge_memory_pressure()?;
        return Ok(if freed > 0 {
            format!("Freed {} of memory", format::bytes(freed))
        } else {
            "Memory already clean".into()
        });
    }

    // Apple removed the whole `periodic` framework in macOS 15 Sequoia and
    // shipped no replacement. Rather than prompt for a password only to
    // fail with "command not found", report the task as done-but-skipped.
    if task_id == OptimizeTaskId::RunMaintenanceScripts && !periodic_available {
        return Ok(PERIODIC_SKIP_MESSAGE.into());
    }

    let steps = task_steps(task_id);
    run_steps(&steps)
}

/// Run each step in order, returning the message to show the user.
fn run_steps(steps: &[Step]) -> Result<String> {
    let mut report = String::new();
    let mut notes: Vec<String> = Vec::new();

    for step in steps {
        // Elevated steps are routed through `osascript` so macOS shows its
        // native admin password dialog; everything else runs directly.
        let result = if step.elevated {
            let command = build_shell_command(step.command, &step.args);
            run_elevated(tool_name(step.command), &command)
        } else {
            run_tool(step.command, &step.args)
        };

        match result {
            Ok(output) => {
                // Keep the most recent tool that actually said something —
                // `diskutil` and `mdutil` both report their findings here.
                if let Some(line) = super::last_meaningful_line(&output) {
                    report = line;
                }
            }
            Err(error) => {
                if step.required {
                    return Err(escalate(step.command, error));
                }
                notes.push(format!(
                    "{} skipped ({})",
                    tool_name(step.command),
                    summarise(&error)
                ));
            }
        }
    }

    let mut message = if report.is_empty() {
        "Done".to_string()
    } else {
        report
    };

    if !notes.is_empty() {
        message.push_str(" — ");
        message.push_str(&notes.join(", "));
    }

    Ok(message)
}

/// Show a native macOS password dialog and return the entered password.
///
/// Uses `osascript`'s `display dialog … with hidden answer`, which is the
/// same secure entry field System Settings uses. Returns an error when the
/// user cancels the dialog.
fn prompt_for_password() -> Result<String> {
    let script = r#"display dialog "Windle 需要管理员密码来执行系统优化任务" & return & return & "请输入您的管理员密码：" default answer "" with hidden answer with title "Windle 系统优化" with icon caution"#;
    let output = std::process::Command::new("osascript")
        .arg("-e")
        .arg(script)
        .output()?;

    if !output.status.success() {
        return Err(WindleError::Command {
            command: "osascript".to_string(),
            message: "administrator privileges were not granted".into(),
        });
    }

    let text = String::from_utf8_lossy(&output.stdout);
    let password = text
        .split("text returned:")
        .nth(1)
        .map(|s| {
            // The osascript output is "text returned:PASSWORD, button returned:OK"
            // or "button returned:OK, text returned:PASSWORD"
            // Take only the password part before any comma
            s.split(',').next().unwrap_or(s).trim().to_string()
        })
        .filter(|s| !s.is_empty())
        .ok_or_else(|| WindleError::Command {
            command: "osascript".to_string(),
            message: "failed to parse password".into(),
        })?;

    Ok(password)
}

/// Run a shell command with macOS administrator privileges, using cached
/// authentication to avoid repeated password prompts within a 15-minute
/// window.
///
/// The first elevated call shows an `osascript` password dialog and caches
/// the result. Subsequent calls within 15 minutes reuse the cached password
/// via `sudo -S`, so no further dialogs appear. After 15 minutes the cache
/// expires and the next call re-prompts.
fn run_elevated(tool: &str, command: &str) -> Result<String> {
    let now = now_secs();

    // Try the cached password first.
    if let Some(ref password) = *CACHED_PASSWORD.lock().unwrap() {
        let last_auth = LAST_AUTH_TIME.load(Ordering::Relaxed);
        if last_auth > 0 && now.saturating_sub(last_auth) < AUTH_CACHE_SECS {
            if let Ok(output) = run_sudo(command, password) {
                if output.status.success() {
                    return Ok(filter_sudo_output(&String::from_utf8_lossy(
                        &output.stdout,
                    )));
                }
                // If the failure is NOT an auth issue, it's a real command
                // error — return it instead of re-prompting.
                let combined = combined_output(&output);
                if !is_auth_failure(&combined) {
                    return Err(WindleError::Command {
                        command: tool.to_string(),
                        message: filter_sudo_output(&combined).trim().to_string(),
                    });
                }
                // Auth failure — fall through to re-prompt.
            }
            // Clear the stale cache.
            *CACHED_PASSWORD.lock().unwrap() = None;
            LAST_AUTH_TIME.store(0, Ordering::Relaxed);
        }
    }

    // Prompt for a new password.
    let password = prompt_for_password()?;
    let now = now_secs(); // refresh after the blocking prompt

    // Cache immediately so subsequent elevated tasks reuse the same password
    // without showing another dialog — even if this command fails for non-auth reasons.
    *CACHED_PASSWORD.lock().unwrap() = Some(password.clone());
    LAST_AUTH_TIME.store(now, Ordering::Relaxed);

    let output = run_sudo(command, &password)?;

    if output.status.success() {
        Ok(filter_sudo_output(&String::from_utf8_lossy(
            &output.stdout,
        )))
    } else {
        let combined = combined_output(&output);
        // If this was an auth failure (wrong password), clear the cache so the
        // next elevated task re-prompts instead of reusing a bad password.
        if is_auth_failure(&combined) {
            *CACHED_PASSWORD.lock().unwrap() = None;
            LAST_AUTH_TIME.store(0, Ordering::Relaxed);
        }
        Err(WindleError::Command {
            command: tool.to_string(),
            message: filter_sudo_output(&combined).trim().to_string(),
        })
    }
}

/// Run multiple elevated commands in a single `sudo` call, returning one
/// output string per command. Only ONE password prompt is shown for the
/// entire batch, making this ideal for "Optimize All".
fn run_elevated_batch(commands: &[String]) -> Result<Vec<String>> {
    const SEP: &str = "___WINDLE_SEP___";
    let batch_script = commands
        .iter()
        .map(|cmd| cmd.as_str())
        .collect::<Vec<_>>()
        .join(&format!("; echo '{}'; ", SEP));

    let now = now_secs();

    // Try cached password first.
    if let Some(ref password) = *CACHED_PASSWORD.lock().unwrap() {
        let last_auth = LAST_AUTH_TIME.load(Ordering::Relaxed);
        if last_auth > 0 && now.saturating_sub(last_auth) < AUTH_CACHE_SECS {
            if let Ok(output) = run_sudo(&batch_script, password) {
                if output.status.success() {
                    let filtered =
                        filter_sudo_output(&String::from_utf8_lossy(&output.stdout));
                    return Ok(filtered
                        .split(SEP)
                        .map(|s| s.trim().to_string())
                        .collect());
                }
                let combined = combined_output(&output);
                if !is_auth_failure(&combined) {
                    return Err(WindleError::Command {
                        command: "sudo".to_string(),
                        message: filter_sudo_output(&combined).trim().to_string(),
                    });
                }
            }
            *CACHED_PASSWORD.lock().unwrap() = None;
            LAST_AUTH_TIME.store(0, Ordering::Relaxed);
        }
    }

    // Prompt for password.
    let password = prompt_for_password()?;
    let now = now_secs(); // refresh after the blocking prompt
    let output = run_sudo(&batch_script, &password)?;

    if output.status.success() {
        *CACHED_PASSWORD.lock().unwrap() = Some(password);
        LAST_AUTH_TIME.store(now, Ordering::Relaxed);
        let filtered = filter_sudo_output(&String::from_utf8_lossy(&output.stdout));
        Ok(filtered
            .split(SEP)
            .map(|s| s.trim().to_string())
            .collect())
    } else {
        let combined = combined_output(&output);
        Err(WindleError::Command {
            command: "sudo".to_string(),
            message: filter_sudo_output(&combined).trim().to_string(),
        })
    }
}

/// Current Unix timestamp in seconds.
fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// Run `command` as root by piping `password` to `sudo -S -k`.
///
/// `-S` reads the password from stdin; `-k` resets sudo's own timestamp so
/// we always authenticate with our password rather than a stale sudo session.
fn run_sudo(command: &str, password: &str) -> std::io::Result<std::process::Output> {
    use std::io::Write;
    use std::process::{Command, Stdio};

    let mut child = Command::new("sudo")
        .args(["-S", "-k", "sh", "-c", command])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;

    if let Some(mut stdin) = child.stdin.take() {
        writeln!(stdin, "{}", password)?;
    }
    child.wait_with_output()
}

/// Strip sudo's "Password:" prompt and blank lines from command output.
fn filter_sudo_output(output: &str) -> String {
    output
        .lines()
        .filter(|line| {
            let trimmed = line.trim();
            !trimmed.is_empty() && !trimmed.starts_with("Password:")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Combine stdout and stderr from a process output into a single string.
fn combined_output(output: &std::process::Output) -> String {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    if stderr.is_empty() {
        stdout.into_owned()
    } else {
        format!("{stdout}\n{stderr}")
    }
}

/// Whether a command's output looks like a sudo authentication failure
/// (wrong password) rather than a real command error.
fn is_auth_failure(output: &str) -> bool {
    let lower = output.to_lowercase();
    lower.contains("sorry, try again")
        || lower.contains("incorrect password")
        || lower.contains("a password is required")
        || lower.contains("no password was provided")
}

/// Join a command and its args into a single shell command string, quoting
/// any argument that contains shell metacharacters so `do shell script`
/// receives it intact.
fn build_shell_command(command: &str, args: &[&str]) -> String {
    std::iter::once(command)
        .chain(args.iter().copied())
        .map(shell_quote)
        .collect::<Vec<_>>()
        .join(" ")
}

/// Single-quote a string for `/bin/sh` when it contains anything other than
/// plain word characters, slashes and dashes.
fn shell_quote(s: &str) -> String {
    let needs_quoting = s.is_empty()
        || s.contains(|c: char| {
            c.is_whitespace()
                || c == '"'
                || c == '\''
                || c == '\\'
                || c == '$'
                || c == '`'
        });

    if needs_quoting {
        format!("'{}'", s.replace('\'', "'\\''"))
    } else {
        s.to_string()
    }
}

/// Every launch agent, daemon and classic login item we can see, sorted by
/// label and de-duplicated by path.
pub fn startup_items() -> Vec<LoginItem> {
    let home = permissions::home_dir();

    let sources: [(PathBuf, LoginItemKind); 3] = [
        (home.join("Library/LaunchAgents"), LoginItemKind::LaunchAgent),
        (
            PathBuf::from("/Library/LaunchAgents"),
            LoginItemKind::LaunchAgent,
        ),
        (
            PathBuf::from("/Library/LaunchDaemons"),
            LoginItemKind::LaunchDaemon,
        ),
    ];

    let disabled_agents = disabled_labels(&format!("gui/{}", permissions::current_uid()));
    let disabled_daemons = disabled_labels("system");

    // Keyed by path so a job listed twice collapses into one row.
    let mut items: BTreeMap<String, LoginItem> = BTreeMap::new();

    for (directory, kind) in sources {
        let Ok(entries) = std::fs::read_dir(&directory) else {
            continue;
        };

        for entry in entries.filter_map(std::result::Result::ok) {
            let path = entry.path();
            if format::extension(&path) != "plist" {
                continue;
            }

            let Some(item) = read_job(&path, kind, &disabled_agents, &disabled_daemons) else {
                continue;
            };

            items.insert(item.id.clone(), item);
        }
    }

    for item in classic_login_items() {
        items.insert(item.id.clone(), item);
    }

    let mut items: Vec<LoginItem> = items.into_values().collect();
    items.sort_by(|a, b| a.label.to_lowercase().cmp(&b.label.to_lowercase()));
    items
}

/// Read one launchd job description.
fn read_job(
    path: &Path,
    kind: LoginItemKind,
    disabled_agents: &HashSet<String>,
    disabled_daemons: &HashSet<String>,
) -> Option<LoginItem> {
    let plist = read_plist(path).ok();

    let label = plist
        .as_ref()
        .and_then(|plist| plist_string(plist, "Label"))
        // The file name mirrors the label by convention, so it is a safe
        // fallback for a plist we could not parse.
        .unwrap_or_else(|| format::file_name(path).trim_end_matches(".plist").to_string());

    if label.is_empty() {
        return None;
    }

    // What the job actually launches, which is the useful thing to show.
    let program = plist
        .as_ref()
        .and_then(|plist| {
            plist_string(plist, "Program").or_else(|| {
                plist
                    .get("ProgramArguments")
                    .and_then(serde_json::Value::as_array)
                    .and_then(|arguments| arguments.first())
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string)
            })
        })
        .unwrap_or_else(|| path.to_string_lossy().into_owned());

    // `launchctl` overrides win over the plist's own `Disabled` key.
    let disabled_here = match kind {
        LoginItemKind::LaunchDaemon => disabled_daemons.contains(&label),
        _ => disabled_agents.contains(&label),
    };

    let disabled_in_plist = plist
        .as_ref()
        .and_then(|plist| plist.get("Disabled"))
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);

    Some(LoginItem {
        id: path.to_string_lossy().into_owned(),
        label: label.clone(),
        path: program,
        kind,
        enabled: !(disabled_here || disabled_in_plist),
        is_system: label.starts_with("com.apple.") || path.starts_with("/System"),
    })
}

/// Login items from System Settings › General › Login Items.
///
/// Reading them needs Automation access, so a refusal is treated as "none
/// found" rather than failing the whole listing.
fn classic_login_items() -> Vec<LoginItem> {
    let script = "tell application \"System Events\" to get the path of every login item";

    let Ok(output) = run_tool("osascript", &["-e", script]) else {
        return Vec::new();
    };

    output
        .trim()
        .split(", ")
        .map(str::trim)
        .filter(|path| !path.is_empty())
        .map(|path| {
            let target = PathBuf::from(path);

            LoginItem {
                id: path.to_string(),
                label: format::file_name(&target)
                    .trim_end_matches(".app")
                    .to_string(),
                path: path.to_string(),
                kind: LoginItemKind::LoginItem,
                // Anything still listed here runs at login.
                enabled: true,
                is_system: target.starts_with("/System"),
            }
        })
        .collect()
}

/// Labels that `launchctl` has been told not to load in `domain`.
fn disabled_labels(domain: &str) -> HashSet<String> {
    let Ok(output) = run_tool("launchctl", &["print-disabled", domain]) else {
        return HashSet::new();
    };

    parse_disabled(&output)
}

/// Parse `"com.example.job" => disabled` lines. Older releases print `true`
/// and `false` instead of `disabled` and `enabled`.
fn parse_disabled(output: &str) -> HashSet<String> {
    output
        .lines()
        .filter_map(|line| {
            let (label, state) = line.split_once("=>")?;
            let label = label.trim().trim_matches('"').to_string();
            let state = state.trim().trim_end_matches(',');

            (state == "true" || state == "disabled").then_some(label)
        })
        .filter(|label| !label.is_empty())
        .collect()
}

pub fn catalogue() -> Vec<OptimizeTask> {
    // macOS 15+ removed `periodic` entirely, so the maintenance-scripts
    // task skips itself instantly and never needs admin privileges.
    let periodic = periodic_available();

    vec![
        OptimizeTask {
            id: OptimizeTaskId::PurgeMemory,
            label: "Purge inactive memory".into(),
            description: "Force the kernel to release cached pages back to the free pool.".into(),
            risk: RiskLevel::Safe,
            requires_elevation: false,
            estimated_seconds: 10,
        },
        OptimizeTask {
            id: OptimizeTaskId::FlushDns,
            label: "Flush DNS cache".into(),
            description: "Clear resolved hostnames after a network or VPN change.".into(),
            risk: RiskLevel::Safe,
            requires_elevation: true,
            estimated_seconds: 2,
        },
        OptimizeTask {
            id: OptimizeTaskId::RebuildSpotlight,
            label: "Rebuild Spotlight index".into(),
            description: "Fixes bad search results. Indexing runs for a while afterwards.".into(),
            risk: RiskLevel::Caution,
            requires_elevation: true,
            estimated_seconds: 30,
        },
        OptimizeTask {
            id: OptimizeTaskId::RebuildLaunchServices,
            label: "Rebuild Launch Services".into(),
            description: "Repairs duplicate or wrong \"Open With\" entries.".into(),
            risk: RiskLevel::Safe,
            requires_elevation: true,
            estimated_seconds: 20,
        },
        OptimizeTask {
            id: OptimizeTaskId::ResetDock,
            label: "Restart the Dock".into(),
            description: "Reloads the Dock and Mission Control when they misbehave.".into(),
            risk: RiskLevel::Safe,
            requires_elevation: false,
            estimated_seconds: 3,
        },
        OptimizeTask {
            id: OptimizeTaskId::ClearQuicklook,
            label: "Reset preview and font caches".into(),
            description: "Clears stale Quick Look thumbnails and rebuilds the user font cache."
                .into(),
            risk: RiskLevel::Safe,
            requires_elevation: false,
            estimated_seconds: 5,
        },
        OptimizeTask {
            id: OptimizeTaskId::RunMaintenanceScripts,
            label: "Run maintenance scripts".into(),
            description: "Runs the daily, weekly and monthly periodic scripts now.".into(),
            risk: RiskLevel::Safe,
            requires_elevation: periodic,
            estimated_seconds: if periodic { 60 } else { 1 },
        },
        OptimizeTask {
            id: OptimizeTaskId::VerifyDisk,
            label: "Verify the startup disk".into(),
            description: "Runs a read-only First Aid check on the boot volume.".into(),
            risk: RiskLevel::Caution,
            requires_elevation: true,
            estimated_seconds: 120,
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_task_has_shell_work_behind_it() {
        for task in catalogue() {
            assert!(!task.label.is_empty());
            assert!(!task.description.is_empty());

            // PurgeMemory is handled in-process via `purge_memory_pressure()`,
            // so it legitimately has no shell steps.
            if task.id == OptimizeTaskId::PurgeMemory {
                continue;
            }

            // `task_steps` builds the step list before running anything, so an
            // unhandled id would show up as an empty task here.
            let steps = task_steps(task.id);
            assert!(!steps.is_empty(), "{} has no steps", task.label);
        }
    }

    #[test]
    fn elevated_tasks_route_through_admin_privileges() {
        // These tasks need root: every step they contain must be marked
        // `elevated` so the osascript admin prompt is shown instead of a bare
        // permission error. PurgeMemory is excluded because it now uses an
        // in-process technique that needs no elevation.
        for id in [
            OptimizeTaskId::RebuildSpotlight,
            OptimizeTaskId::RebuildLaunchServices,
            OptimizeTaskId::RunMaintenanceScripts,
        ] {
            let steps = task_steps(id);
            assert!(
                !steps.is_empty() && steps.iter().all(|s| s.elevated),
                "{id:?} should have only elevated steps"
            );
        }
    }

    #[test]
    fn non_elevated_tasks_do_not_prompt() {
        // Tasks that work as a regular user must not trigger the admin dialog.
        // PurgeMemory uses an in-process allocation-pressure technique.
        for id in [
            OptimizeTaskId::PurgeMemory,
            OptimizeTaskId::FlushDns,
            OptimizeTaskId::ResetDock,
            OptimizeTaskId::ClearQuicklook,
        ] {
            let steps = task_steps(id);
            assert!(
                steps.iter().all(|s| !s.elevated),
                "{id:?} should not have elevated steps"
            );
        }
    }

    #[test]
    fn build_shell_command_joins_simple_args() {
        assert_eq!(build_shell_command("purge", &[]), "purge");
        assert_eq!(build_shell_command("mdutil", &["-E", "/"]), "mdutil -E /");
        assert_eq!(
            build_shell_command("/usr/sbin/periodic", &["daily", "weekly", "monthly"]),
            "/usr/sbin/periodic daily weekly monthly"
        );
    }

    #[test]
    fn shell_quote_leaves_plain_words_alone() {
        assert_eq!(shell_quote("purge"), "purge");
        assert_eq!(shell_quote("-E"), "-E");
        assert_eq!(shell_quote("/"), "/");
        // A path with slashes but no metacharacters is safe bare.
        assert_eq!(
            shell_quote("/System/Library/Support/lsregister"),
            "/System/Library/Support/lsregister"
        );
    }

    #[test]
    fn shell_quote_wraps_special_args() {
        assert_eq!(shell_quote(""), "''");
        assert_eq!(shell_quote("with space"), "'with space'");
        assert_eq!(shell_quote("it's"), "'it'\\''s'");
    }

    #[test]
    fn reads_both_spellings_of_launchctl_overrides() {
        let output = "\
com.apple.something => false
\"com.example.updater\" => true
\"com.example.helper\" => disabled
\"com.example.agent\" => enabled";

        let disabled = parse_disabled(output);

        assert!(disabled.contains("com.example.updater"));
        assert!(disabled.contains("com.example.helper"));
        assert!(!disabled.contains("com.example.agent"));
        assert!(!disabled.contains("com.apple.something"));
    }

    #[test]
    fn optional_steps_are_reported_but_do_not_fail_the_task() {
        // `false` always exits non-zero; `true` always succeeds.
        let steps = vec![step("true", &[]), optional("false", &[])];

        let message = run_steps(&steps).expect("optional failure must not fail the task");
        assert!(message.contains("false skipped"), "got {message}");
    }

    #[test]
    fn a_required_step_failing_fails_the_task() {
        let steps = vec![optional("true", &[]), step("false", &[])];

        assert!(run_steps(&steps).is_err());
    }

    #[test]
    fn startup_items_are_well_formed_on_this_machine() {
        // Machine-dependent, so this only checks the shape of what we read
        // rather than a specific set of jobs.
        for item in startup_items() {
            assert!(!item.label.is_empty(), "every item needs a label");
            assert!(!item.id.is_empty(), "every item needs an id");
            assert!(!item.path.is_empty(), "every item needs a program path");

            if item.label.starts_with("com.apple.") {
                assert!(item.is_system, "{} should be marked system", item.label);
            }
        }
    }

    #[test]
    fn the_last_line_of_output_becomes_the_message() {
        let steps = vec![step("echo", &["all good"])];

        assert_eq!(run_steps(&steps).unwrap(), "all good");
    }

    #[test]
    fn lsregister_step_omits_kill_flag() {
        let steps = task_steps(OptimizeTaskId::RebuildLaunchServices);
        assert!(
            steps.iter().all(|s| !s.args.contains(&"-kill")),
            "lsregister should not use -kill (removed in modern macOS)"
        );
    }

    #[test]
    fn periodic_step_uses_full_path() {
        let steps = task_steps(OptimizeTaskId::RunMaintenanceScripts);
        assert!(
            steps.iter().all(|s| s.command == "/usr/sbin/periodic"),
            "periodic should use full path /usr/sbin/periodic"
        );
    }

    #[test]
    fn a_missing_periodic_binary_is_skipped_not_failed() {
        // A path that cannot exist: this mirrors macOS 15+, where Apple
        // removed /usr/sbin/periodic entirely.
        assert!(
            !tool_exists("/nonexistent/windle-test/periodic"),
            "missing binary must be detected"
        );
        assert!(
            tool_exists("/bin/sh"),
            "an existing binary must be found"
        );
    }

    #[test]
    fn maintenance_scripts_task_skips_when_periodic_is_missing() {
        // The task must finish as a success with the exact skip note,
        // never reaching the password dialog or `run_elevated`. The
        // availability flag is injected, so this holds on any host.
        let message = run_task_inner(OptimizeTaskId::RunMaintenanceScripts, false)
            .expect("skip must not be an error");
        assert_eq!(message, PERIODIC_SKIP_MESSAGE);
    }

    #[test]
    fn maintenance_scripts_task_never_prompts_when_periodic_is_missing() {
        assert!(
            !needs_elevation_inner(OptimizeTaskId::RunMaintenanceScripts, false),
            "a skipped task must not trigger the admin pre-warm"
        );
        // With the binary present the elevated routing is kept.
        assert!(needs_elevation_inner(OptimizeTaskId::RunMaintenanceScripts, true));
        // Other tasks are unaffected by the availability flag.
        assert!(needs_elevation_inner(OptimizeTaskId::RebuildSpotlight, false));
        // A non-elevated task never prompts regardless.
        assert!(!needs_elevation_inner(OptimizeTaskId::ResetDock, false));
        // PurgeMemory uses in-process pressure, never needs admin.
        assert!(!needs_elevation_inner(OptimizeTaskId::PurgeMemory, false));
        assert!(!needs_elevation_inner(OptimizeTaskId::PurgeMemory, true));
    }

    #[test]
    fn purge_memory_does_not_require_elevation() {
        let task = catalogue()
            .into_iter()
            .find(|t| t.id == OptimizeTaskId::PurgeMemory)
            .expect("catalogue lists the purge memory task");
        assert!(
            !task.requires_elevation,
            "purge memory should not require elevation"
        );
        assert!(!needs_elevation(OptimizeTaskId::PurgeMemory));
    }

    #[test]
    fn catalogue_reflects_periodic_availability() {
        let task = catalogue()
            .into_iter()
            .find(|t| t.id == OptimizeTaskId::RunMaintenanceScripts)
            .expect("catalogue lists the maintenance scripts task");

        assert_eq!(
            task.requires_elevation,
            periodic_available(),
            "requires_elevation must track whether periodic exists"
        );
        if !periodic_available() {
            assert!(
                task.estimated_seconds <= 1,
                "a skipped task should be near-instant"
            );
        }
    }

    #[test]
    fn filter_sudo_output_strips_password_prompt() {
        let input = "Password:\nreal output\n";
        assert_eq!(filter_sudo_output(input), "real output");
    }

    #[test]
    fn filter_sudo_output_keeps_meaningful_lines() {
        let input = "Password:\nline1\n\nline2\n";
        assert_eq!(filter_sudo_output(input), "line1\nline2");
    }

    #[test]
    fn filter_sudo_output_handles_empty_output() {
        assert_eq!(filter_sudo_output("Password:\n"), "");
        assert_eq!(filter_sudo_output(""), "");
    }

    #[test]
    fn is_auth_failure_detects_sudo_errors() {
        assert!(is_auth_failure("Sorry, try again."));
        assert!(is_auth_failure("sudo: 1 incorrect password attempt"));
        assert!(is_auth_failure("sudo: a password is required"));
        assert!(is_auth_failure("sudo: no password was provided"));
        assert!(!is_auth_failure("command not found"));
        assert!(!is_auth_failure("operation not permitted"));
    }

    #[test]
    fn clear_auth_resets_cache() {
        // Set some state.
        *CACHED_PASSWORD.lock().unwrap() = Some("test".into());
        LAST_AUTH_TIME.store(999, Ordering::Relaxed);

        clear_auth();

        assert!(CACHED_PASSWORD.lock().unwrap().is_none());
        assert_eq!(LAST_AUTH_TIME.load(Ordering::Relaxed), 0);
    }
}
