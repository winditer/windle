//! The Windows task set, and the elevated helper that runs it.
//!
//! Elevation is the structural difference from macOS: a process cannot gain
//! administrator rights in place, so everything that needs them is handed to a
//! second copy of this binary started through the UAC prompt
//! (`--elevated-batch <result-file> <ids>`). The helper appends one JSON line
//! per status change to the result file and the parent polls it, which is what
//! keeps the progress UI identical to the macOS path.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tauri::AppHandle;
use winreg::enums::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_READ, KEY_WRITE, REG_BINARY};
use winreg::{RegKey, RegValue};

use super::super::{run_tool, RiskLevel};
use super::{
    emit, escalate, execute_with, slug, summarise, tool_name, LoginItem, LoginItemKind,
    OptimizeOutcome, OptimizeTask, OptimizeTaskId, TaskStatus,
};
use crate::utils::{format, fs_ops, permissions, platform, Result, WindleError};

/// Command-line flag that marks the elevated half of a batch.
const ELEVATED_FLAG: &str = "--elevated-batch";

/// `SW_HIDE`: the helper runs in the background, nothing should appear on screen.
const SW_HIDE: i32 = 0;

/// `HRESULT_FROM_WIN32(ERROR_CANCELLED)`, which is what the shell reports when
/// the UAC prompt is dismissed.
const CANCELLED: u32 = 0x8007_04C7;

/// How often the parent looks for new lines in the helper's progress file.
const POLL_INTERVAL: Duration = Duration::from_millis(120);

/// One piece of work inside a task.
enum Action {
    /// A console command. Inside the elevated helper it runs with administrator
    /// rights; in-process it runs as the current user.
    Run {
        command: &'static str,
        args: Vec<String>,
        optional: bool,
    },
    /// Empty a directory through the guard rails, counting the bytes freed.
    /// Always best-effort: a file another program holds open simply stays.
    Empty { dir: PathBuf },
    /// Remove a directory through the guard rails.
    Remove { dir: PathBuf },
}

/// What a task runs, plus the message to show when nothing reported back.
struct Plan {
    actions: Vec<Action>,
    done: &'static str,
}

fn cmd(command: &'static str, args: &[&str]) -> Action {
    Action::Run {
        command,
        args: args.iter().map(|arg| (*arg).to_string()).collect(),
        optional: false,
    }
}

/// A command whose failure is a footnote rather than a task failure, such as
/// stopping a service that is not running.
fn soft_cmd(command: &'static str, args: &[&str]) -> Action {
    Action::Run {
        command,
        args: args.iter().map(|arg| (*arg).to_string()).collect(),
        optional: true,
    }
}

/// A whole directory emptied through the guard rails, e.g. `%TEMP%`. The
/// directory itself survives.
fn empty(entry: &str) -> Action {
    Action::Empty {
        dir: permissions::expand(entry),
    }
}

/// A directory removed through the guard rails, e.g. the search index.
fn remove(entry: &str) -> Action {
    Action::Remove {
        dir: permissions::expand(entry),
    }
}

/// What a task runs. Every entry is either a Windows tool or a scratch
/// directory the OS expects to be trimmed.
fn plan(task_id: OptimizeTaskId) -> Plan {
    match task_id {
        // Handled in-process by `trim_working_sets()`.
        OptimizeTaskId::PurgeMemory => Plan {
            actions: Vec::new(),
            done: "Memory already clean",
        },
        OptimizeTaskId::FlushDns => Plan {
            actions: vec![cmd("ipconfig", &["/flushdns"])],
            done: "DNS cache flushed",
        },
        OptimizeTaskId::ClearTempFiles => Plan {
            actions: vec![
                empty("%TEMP%"),
                empty("%SystemRoot%\\Temp"),
                empty("%ProgramData%\\Temp"),
            ],
            done: "Temporary files cleared",
        },
        OptimizeTaskId::ClearUpdateCache => Plan {
            actions: vec![
                // The update service keeps the download folder locked while it
                // runs, so the stop is best-effort — the start puts things back
                // either way.
                soft_cmd("net", &["stop", "wuauserv"]),
                soft_cmd("net", &["stop", "bits"]),
                empty("%SystemRoot%\\SoftwareDistribution\\Download"),
                empty("%SystemRoot%\\SoftwareDistribution\\DeliveryOptimization"),
                soft_cmd("net", &["start", "wuauserv"]),
                soft_cmd("net", &["start", "bits"]),
            ],
            done: "Windows Update cache cleared",
        },
        OptimizeTaskId::RebuildSearchIndex => Plan {
            actions: vec![
                soft_cmd("net", &["stop", "wsearch"]),
                // Deleting the database is what the "Rebuild" button in
                // Indexing Options does; Windows builds it again from the files
                // it can see.
                remove("%ProgramData%\\Microsoft\\Search\\Data"),
                soft_cmd("net", &["start", "wsearch"]),
            ],
            done: "Search index rebuilt",
        },
        OptimizeTaskId::ResetIconCache => Plan {
            actions: vec![cmd("ie4uinit.exe", &["-show"])],
            done: "Icon cache refreshed",
        },
        OptimizeTaskId::VerifySystemFiles => Plan {
            actions: vec![cmd("sfc", &["/scannow"])],
            done: "System file check finished",
        },
        OptimizeTaskId::RepairSystemImage => Plan {
            actions: vec![cmd("dism", &["/Online", "/Cleanup-Image", "/RestoreHealth"])],
            done: "System image repaired",
        },
        OptimizeTaskId::OptimizeSystemDrive => {
            let drive = system_drive();
            Plan {
                // `/O` picks the right work for the media: TRIM on an SSD,
                // defragmentation on a hard drive.
                actions: vec![cmd("defrag", &[&drive, "/O"])],
                done: "System drive optimized",
            }
        }
        OptimizeTaskId::CheckSystemDrive => {
            let drive = system_drive();
            Plan {
                // `/scan` is the online, read-only check: no reboot, no repair.
                actions: vec![cmd("chkdsk", &[&drive, "/scan"])],
                done: "Disk check finished",
            }
        }
    }
}

/// `C:` — the volume name `defrag` and `chkdsk` expect.
fn system_drive() -> String {
    permissions::boot_mount_point()
        .to_string_lossy()
        .trim_end_matches(platform::SEPARATORS)
        .to_string()
}

/// Whether this task can only do its job with administrator rights.
pub fn needs_elevation(task_id: OptimizeTaskId) -> bool {
    matches!(
        task_id,
        OptimizeTaskId::ClearTempFiles
            | OptimizeTaskId::ClearUpdateCache
            | OptimizeTaskId::RebuildSearchIndex
            | OptimizeTaskId::VerifySystemFiles
            | OptimizeTaskId::RepairSystemImage
            | OptimizeTaskId::OptimizeSystemDrive
            | OptimizeTaskId::CheckSystemDrive
    )
}

fn run_task(task_id: OptimizeTaskId) -> Result<String> {
    if task_id == OptimizeTaskId::PurgeMemory {
        return trim_working_sets();
    }

    run_plan(&plan(task_id))
}

/// Run every action in order and compose the message the UI shows.
fn run_plan(plan: &Plan) -> Result<String> {
    let mut freed = 0u64;
    let mut report: Option<String> = None;
    let mut notes: Vec<String> = Vec::new();

    for action in &plan.actions {
        match action {
            Action::Run {
                command,
                args,
                optional,
            } => {
                let borrowed: Vec<&str> = args.iter().map(String::as_str).collect();

                match run_tool(command, &borrowed) {
                    Ok(output) => {
                        // Keep the most recent tool that said something: `sfc`
                        // and `chkdsk` both report their findings here.
                        if let Some(line) = super::last_meaningful_line(&output) {
                            report = Some(line);
                        }
                    }
                    Err(error) => {
                        if !optional {
                            return Err(escalate(command, error));
                        }
                        notes.push(format!(
                            "{} skipped ({})",
                            tool_name(command),
                            summarise(&error)
                        ));
                    }
                }
            }
            Action::Empty { dir } => {
                if !dir.is_dir() {
                    continue;
                }

                let (bytes, failures) = fs_ops::empty_directory(dir, fs_ops::RemoveMode::Permanent);
                freed += bytes;
                if !failures.is_empty() {
                    // Files another program holds open simply stay put.
                    notes.push(format!(
                        "{} in use in {}",
                        item_count(failures.len()),
                        format::tilde(dir)
                    ));
                }
            }
            Action::Remove { dir } => {
                if !dir.exists() {
                    continue;
                }

                match fs_ops::remove(dir, fs_ops::RemoveMode::Permanent) {
                    Ok(bytes) => freed += bytes,
                    Err(error) => return Err(escalate(&display_name(dir), error)),
                }
            }
        }
    }

    Ok(compose(freed, report, plan.done, &notes))
}

/// Build the one-line message from whatever the actions reported.
fn compose(freed: u64, report: Option<String>, done: &str, notes: &[String]) -> String {
    let freed = (freed > 0).then(|| format!("Freed {}", format::bytes(freed)));

    let mut message = match (freed, report) {
        (Some(freed), Some(report)) => format!("{freed} · {report}"),
        (Some(freed), None) => freed,
        (None, Some(report)) => report,
        (None, None) => done.to_string(),
    };

    if !notes.is_empty() {
        message.push_str(" — ");
        message.push_str(&notes.join(", "));
    }

    message
}

fn item_count(count: usize) -> String {
    if count == 1 {
        "1 item".to_string()
    } else {
        format!("{count} items")
    }
}

/// The last path component, for naming a directory in a message.
fn display_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "remove".to_string())
}

/// Ask Windows to trim the working set of every process it can, which is the
/// closest thing it has to macOS's memory purge: cached pages go back to the
/// free pool. Processes owned by other users refuse without elevation, and
/// that refusal is simply skipped.
fn trim_working_sets() -> Result<String> {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
        TH32CS_SNAPPROCESS,
    };
    use windows::Win32::System::ProcessStatus::EmptyWorkingSet;
    use windows::Win32::System::Threading::{
        OpenProcess, PROCESS_QUERY_INFORMATION, PROCESS_SET_QUOTA,
    };

    let mut system = sysinfo::System::new();
    system.refresh_memory();
    let available = system.available_memory();

    // SAFETY: the snapshot and each process handle are opened and closed inside
    // this block, and `entry` is sized before the first call as the API requires.
    unsafe {
        let Ok(snapshot) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else {
            return Err(WindleError::Command {
                command: "trim-memory".into(),
                message: "the process list could not be read".into(),
            });
        };

        let mut entry = PROCESSENTRY32W {
            dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };

        if Process32FirstW(snapshot, &mut entry).is_ok() {
            loop {
                if let Ok(process) = OpenProcess(
                    PROCESS_QUERY_INFORMATION | PROCESS_SET_QUOTA,
                    false,
                    entry.th32ProcessID,
                ) {
                    // Best effort: protected and system processes refuse this.
                    let _ = EmptyWorkingSet(process);
                    let _ = CloseHandle(process);
                }

                if Process32NextW(snapshot, &mut entry).is_err() {
                    break;
                }
            }
        }

        let _ = CloseHandle(snapshot);
    }

    // Give the OS a moment to reclaim before measuring.
    std::thread::sleep(Duration::from_millis(100));
    system.refresh_memory();

    let freed = system.available_memory().saturating_sub(available);
    Ok(if freed > 0 {
        format!("Freed {} of memory", format::bytes(freed))
    } else {
        "Memory already clean".into()
    })
}

/// Run several tasks, one outcome per task, in the order given. Everything a
/// standard user can do runs in-process; the rest goes to one elevated helper,
/// so the batch costs exactly one UAC prompt.
pub async fn run_tasks(app: &AppHandle, task_ids: Vec<OptimizeTaskId>) -> Vec<OptimizeOutcome> {
    let total = task_ids.len();

    // Everything queued is announced up front so the UI can render the whole
    // run before the first command starts.
    for (index, task_id) in task_ids.iter().enumerate() {
        emit(app, *task_id, TaskStatus::Pending, index, total, None);
    }

    let mut outcomes: Vec<Option<OptimizeOutcome>> = (0..total).map(|_| None).collect();
    let mut elevated: Vec<(usize, OptimizeTaskId)> = Vec::new();

    // The tasks that need no administrator go first: the work starts
    // immediately and the UAC prompt comes after visible progress.
    for (index, task_id) in task_ids.iter().enumerate() {
        if needs_elevation(*task_id) {
            elevated.push((index, *task_id));
            continue;
        }

        outcomes[index] = Some(execute_with(app, *task_id, index, total, run_task).await);
    }

    if !elevated.is_empty() {
        if permissions::is_root() {
            // Already running as administrator: no prompt and no helper needed.
            for (index, task_id) in &elevated {
                outcomes[*index] = Some(execute_with(app, *task_id, *index, total, run_task).await);
            }
        } else {
            let helper_app = app.clone();
            let helper_tasks = elevated.clone();
            let results = tauri::async_runtime::spawn_blocking(move || {
                run_helper(&helper_app, &helper_tasks, total)
            })
            .await
            .unwrap_or_else(|error| {
                failed(&elevated, format!("the elevated helper could not be started ({error})"))
            });

            for (index, task_id, result) in results {
                outcomes[index] = Some(OptimizeOutcome {
                    task_id,
                    succeeded: result.succeeded,
                    message: result.message,
                    duration_ms: result.duration_ms,
                });
            }
        }
    }

    outcomes.into_iter().flatten().collect()
}

// ---------------------------------------------------------------------------
// The elevated helper
//
// `ShellExecuteExW` with the `runas` verb is the only way to raise the UAC
// prompt, and an elevated process cannot be handed our pipes, so it reports
// back through a file: one JSON line per status change, appended and flushed.
// The parent polls that file, forwards the lines as progress events, and reads
// the last line of each task as its result.
// ---------------------------------------------------------------------------

/// One line of the helper's progress file.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct HelperLine {
    task_id: OptimizeTaskId,
    status: TaskStatus,
    message: Option<String>,
    duration_ms: Option<u64>,
}

/// One task's result, as the helper reported it.
struct HelperResult {
    succeeded: bool,
    message: String,
    duration_ms: u64,
}

impl HelperResult {
    fn failed(message: impl Into<String>) -> Self {
        Self {
            succeeded: false,
            message: message.into(),
            duration_ms: 0,
        }
    }
}

/// Undo [`super::slug`] — `"purge-memory"` back to the enum, or `None` when the
/// name is not one this build knows.
fn parse_slug(slug: &str) -> Option<OptimizeTaskId> {
    if slug.is_empty() {
        return None;
    }

    serde_json::from_value(serde_json::Value::String(slug.to_string())).ok()
}

/// Run when this process was started as the elevated half of a batch:
/// `windle.exe --elevated-batch <result-file> <id,id,…>`. Returns the process
/// exit code, or `None` when this is an ordinary launch.
pub fn elevated_entrypoint(mut args: impl Iterator<Item = String>) -> Option<i32> {
    if args.nth(1).as_deref() != Some(ELEVATED_FLAG) {
        return None;
    }

    let (Some(path), Some(ids)) = (args.next(), args.next()) else {
        return Some(2);
    };

    let task_ids: Vec<OptimizeTaskId> = ids.split(',').filter_map(parse_slug).collect();

    let Ok(mut file) = std::fs::File::create(&path) else {
        return Some(2);
    };

    let mut failures = 0;
    for task_id in task_ids {
        if !write_line(
            &mut file,
            &HelperLine {
                task_id,
                status: TaskStatus::Running,
                message: None,
                duration_ms: None,
            },
        ) {
            return Some(2);
        }

        let started = std::time::Instant::now();
        let result = run_task(task_id);
        let status = if result.is_ok() {
            TaskStatus::Done
        } else {
            TaskStatus::Error
        };
        let message = match result {
            Ok(message) => message,
            Err(error) => {
                failures += 1;
                error.to_string()
            }
        };

        if !write_line(
            &mut file,
            &HelperLine {
                task_id,
                status,
                message: Some(message),
                duration_ms: Some(started.elapsed().as_millis() as u64),
            },
        ) {
            return Some(2);
        }
    }

    Some(if failures == 0 { 0 } else { 1 })
}

/// Append one status line and make it visible to the parent right away.
fn write_line(file: &mut std::fs::File, line: &HelperLine) -> bool {
    use std::io::Write;

    let Ok(mut text) = serde_json::to_string(line) else {
        return false;
    };
    text.push('\n');

    file.write_all(text.as_bytes())
        .and_then(|()| file.flush())
        .is_ok()
}

/// Hand the elevated tasks to a second, elevated copy of this binary and stream
/// its progress into the event channel. Blocks until the helper exits.
fn run_helper(
    app: &AppHandle,
    tasks: &[(usize, OptimizeTaskId)],
    total: usize,
) -> Vec<(usize, OptimizeTaskId, HelperResult)> {
    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(error) => {
            return failed(tasks, format!("the elevated helper could not be started ({error})"))
        }
    };

    let result_file = std::env::temp_dir().join(format!(
        "windle-elevated-{}-{}.jsonl",
        std::process::id(),
        crate::commands::clean::now_millis()
    ));

    let ids: Vec<String> = tasks.iter().map(|(_, task_id)| slug(*task_id)).collect();
    let parameters = format!(
        "{} \"{}\" {}",
        ELEVATED_FLAG,
        result_file.display(),
        ids.join(",")
    );

    let process = match launch_elevated(&exe, &parameters) {
        Ok(process) => process,
        Err(error) if error.cancelled => {
            // The prompt was dismissed: nothing ran, and the wording matches
            // what macOS shows when its password dialog is cancelled.
            return failed(tasks, "administrator privileges were not granted");
        }
        Err(error) => {
            return failed(
                tasks,
                format!("the elevated helper could not be started ({})", error.message),
            )
        }
    };

    let index_of: BTreeMap<OptimizeTaskId, usize> = tasks
        .iter()
        .map(|(index, task_id)| (*task_id, *index))
        .collect();
    let mut seen = 0usize;
    let mut results: BTreeMap<OptimizeTaskId, HelperResult> = BTreeMap::new();

    loop {
        drain(app, &result_file, &mut seen, &index_of, total, &mut results);

        if process.finished() {
            break;
        }
        std::thread::sleep(POLL_INTERVAL);
    }

    // The last lines can land between the final poll and the exit.
    drain(app, &result_file, &mut seen, &index_of, total, &mut results);

    process.close();
    let _ = std::fs::remove_file(&result_file);

    tasks
        .iter()
        .map(|(index, task_id)| {
            let result = results.remove(task_id).unwrap_or_else(|| {
                HelperResult::failed("the elevated helper stopped before this task finished")
            });
            (*index, *task_id, result)
        })
        .collect()
}

/// Mark every task in `tasks` as failed with one message.
fn failed(
    tasks: &[(usize, OptimizeTaskId)],
    message: impl Into<String>,
) -> Vec<(usize, OptimizeTaskId, HelperResult)> {
    let message = message.into();
    tasks
        .iter()
        .map(|(index, task_id)| (*index, *task_id, HelperResult::failed(message.clone())))
        .collect()
}

/// Read any new lines the helper wrote and forward them as progress events.
fn drain(
    app: &AppHandle,
    file: &Path,
    seen: &mut usize,
    index_of: &BTreeMap<OptimizeTaskId, usize>,
    total: usize,
    results: &mut BTreeMap<OptimizeTaskId, HelperResult>,
) {
    let Ok(text) = std::fs::read_to_string(file) else {
        return;
    };

    // Only whole lines are consumed: the helper flushes after every line, but
    // the final write can still be in flight when this reads the file.
    let complete = if text.is_empty() {
        0
    } else {
        text.lines().count() - usize::from(!text.ends_with('\n'))
    };

    for line in text.lines().take(complete).skip(*seen) {
        let Ok(event) = serde_json::from_str::<HelperLine>(line) else {
            continue;
        };
        let Some(index) = index_of.get(&event.task_id).copied() else {
            continue;
        };

        match event.status {
            TaskStatus::Pending => {}
            TaskStatus::Running => emit(app, event.task_id, TaskStatus::Running, index, total, None),
            TaskStatus::Done | TaskStatus::Error => {
                let result = HelperResult {
                    succeeded: event.status == TaskStatus::Done,
                    message: event.message.unwrap_or_default(),
                    duration_ms: event.duration_ms.unwrap_or(0),
                };
                emit(
                    app,
                    event.task_id,
                    event.status,
                    index,
                    total,
                    Some(result.message.clone()),
                );
                results.insert(event.task_id, result);
            }
        }
    }

    *seen = complete;
}

/// Why the elevated helper could not be started.
struct LaunchError {
    /// The UAC prompt was dismissed.
    cancelled: bool,
    message: String,
}

/// A process started through the shell, waiting to be reaped.
struct ElevatedProcess(windows::Win32::Foundation::HANDLE);

impl ElevatedProcess {
    /// Whether the helper has exited.
    fn finished(&self) -> bool {
        use windows::Win32::Foundation::WAIT_OBJECT_0;
        use windows::Win32::System::Threading::WaitForSingleObject;

        // SAFETY: the handle is a live process handle owned by this value.
        unsafe { WaitForSingleObject(self.0, 0) == WAIT_OBJECT_0 }
    }

    /// Release the handle. The process itself has already exited.
    fn close(self) {
        use windows::Win32::Foundation::CloseHandle;

        // SAFETY: the handle is owned by this value and closed exactly once.
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

/// Start `exe` with administrator rights through the shell's `runas` verb,
/// which is what raises the UAC prompt.
fn launch_elevated(
    exe: &Path,
    parameters: &str,
) -> std::result::Result<ElevatedProcess, LaunchError> {
    use std::os::windows::ffi::OsStrExt;
    use windows::core::PCWSTR;
    use windows::Win32::UI::Shell::{
        ShellExecuteExW, SEE_MASK_FLAG_NO_UI, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW,
    };

    let file: Vec<u16> = exe
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let verb: Vec<u16> = "runas".encode_utf16().chain(std::iter::once(0)).collect();
    let parameters: Vec<u16> = parameters.encode_utf16().chain(std::iter::once(0)).collect();

    let mut info = SHELLEXECUTEINFOW {
        cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOCLOSEPROCESS | SEE_MASK_FLAG_NO_UI,
        lpVerb: PCWSTR(verb.as_ptr()),
        lpFile: PCWSTR(file.as_ptr()),
        lpParameters: PCWSTR(parameters.as_ptr()),
        nShow: SW_HIDE,
        ..Default::default()
    };

    // SAFETY: every pointer in `info` refers to a buffer that outlives the call,
    // and the process handle the shell returns is owned by the result.
    if let Err(error) = unsafe { ShellExecuteExW(&mut info) } {
        return Err(LaunchError {
            cancelled: error.code().0 as u32 == CANCELLED,
            message: error.message(),
        });
    }

    Ok(ElevatedProcess(info.hProcess))
}

// ---------------------------------------------------------------------------
// Startup items
//
// Two sources, both of which Explorer itself uses: the `Run` registry keys,
// whose on/off state lives in a `StartupApproved` blob, and the Startup
// folders, where a shortcut is taken out of the scan by renaming it.
// ---------------------------------------------------------------------------

/// Shortcuts disabled by Windle get this appended, which takes them out of the
/// folder's scan without moving them somewhere the user cannot find them.
const DISABLED_SUFFIX: &str = ".disabled";

/// One of the registry keys Windows starts programs from at login.
struct RunKey {
    /// Whether the commands live in the machine hive, which needs admin rights.
    machine: bool,
    /// Registry path holding the commands.
    path: &'static str,
    /// Registry path holding the enabled/disabled blobs Explorer writes.
    approved: &'static str,
}

impl RunKey {
    fn hive(&self) -> RegKey {
        RegKey::predef(if self.machine {
            HKEY_LOCAL_MACHINE
        } else {
            HKEY_CURRENT_USER
        })
    }

    fn hive_name(&self) -> &'static str {
        if self.machine {
            "HKLM"
        } else {
            "HKCU"
        }
    }

    /// The id a Run value is addressed by: `HKLM\…\Run\OneDrive`. Splitting on
    /// the last backslash recovers the location and the value name, and the
    /// location is checked against this table before anything is written.
    fn id(&self, name: &str) -> String {
        format!("{}\\{}\\{}", self.hive_name(), self.path, name)
    }
}

static RUN_KEYS: &[RunKey] = &[
    RunKey {
        machine: false,
        path: r"Software\Microsoft\Windows\CurrentVersion\Run",
        approved: r"Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run",
    },
    RunKey {
        machine: true,
        path: r"Software\Microsoft\Windows\CurrentVersion\Run",
        approved: r"Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run",
    },
    // The 32-bit view of the machine hive, where 32-bit installers still write.
    RunKey {
        machine: true,
        path: r"Software\WOW6432Node\Microsoft\Windows\CurrentVersion\Run",
        approved: r"Software\WOW6432Node\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run",
    },
];

/// A Startup folder, and whether it belongs to the machine.
struct StartupFolder {
    path: PathBuf,
    machine: bool,
}

fn startup_folders() -> Vec<StartupFolder> {
    let mut folders = Vec::new();

    if let Some(appdata) = platform::env_path("APPDATA") {
        folders.push(StartupFolder {
            path: appdata.join(r"Microsoft\Windows\Start Menu\Programs\Startup"),
            machine: false,
        });
    }
    if let Some(program_data) = platform::env_path("ProgramData") {
        folders.push(StartupFolder {
            path: program_data.join(r"Microsoft\Windows\Start Menu\Programs\Startup"),
            machine: true,
        });
    }

    folders
}

/// Every registry `Run` value and Startup folder shortcut, sorted by label.
pub fn startup_items() -> Vec<LoginItem> {
    let mut items = run_key_items();
    items.extend(startup_folder_items());
    items.sort_by(|a, b| a.label.to_lowercase().cmp(&b.label.to_lowercase()));
    items
}

fn run_key_items() -> Vec<LoginItem> {
    let mut items = Vec::new();

    for key in RUN_KEYS {
        let hive = key.hive();
        // A missing key is normal: the 32-bit view does not exist everywhere.
        let Ok(commands) = hive.open_subkey_with_flags(key.path, KEY_READ) else {
            continue;
        };
        let approved = hive.open_subkey_with_flags(key.approved, KEY_READ);

        for (name, _) in commands.enum_values().filter_map(std::result::Result::ok) {
            if name.is_empty() {
                continue;
            }

            let command: String = commands.get_value(&name).unwrap_or_default();
            let enabled = approved
                .as_ref()
                .map(|approved| startup_state(approved, &name))
                .unwrap_or(true);

            items.push(LoginItem {
                id: key.id(&name),
                label: name.clone(),
                path: command.clone(),
                kind: LoginItemKind::RunKey,
                enabled,
                is_system: looks_like_system(&command),
            });
        }
    }

    items
}

fn startup_folder_items() -> Vec<LoginItem> {
    let mut items = Vec::new();

    for folder in startup_folders() {
        let Ok(entries) = std::fs::read_dir(&folder.path) else {
            continue;
        };

        for entry in entries.filter_map(std::result::Result::ok) {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }

            let file_name = format::file_name(&path);
            let enabled = !file_name.ends_with(DISABLED_SUFFIX);
            let label = file_name.trim_end_matches(DISABLED_SUFFIX);
            let label = label.rsplit_once('.').map_or(label, |(stem, _)| stem);
            if label.is_empty() {
                continue;
            }

            items.push(LoginItem {
                id: path.to_string_lossy().into_owned(),
                label: label.to_string(),
                path: path.to_string_lossy().into_owned(),
                kind: LoginItemKind::StartupFolder,
                enabled,
                // Anyone can put a shortcut here, so nothing in a Startup
                // folder is treated as the system's own.
                is_system: false,
            });
        }
    }

    items
}

/// A command that lives under the Windows directories belongs to the system
/// and must never be toggled — the rule that keeps `com.apple.*` jobs alone on
/// macOS.
fn looks_like_system(command: &str) -> bool {
    let lower = command
        .to_lowercase()
        .replace("%windir%", r"\windows")
        .replace("%systemroot%", r"\windows");

    lower.contains(r"\windows\system32") || lower.contains(r"\windows\syswow64")
}

/// Whether Explorer considers a startup item enabled. The state is a 12-byte
/// blob whose first byte is `2` (or `6`) when on and `3` (or `7`) when off, so
/// the low bit is what decides; a missing blob means enabled.
fn startup_state(approved: &RegKey, name: &str) -> bool {
    match approved.get_raw_value(name) {
        Ok(value) => state_of(&value.bytes),
        Err(_) => true,
    }
}

fn state_of(blob: &[u8]) -> bool {
    blob.first().map(|byte| byte & 1 == 0).unwrap_or(true)
}

/// Turn a startup item on or off the way Task Manager does, by rewriting the
/// blob in the `StartupApproved` key. The `Run` value itself is never touched,
/// so the command stays where the user put it.
fn write_startup_state(approved: &RegKey, name: &str, enabled: bool) -> Result<()> {
    let mut bytes = vec![if enabled { 0x02u8 } else { 0x03u8 }];
    bytes.resize(12, 0);

    approved
        .set_raw_value(
            name,
            &RegValue {
                bytes,
                vtype: REG_BINARY,
            },
        )
        .map_err(|error| WindleError::Command {
            command: "registry".into(),
            message: format!("{name}: {error}"),
        })
}

/// Enable or disable a startup item.
pub fn set_login_item_enabled(item: &LoginItem, enabled: bool) -> Result<()> {
    match item.kind {
        LoginItemKind::RunKey => set_run_key_item(item, enabled),
        LoginItemKind::StartupFolder => set_startup_folder_item(item, enabled),
    }
}

/// Nothing to drop: the administrator password macOS caches is replaced here by
/// the UAC prompt, which Windows asks per elevation and never lets us hold on
/// to. Kept so the "sign out of the cache" command behaves the same everywhere.
pub fn clear_auth() {}

fn set_run_key_item(item: &LoginItem, enabled: bool) -> Result<()> {
    let (key, name) =
        split_run_id(&item.id).ok_or_else(|| WindleError::NotFound(item.id.clone()))?;

    if key.machine && !permissions::is_root() {
        return Err(WindleError::NeedsElevation(format!(
            "changing the {} startup item",
            item.label
        )));
    }

    let hive = key.hive();
    let commands = hive
        .open_subkey_with_flags(key.path, KEY_READ)
        .map_err(registry_error(&item.id))?;

    // The command has to still be there: an id that no longer matches one would
    // otherwise leave a stray blob behind.
    commands
        .get_value::<String, _>(&name)
        .map_err(registry_error(&item.id))?;

    let approved = hive
        .open_subkey_with_flags(key.approved, KEY_READ | KEY_WRITE)
        .or_else(|_| {
            hive.create_subkey_with_flags(key.approved, KEY_WRITE)
                .map(|(key, _)| key)
        })
        .map_err(registry_error(&item.id))?;

    write_startup_state(&approved, &name, enabled)
}

/// Rename a Startup folder shortcut in or out of the disabled state.
fn set_startup_folder_item(item: &LoginItem, enabled: bool) -> Result<()> {
    let path = PathBuf::from(&item.id);

    // Only files inside a Startup folder may be renamed, so a forged id cannot
    // reach anything else.
    let folder = startup_folders()
        .into_iter()
        .find(|folder| platform::starts_with(&path, &folder.path))
        .ok_or_else(|| WindleError::NotFound(item.id.clone()))?;

    if folder.machine && !permissions::is_root() {
        return Err(WindleError::NeedsElevation(format!(
            "changing the {} startup item",
            item.label
        )));
    }

    let name = format::file_name(&path);
    let disabled = name.ends_with(DISABLED_SUFFIX);
    if disabled != enabled {
        // Already in the requested state.
        return Ok(());
    }

    let target = if enabled {
        folder.path.join(name.trim_end_matches(DISABLED_SUFFIX))
    } else {
        folder.path.join(format!("{name}{DISABLED_SUFFIX}"))
    };

    std::fs::rename(&path, &target).map_err(|error| permissions::classify_io_error(&path, &error))
}

/// The Run key a value id came from, plus the name inside it.
fn split_run_id(id: &str) -> Option<(&'static RunKey, String)> {
    let (location, name) = id.rsplit_once('\\')?;

    RUN_KEYS
        .iter()
        .find(|key| {
            location.eq_ignore_ascii_case(&format!("{}\\{}", key.hive_name(), key.path))
        })
        .map(|key| (key, name.to_string()))
}

fn registry_error(id: &str) -> impl FnOnce(std::io::Error) -> WindleError + '_ {
    move |error| WindleError::Command {
        command: "registry".into(),
        message: format!("{id}: {error}"),
    }
}

pub fn catalogue() -> Vec<OptimizeTask> {
    vec![
        OptimizeTask {
            id: OptimizeTaskId::PurgeMemory,
            label: "Purge inactive memory".into(),
            description: "Ask Windows to trim cached pages from every process back to the free pool."
                .into(),
            risk: RiskLevel::Safe,
            requires_elevation: false,
            estimated_seconds: 10,
        },
        OptimizeTask {
            id: OptimizeTaskId::FlushDns,
            label: "Flush DNS cache".into(),
            description: "Clear resolved hostnames after a network or VPN change.".into(),
            risk: RiskLevel::Safe,
            requires_elevation: false,
            estimated_seconds: 2,
        },
        OptimizeTask {
            id: OptimizeTaskId::ClearTempFiles,
            label: "Clear temporary files".into(),
            description: "Empties the user, Windows and ProgramData temp folders.".into(),
            risk: RiskLevel::Safe,
            requires_elevation: true,
            estimated_seconds: 20,
        },
        OptimizeTask {
            id: OptimizeTaskId::ClearUpdateCache,
            label: "Clear the Windows Update cache".into(),
            description: "Removes downloaded update files so a stalled update downloads again."
                .into(),
            risk: RiskLevel::Safe,
            requires_elevation: true,
            estimated_seconds: 30,
        },
        OptimizeTask {
            id: OptimizeTaskId::RebuildSearchIndex,
            label: "Rebuild the search index".into(),
            description: "Deletes the index database so Windows builds it again. Searching stays slow until it finishes."
                .into(),
            risk: RiskLevel::Caution,
            requires_elevation: true,
            estimated_seconds: 30,
        },
        OptimizeTask {
            id: OptimizeTaskId::ResetIconCache,
            label: "Refresh the icon cache".into(),
            description: "Fixes blank or wrong icons in File Explorer.".into(),
            risk: RiskLevel::Safe,
            requires_elevation: false,
            estimated_seconds: 5,
        },
        OptimizeTask {
            id: OptimizeTaskId::VerifySystemFiles,
            label: "Verify system files".into(),
            description: "Runs System File Checker, which repairs corrupted Windows files in place."
                .into(),
            risk: RiskLevel::Caution,
            requires_elevation: true,
            estimated_seconds: 600,
        },
        OptimizeTask {
            id: OptimizeTaskId::RepairSystemImage,
            label: "Repair the system image".into(),
            description: "Runs DISM to replace damaged files in the component store.".into(),
            risk: RiskLevel::Caution,
            requires_elevation: true,
            estimated_seconds: 900,
        },
        OptimizeTask {
            id: OptimizeTaskId::OptimizeSystemDrive,
            label: "Optimize the system drive".into(),
            description: "Runs TRIM on SSDs and defragmentation on hard drives.".into(),
            risk: RiskLevel::Safe,
            requires_elevation: true,
            estimated_seconds: 120,
        },
        OptimizeTask {
            id: OptimizeTaskId::CheckSystemDrive,
            label: "Check the system drive".into(),
            description: "Scans the file system for errors without taking the drive offline.".into(),
            risk: RiskLevel::Caution,
            requires_elevation: true,
            estimated_seconds: 180,
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_task_matches_its_elevation_needs() {
        for task in catalogue() {
            assert!(!task.label.is_empty());
            assert!(!task.description.is_empty());
            assert_eq!(
                task.requires_elevation,
                needs_elevation(task.id),
                "{} is inconsistent",
                task.label
            );
        }
    }

    #[test]
    fn every_task_has_work_behind_it() {
        for task in catalogue() {
            // PurgeMemory is a local API loop rather than a plan.
            if task.id == OptimizeTaskId::PurgeMemory {
                continue;
            }
            assert!(
                !plan(task.id).actions.is_empty(),
                "{} has no actions",
                task.label
            );
        }
    }

    #[test]
    fn purge_memory_needs_no_administrator() {
        assert!(!needs_elevation(OptimizeTaskId::PurgeMemory));
        assert!(!needs_elevation(OptimizeTaskId::FlushDns));
        assert!(!needs_elevation(OptimizeTaskId::ResetIconCache));
        assert!(needs_elevation(OptimizeTaskId::VerifySystemFiles));
        assert!(needs_elevation(OptimizeTaskId::CheckSystemDrive));
    }

    /// Anything the plans delete has to sit under a scratch root, so a mistake
    /// in one of the paths cannot point the guard rails at user data.
    #[test]
    fn the_plans_only_touch_machine_wide_scratch_areas() {
        let allowed = [
            permissions::expand("%TEMP%"),
            permissions::expand("%SystemRoot%"),
            permissions::expand("%ProgramData%"),
        ];

        for task in catalogue() {
            for action in plan(task.id).actions {
                let dir = match action {
                    Action::Empty { dir } | Action::Remove { dir } => dir,
                    Action::Run { .. } => continue,
                };

                assert!(
                    allowed
                        .iter()
                        .any(|root| platform::starts_with(&dir, root)),
                    "{} is outside the scratch areas",
                    dir.display()
                );
            }
        }
    }

    #[test]
    fn a_command_reports_its_last_line() {
        let plan = Plan {
            actions: vec![cmd("cmd", &["/c", "echo all good"])],
            done: "Done",
        };

        assert_eq!(run_plan(&plan).unwrap(), "all good");
    }

    #[test]
    fn optional_steps_are_reported_but_do_not_fail_the_task() {
        let plan = Plan {
            actions: vec![soft_cmd("cmd", &["/c", "exit 1"])],
            done: "Did the thing",
        };

        let message = run_plan(&plan).expect("an optional failure must not fail the task");
        assert!(message.contains("Did the thing"), "got {message}");
        assert!(message.contains("skipped"), "got {message}");
    }

    #[test]
    fn a_required_step_failing_fails_the_task() {
        let plan = Plan {
            actions: vec![cmd("cmd", &["/c", "exit 1"])],
            done: "Did the thing",
        };

        assert!(run_plan(&plan).is_err());
    }

    #[test]
    fn freed_bytes_lead_the_message() {
        let message = compose(2_048, None, "Temporary files cleared", &[]);
        assert_eq!(message, format!("Freed {}", format::bytes(2_048)));

        let message = compose(2_048, Some("all good".into()), "Done", &[]);
        assert_eq!(message, format!("Freed {} · all good", format::bytes(2_048)));
    }

    #[test]
    fn notes_are_appended_to_the_message() {
        let message = compose(0, None, "Done", &["cmd skipped (boom)".to_string()]);
        assert_eq!(message, "Done — cmd skipped (boom)");
    }

    #[test]
    fn the_helper_protocol_round_trips() {
        let root = crate::utils::test_support::scratch("helper-lines");
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("result.jsonl");

        let mut file = std::fs::File::create(&path).unwrap();
        assert!(write_line(
            &mut file,
            &HelperLine {
                task_id: OptimizeTaskId::FlushDns,
                status: TaskStatus::Running,
                message: None,
                duration_ms: None,
            }
        ));
        assert!(write_line(
            &mut file,
            &HelperLine {
                task_id: OptimizeTaskId::FlushDns,
                status: TaskStatus::Done,
                message: Some("DNS cache flushed".into()),
                duration_ms: Some(12),
            }
        ));

        let text = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<HelperLine> = text
            .lines()
            .map(|line| serde_json::from_str(line).expect("every line we write parses back"))
            .collect();

        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].status, TaskStatus::Running);
        assert_eq!(lines[1].status, TaskStatus::Done);
        assert_eq!(lines[1].task_id, OptimizeTaskId::FlushDns);
        assert_eq!(lines[1].message.as_deref(), Some("DNS cache flushed"));
        assert_eq!(lines[1].duration_ms, Some(12));

        std::fs::remove_dir_all(&root).ok();
    }

    /// The parent writes each id with [`super::slug`] and the helper reads it
    /// back with [`parse_slug`]; a name that does not survive the trip would
    /// silently drop a task from the batch.
    #[test]
    fn task_ids_survive_the_helper_command_line() {
        for task in catalogue() {
            let encoded = slug(task.id);
            assert_eq!(
                parse_slug(&encoded),
                Some(task.id),
                "{encoded} did not round-trip"
            );
        }

        assert_eq!(parse_slug(""), None);
        assert_eq!(parse_slug("no-such-task"), None);
    }

    #[test]
    fn the_startup_approved_blob_decides_enabled_state() {
        assert!(state_of(&[0x02]));
        assert!(state_of(&[0x06]));
        assert!(!state_of(&[0x03]));
        assert!(!state_of(&[0x07]));
        assert!(state_of(&[]), "a missing blob means enabled");
    }

    #[test]
    fn run_ids_round_trip_through_the_key_table() {
        let id = RUN_KEYS[0].id("OneDrive");
        assert_eq!(
            id,
            r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run\OneDrive"
        );

        let (key, name) = split_run_id(&id).expect("the id addresses a known key");
        assert_eq!(name, "OneDrive");
        assert_eq!(key.hive_name(), "HKCU");
        assert!(!key.machine);

        // A location that is not in the table is refused outright.
        assert!(split_run_id(r"HKCU\Software\Evil\Run\payload").is_none());
    }

    #[test]
    fn commands_under_the_windows_directories_count_as_system() {
        assert!(looks_like_system(
            r"C:\Windows\System32\SecurityHealthSystray.exe"
        ));
        assert!(looks_like_system(r"%windir%\system32\foo.exe"));
        assert!(!looks_like_system(r"C:\Program Files\Vendor\app.exe"));
    }

    #[test]
    fn startup_items_are_well_formed_on_this_machine() {
        for item in startup_items() {
            assert!(!item.label.is_empty(), "every item needs a label");
            assert!(!item.id.is_empty(), "every item needs an id");
        }
    }
}
