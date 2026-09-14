//! Deep Clean — caches, logs and other junk that is safe to remove.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, State};

use super::{CleanOutcome, RiskLevel};
use crate::scanner::walker::{self, CancelToken};
use crate::scanner::ScanProgress;
use crate::utils::fs_ops::{self, RemoveMode};
use crate::utils::{format, history, permissions, WindleError, Result};

pub const PROGRESS_EVENT: &str = "clean://progress";

/// Files in `~/Downloads` are only offered once they are this old.
const DOWNLOAD_AGE_DAYS: u64 = 30;

/// Temporary files are only offered once nothing has touched them for a while,
/// so we never pull the rug out from under a running app.
const TEMP_AGE_DAYS: u64 = 7;

/// Ignore cache entries below this size; hundreds of tiny directories only make
/// the list harder to read.
const MIN_ITEM_SIZE: u64 = 4_096;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CleanCategoryId {
    UserCache,
    SystemCache,
    AppLogs,
    AppJunk,
    BrowserCache,
    Trash,
    Downloads,
    MailAttachments,
    XcodeDerivedData,
    IosBackups,
    LanguageFiles,
    BrokenSymlinks,
}

/// A sub-grouping inside a category, e.g. "Crash Reports" inside Logs, so the
/// UI can cluster related items. The ids are part of the frontend contract —
/// do not rename them without coordinating with the frontend.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ItemGroup {
    pub id: String,
    pub label: String,
}

/// Sub-group shown inside a category: `(id, label)`.
type GroupSpec = (&'static str, &'static str);

/// The sub-groups the deep-clean UI renders inside a category.
const GROUP_USER_LOGS: GroupSpec = ("user-logs", "User Logs");
const GROUP_CRASH_REPORTS: GroupSpec = ("crash-reports", "Crash Reports");
const GROUP_SYSTEM_LOGS: GroupSpec = ("system-logs", "System Logs");
const GROUP_SANDBOX_LOGS: GroupSpec = ("sandbox-logs", "Sandboxed App Logs");
const GROUP_APP_CACHES: GroupSpec = ("app-caches", "App Caches");
const GROUP_HTTP_STORAGES: GroupSpec = ("http-storages", "Website Data");
const GROUP_SAVED_STATE: GroupSpec = ("saved-state", "Saved Application State");
const GROUP_WEBKIT: GroupSpec = ("webkit", "WebKit Data");
const GROUP_SANDBOX_CACHES: GroupSpec = ("sandbox-caches", "Sandboxed App Caches");
const GROUP_SYSTEM_CACHES: GroupSpec = ("system-caches", "System Caches");
const GROUP_TEMP_FILES: GroupSpec = ("temp-files", "Temporary Files");

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CleanableItem {
    pub id: String,
    pub path: String,
    pub size: u64,
    pub modified_at: Option<u64>,
    pub category: CleanCategoryId,
    pub risk: RiskLevel,
    pub description: String,
    pub group: Option<ItemGroup>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CleanCategory {
    pub id: CleanCategoryId,
    pub label: String,
    pub description: String,
    pub risk: RiskLevel,
    pub total_size: u64,
    pub item_count: usize,
    pub items: Vec<CleanableItem>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CleanScanResult {
    pub categories: Vec<CleanCategory>,
    pub total_size: u64,
    pub scanned_at: u64,
    pub duration_ms: u64,
}

/// Cancellation flag shared by the scan and the cancel command.
#[derive(Debug, Default)]
pub struct CleanState {
    pub cancel: CancelToken,
    /// Prevents two concurrent scans from clobbering each other's cancel token.
    scanning: AtomicBool,
}

/// RAII guard that clears the `scanning` flag when dropped, so a cancelled
/// future or a panic cannot leave it stuck.
struct ScanningGuard<'a>(&'a AtomicBool);

impl Drop for ScanningGuard<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

/// Walk the requested categories and report everything we could remove.
#[tauri::command]
pub async fn scan_junk(
    app: AppHandle,
    state: State<'_, CleanState>,
    categories: Option<Vec<CleanCategoryId>>,
) -> Result<CleanScanResult> {
    let started = std::time::Instant::now();

    // Atomically claim the scanning slot. If another invocation is already
    // running, refuse so it cannot reset the first scan's cancel token.
    if state
        .scanning
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return Err(WindleError::Command {
            command: "scan_junk".into(),
            message: "a scan is already in progress".into(),
        });
    }

    // Clear the flag when we leave, whether by returning or being cancelled.
    let _guard = ScanningGuard(&state.scanning);

    state.cancel.reset();

    let requested = categories.unwrap_or_else(all_categories);
    let total_steps = requested.len().max(1) as f32;

    let mut results = Vec::with_capacity(requested.len());
    let mut total_size = 0;

    for (step, id) in requested.into_iter().enumerate() {
        if state.cancel.is_cancelled() {
            break;
        }

        let mut items = scan_category(id, &state.cancel);
        // Biggest items first so a category view leads with what matters.
        items.sort_by(|a, b| b.size.cmp(&a.size));
        let size: u64 = items.iter().map(|item| item.size).sum();
        total_size += size;

        let _ = app.emit(
            PROGRESS_EVENT,
            ScanProgress {
                progress: Some((step + 1) as f32 / total_steps),
                current_path: id_of(id).to_string(),
                items_scanned: items.len() as u64,
                bytes_found: total_size,
            },
        );

        results.push(CleanCategory {
            id,
            label: label_of(id).to_string(),
            description: description_of(id).to_string(),
            risk: risk_of(id),
            total_size: size,
            item_count: items.len(),
            items,
        });
    }

    // Biggest win first.
    results.sort_by(|a, b| b.total_size.cmp(&a.total_size));

    Ok(CleanScanResult {
        categories: results,
        total_size,
        scanned_at: now_millis(),
        duration_ms: started.elapsed().as_millis() as u64,
    })
}

/// Remove the given paths, trashing them unless `permanent` is set.
#[tauri::command]
pub async fn clean_paths(paths: Vec<String>, permanent: bool) -> Result<CleanOutcome> {
    let mode = RemoveMode::from_permanent(permanent);
    let mut outcome = CleanOutcome::default();

    for path in paths {
        let target = PathBuf::from(&path);

        match fs_ops::remove(&target, mode) {
            Ok(freed) => outcome.succeed(path, freed),
            Err(error) => outcome.fail(path, error.to_string()),
        }
    }

    history::record_outcome(history::Operation::Clean, &outcome);

    // The dashboard's junk-size cache (60 s TTL) still holds the pre-clean
    // total, so invalidate it now — otherwise the next `getDashboardSummary`
    // call returns stale data.
    crate::commands::invalidate_junk_cache();

    Ok(outcome)
}

/// Empty the user's trash.
#[tauri::command]
pub async fn empty_trash() -> Result<CleanOutcome> {
    let trash = permissions::home_dir().join(".Trash");
    let mut outcome = CleanOutcome::default();

    if !trash.is_dir() {
        return Ok(outcome);
    }

    // Items in the trash were already discarded once, so they go for good.
    let (freed, failures) = fs_ops::empty_directory(&trash, RemoveMode::Permanent);

    outcome.freed_bytes = freed;
    outcome.removed_paths.push(trash.to_string_lossy().into_owned());

    for (path, error) in failures {
        outcome.fail(path.to_string_lossy().into_owned(), error.to_string());
    }

    history::record_outcome(history::Operation::EmptyTrash, &outcome);

    Ok(outcome)
}

/// Stop an in-flight scan.
#[tauri::command]
pub async fn cancel_clean_scan(state: State<'_, CleanState>) -> Result<()> {
    state.cancel.cancel();
    Ok(())
}

fn all_categories() -> Vec<CleanCategoryId> {
    use CleanCategoryId::*;
    vec![
        UserCache,
        SystemCache,
        AppLogs,
        AppJunk,
        BrowserCache,
        Trash,
        Downloads,
        MailAttachments,
        XcodeDerivedData,
        IosBackups,
        LanguageFiles,
        BrokenSymlinks,
    ]
}

/// The kebab-case id of a category, matching its serialised form. Progress
/// events carry it instead of the English label so the frontend can map the
/// current step to a localised name.
fn id_of(id: CleanCategoryId) -> &'static str {
    use CleanCategoryId::*;
    match id {
        UserCache => "user-cache",
        SystemCache => "system-cache",
        AppLogs => "app-logs",
        AppJunk => "app-junk",
        BrowserCache => "browser-cache",
        Trash => "trash",
        Downloads => "downloads",
        MailAttachments => "mail-attachments",
        XcodeDerivedData => "xcode-derived-data",
        IosBackups => "ios-backups",
        LanguageFiles => "language-files",
        BrokenSymlinks => "broken-symlinks",
    }
}

fn label_of(id: CleanCategoryId) -> &'static str {
    use CleanCategoryId::*;
    match id {
        UserCache => "User caches",
        SystemCache => "System caches",
        AppLogs => "Logs",
        AppJunk => "App Junk",
        BrowserCache => "Browser caches",
        Trash => "Trash",
        Downloads => "Old downloads",
        MailAttachments => "Mail attachments",
        XcodeDerivedData => "Xcode leftovers",
        IosBackups => "iOS backups",
        LanguageFiles => "Unused languages",
        BrokenSymlinks => "Broken symlinks",
    }
}

fn description_of(id: CleanCategoryId) -> &'static str {
    use CleanCategoryId::*;
    match id {
        UserCache => "Data apps can rebuild on demand.",
        SystemCache => "Shared caches and stale temporary files.",
        AppLogs => "Diagnostic logs kept by apps and the system.",
        AppJunk => "Caches, website data, and leftovers kept by applications.",
        BrowserCache => "Cached pages and images. Does not touch history or logins.",
        Trash => "Items you already deleted.",
        Downloads => "Files in Downloads nobody has opened in a month.",
        MailAttachments => "Attachments Mail can download again from the server.",
        XcodeDerivedData => "Build products, archives and simulator support files.",
        IosBackups => "Local device backups. Irreplaceable unless you have a copy.",
        LanguageFiles => "Translations for languages you do not use.",
        BrokenSymlinks => "Links whose target no longer exists.",
    }
}

fn risk_of(id: CleanCategoryId) -> RiskLevel {
    use CleanCategoryId::*;
    match id {
        UserCache | AppLogs | BrowserCache | Trash | XcodeDerivedData | BrokenSymlinks => {
            RiskLevel::Safe
        }
        SystemCache | AppJunk | Downloads | MailAttachments | LanguageFiles => RiskLevel::Caution,
        IosBackups => RiskLevel::Danger,
    }
}

/// Locations under `~/Library/Caches` that belong to a browser, so they are
/// reported under `BrowserCache` instead of being counted twice under
/// `UserCache`. Reused by `is_browser_entry` to keep browser data out of the
/// app-junk WebKit / website-data groups.
const BROWSER_CACHE_NAMES: &[&str] = &[
    "com.apple.Safari",
    "Google/Chrome",
    "com.google.Chrome",
    "Firefox",
    "com.microsoft.edgemac",
    "com.brave.Browser",
    "BraveSoftware",
    "com.operasoftware.Opera",
    "Arc",
    "company.thebrowser.Browser",
];

fn browser_cache_roots() -> Vec<PathBuf> {
    let home = permissions::home_dir();
    let caches = home.join("Library/Caches");

    BROWSER_CACHE_NAMES
        .iter()
        .map(|name| caches.join(name))
        .chain([
            home.join("Library/Safari/Touch Icons Cache"),
            home.join("Library/Containers/com.apple.Safari/Data/Library/Caches"),
        ])
        .filter(|path| path.exists())
        .collect()
}

/// Whether a first-level entry name under `~/Library/WebKit` or
/// `~/Library/HTTPStorages` belongs to a browser. Browser data is the
/// browser-cache category's business — and that category promises not to
/// touch logins — so the app-junk groups leave these entries alone.
fn is_browser_entry(name: &str) -> bool {
    BROWSER_CACHE_NAMES.iter().any(|id| {
        // Exact bundle match or a sibling like `com.apple.Safari.binarycookies`.
        name == *id || name.starts_with(&format!("{id}."))
    })
}

/// Whether a first-level entry of `~/Library/WebKit` is offered under the
/// WebKit group: browser bundles are BrowserCache's business, and the
/// framework-level `Databases`/`WebPush` directories are shared by every
/// WebKit app (`WebPush` holds their push subscriptions), so they stay put.
fn accept_webkit_entry(name: &str) -> bool {
    !is_browser_entry(name) && name != "Databases" && name != "WebPush"
}

/// Whether a first-level entry of `~/Library/HTTPStorages` is offered under
/// the website-data group: browser bundles are BrowserCache's business, and
/// loose `.binarycookies` files hold embedded-webview logins (deleting one
/// logs the user out of that app's built-in web page).
fn accept_http_storage_entry(name: &str) -> bool {
    !is_browser_entry(name) && !name.ends_with(".binarycookies")
}

/// Direct children of `root` as paths — empty when the root is missing or
/// unreadable.
fn list_children(root: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(root)
        .map(|entries| {
            entries
                .filter_map(std::result::Result::ok)
                .map(|entry| entry.path())
                .collect()
        })
        .unwrap_or_default()
}

/// Cache directories under `<root>/<app>/` — only sub-directories explicitly
/// named `Cache` or `Caches`. Everything else in an app's support folder is
/// real data, so nothing else is ever offered.
fn app_support_cache_dirs(root: &Path) -> Vec<PathBuf> {
    let mut dirs = Vec::new();

    for app_dir in list_children(root) {
        for child in list_children(&app_dir) {
            if child.is_dir() && is_cache_dir_name(&format::file_name(&child)) {
                dirs.push(child);
            }
        }
    }

    dirs
}

/// Directory names that unambiguously mark a cache.
fn is_cache_dir_name(name: &str) -> bool {
    name == "Cache" || name == "Caches"
}

/// `<container>/Data/Library/<sub>` directories that exist for every sandbox
/// container under `root` (typically `~/Library/Containers`).
fn container_data_dirs(root: &Path, sub: &str) -> Vec<PathBuf> {
    list_children(root)
        .into_iter()
        .map(|container| container.join("Data/Library").join(sub))
        .filter(|dir| dir.is_dir())
        .collect()
}

/// Log directories inside app-group containers. Both the `Logs` and the
/// `Data/Library/Logs` layouts occur in the wild.
fn group_container_log_dirs(root: &Path) -> Vec<PathBuf> {
    let mut dirs = Vec::new();

    for group in list_children(root) {
        for candidate in [group.join("Logs"), group.join("Data/Library/Logs")] {
            if candidate.is_dir() {
                dirs.push(candidate);
            }
        }
    }

    dirs
}

/// Every sandboxed log location: per-app containers plus app-group containers.
fn sandbox_log_roots(home: &Path) -> Vec<PathBuf> {
    let mut roots = container_data_dirs(&home.join("Library/Containers"), "Logs");
    roots.extend(group_container_log_dirs(&home.join("Library/Group Containers")));
    roots
}

/// Enumerate the items for one category.
fn scan_category(id: CleanCategoryId, cancel: &CancelToken) -> Vec<CleanableItem> {
    use CleanCategoryId::*;

    let home = permissions::home_dir();
    let risk = risk_of(id);

    match id {
        UserCache => {
            let excluded = browser_cache_roots();
            children_of(
                &home.join("Library/Caches"),
                id,
                risk,
                None,
                cancel,
                &|path| !excluded.iter().any(|browser| path.starts_with(browser)),
            )
        }
        SystemCache => {
            let mut items = children_of(
                Path::new("/Library/Caches"),
                id,
                risk,
                Some(GROUP_SYSTEM_CACHES),
                cancel,
                &|_| true,
            );

            // `/tmp` is shared with running processes, so only stale entries.
            for temp in [Path::new("/private/tmp"), Path::new("/private/var/tmp")] {
                items.extend(children_of(temp, id, risk, Some(GROUP_TEMP_FILES), cancel, &|path| {
                    older_than(path, TEMP_AGE_DAYS)
                }));
            }

            items
        }
        AppLogs => {
            let mut items = Vec::new();

            // User logs. Crash-report directories are carved out into their
            // own sub-group rather than lumped in with the rest.
            let user_logs = home.join("Library/Logs");
            items.extend(children_of(
                &user_logs,
                id,
                risk,
                Some(GROUP_USER_LOGS),
                cancel,
                &|path| format::file_name(path) != "DiagnosticReports",
            ));

            // Crash reports, from the three usual places.
            for root in [
                user_logs.join("DiagnosticReports"),
                home.join("Library/Application Support/CrashReporter"),
                PathBuf::from("/Library/Application Support/CrashReporter"),
            ] {
                items.extend(children_of(
                    &root,
                    id,
                    risk,
                    Some(GROUP_CRASH_REPORTS),
                    cancel,
                    &|_| true,
                ));
            }

            // System-level crash reports, carved out of `/Library/Logs` below
            // just like `~/Library/Logs/DiagnosticReports` is above, and with
            // the same Caution risk and stale-only age filter as their parent.
            items.extend(children_of(
                Path::new("/Library/Logs/DiagnosticReports"),
                id,
                RiskLevel::Caution,
                Some(GROUP_CRASH_REPORTS),
                cancel,
                &|path| older_than(path, TEMP_AGE_DAYS),
            ));

            // System logs, shared with running daemons — stale entries only.
            // `/private/var/log` needs Full Disk Access; silently empty without.
            items.extend(children_of(
                Path::new("/private/var/log"),
                id,
                RiskLevel::Caution,
                Some(GROUP_SYSTEM_LOGS),
                cancel,
                &|path| older_than(path, TEMP_AGE_DAYS),
            ));
            // `DiagnosticReports` under `/Library/Logs` is carved out into the
            // crash-reports group above instead of being reported as a plain
            // system log, mirroring the user-logs scan.
            items.extend(children_of(
                Path::new("/Library/Logs"),
                id,
                RiskLevel::Caution,
                Some(GROUP_SYSTEM_LOGS),
                cancel,
                &|path| {
                    format::file_name(path) != "DiagnosticReports"
                        && older_than(path, TEMP_AGE_DAYS)
                },
            ));

            // Unified-log archives (`tracev3`). Readable by the admin group,
            // so the scan works without elevation; deleting needs root and
            // will surface as NeedsElevation failures without it — the same
            // trade-off as `/private/var/log`.
            //
            // The archives live one level down (Persist/Special/Signpost/
            // HighVolume/timesync), and every archived file is offered — no
            // age filter. By product decision the whole archive set shows
            // immediately, recent rotations included: logd re-creates its
            // files automatically, so removing them carries no system risk.
            // That is a deliberate difference from `/private/var/log`, which
            // keeps its 7-day stale-only filter because it holds live logs
            // shared with running daemons. (An earlier version filtered these
            // archives by file mtime; the scan is still file-level because a
            // directory-level mtime never matches — logd refreshes the
            // archive directories' mtimes continuously.) Loose files directly
            // under `diagnostics` (logd.0.log, roles.plist, …) are logd's
            // live working set and are skipped, which is why only
            // sub-directories are visited. Missing archive directories
            // (HighVolume is usually empty) simply yield nothing.
            for archive_dir in list_children(Path::new("/private/var/db/diagnostics"))
                .into_iter()
                .filter(|path| path.is_dir())
            {
                items.extend(children_of(
                    &archive_dir,
                    id,
                    RiskLevel::Caution,
                    Some(GROUP_SYSTEM_LOGS),
                    cancel,
                    &|_| true,
                ));
            }

            // Logs kept inside sandboxed app and app-group containers.
            for root in sandbox_log_roots(&home) {
                items.extend(children_of(
                    &root,
                    id,
                    risk,
                    Some(GROUP_SANDBOX_LOGS),
                    cancel,
                    &|_| true,
                ));
            }

            items
        }
        AppJunk => {
            let mut items = Vec::new();

            // Caches apps keep inside their support folders. Only directories
            // explicitly named `Cache`/`Caches` match — the rest of an app's
            // support folder is real data and stays untouched.
            let support = home.join("Library/Application Support");
            for cache_dir in app_support_cache_dirs(&support) {
                items.extend(children_of(
                    &cache_dir,
                    id,
                    RiskLevel::Safe,
                    Some(GROUP_APP_CACHES),
                    cancel,
                    &|_| true,
                ));
            }

            // Website data, window state and WebKit's own storage. Browser
            // bundles, framework-level WebKit directories and
            // `.binarycookies` login files are filtered out — see the
            // `accept_*_entry` predicates.
            items.extend(children_of(
                &home.join("Library/HTTPStorages"),
                id,
                RiskLevel::Caution,
                Some(GROUP_HTTP_STORAGES),
                cancel,
                &|path| accept_http_storage_entry(&format::file_name(path)),
            ));
            items.extend(children_of(
                &home.join("Library/Saved Application State"),
                id,
                RiskLevel::Caution,
                Some(GROUP_SAVED_STATE),
                cancel,
                &|_| true,
            ));
            items.extend(children_of(
                &home.join("Library/WebKit"),
                id,
                RiskLevel::Caution,
                Some(GROUP_WEBKIT),
                cancel,
                &|path| accept_webkit_entry(&format::file_name(path)),
            ));

            // Caches inside sandboxed app containers. Anything a browser keeps
            // there is reported under the browser-cache category instead,
            // mirroring how `UserCache` excludes browser caches.
            let browsers = browser_cache_roots();
            for cache_dir in container_data_dirs(&home.join("Library/Containers"), "Caches") {
                if browsers.iter().any(|browser| cache_dir.starts_with(browser)) {
                    continue;
                }
                items.extend(children_of(
                    &cache_dir,
                    id,
                    RiskLevel::Safe,
                    Some(GROUP_SANDBOX_CACHES),
                    cancel,
                    &|_| true,
                ));
            }

            items
        }
        BrowserCache => browser_cache_roots()
            .iter()
            .flat_map(|root| children_of(root, id, risk, None, cancel, &|_| true))
            .collect(),
        Trash => children_of(&home.join(".Trash"), id, risk, None, cancel, &|_| true),
        Downloads => children_of(&home.join("Downloads"), id, risk, None, cancel, &|path| {
            older_than(path, DOWNLOAD_AGE_DAYS)
        }),
        MailAttachments => walker::find_dirs_named(
            &home.join("Library/Mail"),
            &["Attachments"],
            6,
        )
        .into_iter()
        .filter_map(|path| describe(&path, id, risk))
        .collect(),
        XcodeDerivedData => {
            let developer = home.join("Library/Developer");
            [
                developer.join("Xcode/DerivedData"),
                developer.join("Xcode/Archives"),
                developer.join("Xcode/iOS DeviceSupport"),
                developer.join("Xcode/watchOS DeviceSupport"),
                developer.join("CoreSimulator/Caches"),
            ]
            .iter()
            .flat_map(|root| children_of(root, id, risk, None, cancel, &|_| true))
            .collect()
        }
        IosBackups => children_of(
            &home.join("Library/Application Support/MobileSync/Backup"),
            id,
            risk,
            None,
            cancel,
            &|_| true,
        ),
        LanguageFiles => unused_language_files(cancel),
        BrokenSymlinks => broken_symlinks(cancel),
    }
}

/// One item per direct child of `root`, sized in parallel and filtered by
/// `accept`. Missing or unreadable roots simply yield nothing. Items are
/// tagged with `group` when the category is split into sub-groups.
fn children_of(
    root: &Path,
    category: CleanCategoryId,
    risk: RiskLevel,
    group: Option<GroupSpec>,
    cancel: &CancelToken,
    accept: &dyn Fn(&Path) -> bool,
) -> Vec<CleanableItem> {
    if cancel.is_cancelled() || !root.is_dir() {
        return Vec::new();
    }

    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };

    let candidates: Vec<PathBuf> = entries
        .filter_map(std::result::Result::ok)
        .map(|entry| entry.path())
        // Never offer something the guard rails would refuse anyway.
        .filter(|path| permissions::ensure_removable(path).is_ok())
        .filter(|path| accept(path))
        .collect();

    if cancel.is_cancelled() {
        return Vec::new();
    }

    let group = group.map(|(id, label)| ItemGroup {
        id: id.to_string(),
        label: label.to_string(),
    });
    let sizes = walker::sizes_of(&candidates);

    candidates
        .into_iter()
        .zip(sizes)
        .filter(|(_, size)| *size >= MIN_ITEM_SIZE)
        .map(|(path, size)| item(&path, size, category, risk, group.as_ref()))
        .collect()
}

/// Build an item, measuring the path itself.
fn describe(path: &Path, category: CleanCategoryId, risk: RiskLevel) -> Option<CleanableItem> {
    let size = walker::size_of_any(path);
    (size >= MIN_ITEM_SIZE).then(|| item(path, size, category, risk, None))
}

fn item(
    path: &Path,
    size: u64,
    category: CleanCategoryId,
    risk: RiskLevel,
    group: Option<&ItemGroup>,
) -> CleanableItem {
    CleanableItem {
        id: path.to_string_lossy().into_owned(),
        path: path.to_string_lossy().into_owned(),
        size,
        modified_at: std::fs::symlink_metadata(path)
            .ok()
            .and_then(|meta| meta.modified().ok())
            .and_then(format::epoch_millis),
        category,
        risk,
        description: format::shorten_path(path, 64),
        group: group.cloned(),
    }
}

/// Localisation bundles inside installed apps, minus the languages the user
/// actually reads.
fn unused_language_files(cancel: &CancelToken) -> Vec<CleanableItem> {
    let keep = preferred_languages();
    let mut items = Vec::new();

    let Ok(apps) = std::fs::read_dir("/Applications") else {
        return items;
    };

    for app in apps.filter_map(std::result::Result::ok) {
        if cancel.is_cancelled() {
            break;
        }

        let resources = app.path().join("Contents/Resources");
        let Ok(entries) = std::fs::read_dir(&resources) else {
            continue;
        };

        let bundles: Vec<PathBuf> = entries
            .filter_map(std::result::Result::ok)
            .map(|entry| entry.path())
            .filter(|path| {
                let name = format::file_name(path);
                let Some(language) = name.strip_suffix(".lproj") else {
                    return false;
                };
                // `Base` holds the layout every localisation falls back to.
                language != "Base" && !keep.iter().any(|wanted| language.starts_with(wanted))
            })
            .collect();

        let sizes = walker::sizes_of(&bundles);

        items.extend(
            bundles
                .into_iter()
                .zip(sizes)
                .filter(|(_, size)| *size >= MIN_ITEM_SIZE)
                .map(|(path, size)| {
                    item(&path, size, CleanCategoryId::LanguageFiles, RiskLevel::Caution, None)
                }),
        );
    }

    items
}

/// Language codes to keep, from the user's own preference list.
fn preferred_languages() -> Vec<String> {
    let mut languages: Vec<String> = super::run_tool("defaults", &["read", "-g", "AppleLanguages"])
        .map(|output| {
            output
                .lines()
                .filter_map(|line| {
                    let cleaned = line.trim().trim_end_matches(',').trim_matches('"');
                    // Keep the base code: `en-GB` still wants `en.lproj`.
                    cleaned
                        .split('-')
                        .next()
                        .filter(|code| code.len() == 2 && code.chars().all(char::is_alphabetic))
                        .map(str::to_string)
                })
                .collect()
        })
        .unwrap_or_default();

    // English resources are the fallback for almost every app.
    if !languages.iter().any(|code| code == "en") {
        languages.push("en".to_string());
    }

    languages
}

/// Symlinks in the usual dumping grounds whose target has gone away.
fn broken_symlinks(cancel: &CancelToken) -> Vec<CleanableItem> {
    let home = permissions::home_dir();
    let mut items = Vec::new();

    for root in [
        home.join("Library/Application Support"),
        home.join("Library/Preferences"),
        home.join("Library/LaunchAgents"),
    ] {
        if cancel.is_cancelled() {
            break;
        }

        let walk = walkdir::WalkDir::new(&root)
            .max_depth(3)
            .follow_links(false)
            .into_iter();

        for entry in walk.filter_map(std::result::Result::ok) {
            if !entry.path_is_symlink() {
                continue;
            }
            // `exists` follows the link, so a false here means it dangles.
            if entry.path().exists() {
                continue;
            }
            if permissions::ensure_removable(entry.path()).is_err() {
                continue;
            }

            // A dangling link occupies almost nothing, but it is still clutter,
            // so it bypasses the minimum-size filter.
            items.push(item(
                entry.path(),
                0,
                CleanCategoryId::BrokenSymlinks,
                RiskLevel::Safe,
                None,
            ));
        }
    }

    items
}

/// Whether nothing has modified `path` for `days`.
fn older_than(path: &Path, days: u64) -> bool {
    let Ok(metadata) = std::fs::symlink_metadata(path) else {
        return false;
    };
    let Ok(modified) = metadata.modified() else {
        return false;
    };

    modified
        .elapsed()
        .map(|age| age.as_secs() > days * 24 * 60 * 60)
        .unwrap_or(false)
}

/// Rough junk total for the dashboard tile. Deliberately limited to the
/// biggest, cheapest-to-measure locations so the dashboard stays responsive.
///
/// Only `~/Library/Caches` and `~/Library/Logs` are measured — `~/.Trash` is
/// excluded because QuickClean now permanently deletes junk (instead of moving
/// it to Trash), so counting Trash would create a circular dependency.
///
/// The same permission and minimum-size filters as `scan_junk` are applied so
/// the estimate only reports junk that can actually be cleaned.
pub fn quick_junk_estimate() -> u64 {
    let home = permissions::home_dir();

    let roots = [
        home.join("Library/Caches"),
        home.join("Library/Logs"),
    ];

    // Collect removable children from all roots, applying the same permission
    // guard as `children_of` so we never count items that can't be deleted.
    let candidates: Vec<PathBuf> = roots
        .iter()
        .filter(|root| root.is_dir())
        .filter_map(|root| std::fs::read_dir(root).ok())
        .flatten()
        .filter_map(std::result::Result::ok)
        .map(|entry| entry.path())
        .filter(|path| permissions::ensure_removable(path).is_ok())
        .collect();

    // Size each candidate in parallel and skip entries below the minimum,
    // matching the `children_of` filtering used by the full scan.
    walker::sizes_of(&candidates)
        .into_iter()
        .filter(|size| *size >= MIN_ITEM_SIZE)
        .sum()
}

pub(crate) fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_category_has_presentation_metadata() {
        for id in all_categories() {
            assert!(!label_of(id).is_empty());
            assert!(!description_of(id).is_empty());
        }
    }

    #[test]
    fn progress_event_ids_match_serialised_category_ids() {
        // The progress event carries the kebab-case id so the frontend can
        // localise the current step; it must stay in sync with the wire
        // format of `CleanCategoryId` itself.
        for id in all_categories() {
            assert_eq!(
                serde_json::to_value(id).unwrap(),
                serde_json::json!(id_of(id)),
                "{id:?} must emit its serialised id"
            );
        }
    }

    #[test]
    fn backups_are_flagged_as_dangerous() {
        assert_eq!(risk_of(CleanCategoryId::IosBackups), RiskLevel::Danger);
        assert_eq!(risk_of(CleanCategoryId::UserCache), RiskLevel::Safe);
        assert_eq!(risk_of(CleanCategoryId::Downloads), RiskLevel::Caution);
    }

    #[test]
    fn children_of_skips_small_entries_and_missing_roots() {
        let root = PathBuf::from("/tmp").join(format!("windle-clean-{}", std::process::id()));
        std::fs::remove_dir_all(&root).ok();
        std::fs::create_dir_all(root.join("big")).unwrap();
        std::fs::write(root.join("big/blob.bin"), vec![0u8; MIN_ITEM_SIZE as usize + 1]).unwrap();
        std::fs::write(root.join("tiny.bin"), b"nope").unwrap();

        let items = children_of(
            &root,
            CleanCategoryId::UserCache,
            RiskLevel::Safe,
            None,
            &CancelToken::default(),
            &|_| true,
        );

        assert_eq!(items.len(), 1);
        assert_eq!(items[0].path, root.join("big").to_string_lossy());

        // A path that does not exist is not an error.
        assert!(children_of(
            &root.join("missing"),
            CleanCategoryId::UserCache,
            RiskLevel::Safe,
            None,
            &CancelToken::default(),
            &|_| true,
        )
        .is_empty());

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_cancelled_scan_reports_nothing() {
        let cancel = CancelToken::default();
        cancel.cancel();

        assert!(scan_category(CleanCategoryId::UserCache, &cancel).is_empty());
    }

    #[test]
    fn english_is_always_a_preferred_language() {
        assert!(preferred_languages().contains(&"en".to_string()));
    }

    #[test]
    fn user_caches_are_not_double_counted_as_browser_caches() {
        let browsers = browser_cache_roots();
        let items = scan_category(CleanCategoryId::UserCache, &CancelToken::default());

        for item in items {
            let path = PathBuf::from(&item.path);
            assert!(
                !browsers.iter().any(|browser| path.starts_with(browser)),
                "{} belongs to the browser category",
                item.path
            );
        }
    }

    #[test]
    fn browser_entries_and_framework_dirs_are_recognised() {
        // Browser bundles under WebKit / HTTPStorages, including their
        // `.binarycookies` siblings …
        assert!(is_browser_entry("com.apple.Safari"));
        assert!(is_browser_entry("com.apple.Safari.binarycookies"));
        assert!(is_browser_entry("com.google.Chrome"));
        assert!(is_browser_entry("Firefox"));
        assert!(is_browser_entry("com.microsoft.edgemac"));
        assert!(is_browser_entry("company.thebrowser.Browser"));
        // … and ordinary apps plus framework-level directories are not.
        assert!(!is_browser_entry("com.readdle.PDFExpert-Mac"));
        assert!(!is_browser_entry("ChatGPTHelper.binarycookies"));
        assert!(!is_browser_entry("Databases"));
        assert!(!is_browser_entry("WebPush"));

        // The WebKit group skips browser bundles and framework directories …
        assert!(!accept_webkit_entry("com.apple.Safari"));
        assert!(!accept_webkit_entry("Databases"));
        assert!(!accept_webkit_entry("WebPush"));
        assert!(accept_webkit_entry("com.readdle.PDFExpert-Mac"));

        // … and the website-data group skips browser bundles plus any
        // `.binarycookies` login file, browser or not.
        assert!(!accept_http_storage_entry("com.apple.Safari"));
        assert!(!accept_http_storage_entry("ChatGPTHelper.binarycookies"));
        assert!(accept_http_storage_entry("com.readdle.PDFExpert-Mac"));
    }

    #[test]
    fn app_logs_items_carry_one_of_the_expected_groups() {
        let known = [
            "user-logs",
            "crash-reports",
            "system-logs",
            "sandbox-logs",
        ];

        for item in scan_category(CleanCategoryId::AppLogs, &CancelToken::default()) {
            let group = item
                .group
                .as_ref()
                .unwrap_or_else(|| panic!("ungrouped app-logs item: {}", item.path));
            assert!(
                known.contains(&group.id.as_str()),
                "unexpected group {} for {}",
                group.id,
                item.path
            );
        }
    }

    #[test]
    fn groups_serialise_as_id_label_objects() {
        let json = serde_json::to_value(ItemGroup {
            id: "crash-reports".into(),
            label: "Crash Reports".into(),
        })
        .unwrap();

        assert_eq!(json["id"], "crash-reports");
        assert_eq!(json["label"], "Crash Reports");
    }

    #[test]
    fn children_of_tags_items_with_the_requested_group() {
        let root = PathBuf::from("/tmp").join(format!("windle-clean-group-{}", std::process::id()));
        std::fs::remove_dir_all(&root).ok();
        std::fs::create_dir_all(root.join("Reports")).unwrap();
        std::fs::write(root.join("Reports/panic.log"), vec![0u8; MIN_ITEM_SIZE as usize + 1]).unwrap();

        let grouped = children_of(
            &root,
            CleanCategoryId::AppLogs,
            RiskLevel::Safe,
            Some(GROUP_CRASH_REPORTS),
            &CancelToken::default(),
            &|_| true,
        );

        assert_eq!(grouped.len(), 1);
        let group = grouped[0].group.as_ref().expect("items must carry the group");
        assert_eq!(group.id, "crash-reports");
        assert_eq!(group.label, "Crash Reports");

        // Without a group the field is simply null on the wire.
        let ungrouped = children_of(
            &root,
            CleanCategoryId::AppLogs,
            RiskLevel::Safe,
            None,
            &CancelToken::default(),
            &|_| true,
        );
        assert!(ungrouped[0].group.is_none());

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn app_support_scan_matches_only_cache_named_directories() {
        let root = std::env::temp_dir().join(format!("windle-clean-appcache-{}", std::process::id()));
        std::fs::remove_dir_all(&root).ok();

        // Apps that keep an explicitly named cache directory …
        std::fs::create_dir_all(root.join("com.example.CleanApp/Cache")).unwrap();
        std::fs::write(root.join("com.example.CleanApp/Cache/blob.bin"), b"junk").unwrap();
        std::fs::create_dir_all(root.join("com.example.PluralApp/Caches")).unwrap();
        // … an app whose sub-directory is real user data …
        std::fs::create_dir_all(root.join("com.example.DataApp/Documents")).unwrap();
        // … and a mere file that happens to be called `Cache`.
        std::fs::create_dir_all(root.join("com.example.FileApp")).unwrap();
        std::fs::write(root.join("com.example.FileApp/Cache"), b"not a directory").unwrap();

        let mut found = app_support_cache_dirs(&root);
        found.sort();

        assert_eq!(
            found,
            vec![
                root.join("com.example.CleanApp/Cache"),
                root.join("com.example.PluralApp/Caches"),
            ]
        );

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn sandbox_log_roots_cover_containers_and_group_containers() {
        let home = std::env::temp_dir().join(format!("windle-clean-sbx-{}", std::process::id()));
        std::fs::remove_dir_all(&home).ok();

        // A sandboxed app container with the standard Data/Library/Logs layout.
        std::fs::create_dir_all(home.join("Library/Containers/com.example.App/Data/Library/Logs"))
            .unwrap();
        // A container without logs — contributes nothing.
        std::fs::create_dir_all(home.join("Library/Containers/com.example.NoLogs/Data/Library"))
            .unwrap();
        // App-group containers using either of the two known layouts.
        std::fs::create_dir_all(home.join("Library/Group Containers/ABC123.Group/Logs")).unwrap();
        std::fs::create_dir_all(
            home.join("Library/Group Containers/XYZ789.Group/Data/Library/Logs"),
        )
        .unwrap();

        let mut found = sandbox_log_roots(&home);
        found.sort();

        assert_eq!(
            found,
            vec![
                home.join("Library/Containers/com.example.App/Data/Library/Logs"),
                home.join("Library/Group Containers/ABC123.Group/Logs"),
                home.join("Library/Group Containers/XYZ789.Group/Data/Library/Logs"),
            ]
        );

        std::fs::remove_dir_all(&home).ok();
    }

    #[test]
    fn app_logs_items_carry_the_group_of_their_source() {
        let home = permissions::home_dir();
        let items = scan_category(CleanCategoryId::AppLogs, &CancelToken::default());

        for item in &items {
            let path = Path::new(&item.path);
            let group = item.group.as_ref().map(|group| group.id.as_str());

            let expected = if path.starts_with(home.join("Library/Logs/DiagnosticReports"))
                || path.starts_with(home.join("Library/Application Support/CrashReporter"))
                || path.starts_with("/Library/Application Support/CrashReporter")
                || path.starts_with("/Library/Logs/DiagnosticReports")
            {
                Some("crash-reports")
            } else if path.starts_with("/private/var/log")
                || path.starts_with("/private/var/db/diagnostics")
                || path.starts_with("/Library/Logs")
            {
                Some("system-logs")
            } else if path.starts_with(home.join("Library/Containers"))
                || path.starts_with(home.join("Library/Group Containers"))
            {
                Some("sandbox-logs")
            } else {
                assert!(
                    path.starts_with(home.join("Library/Logs")),
                    "unexpected app-logs source: {}",
                    item.path
                );
                Some("user-logs")
            };

            assert_eq!(group, expected, "wrong group for {}", item.path);
        }
    }

    #[test]
    fn app_logs_and_app_junk_never_report_a_path_twice() {
        for id in [CleanCategoryId::AppLogs, CleanCategoryId::AppJunk] {
            let items = scan_category(id, &CancelToken::default());
            let mut seen = std::collections::HashSet::new();

            for item in items {
                assert!(
                    seen.insert(item.path.clone()),
                    "{} is reported twice in {:?}",
                    item.path,
                    id
                );
            }
        }
    }

    #[test]
    fn app_junk_excludes_browser_caches() {
        let browsers = browser_cache_roots();
        let items = scan_category(CleanCategoryId::AppJunk, &CancelToken::default());

        for item in items {
            let path = PathBuf::from(&item.path);
            assert!(
                !browsers.iter().any(|browser| path.starts_with(browser)),
                "{} belongs to the browser category",
                item.path
            );

            // The WebKit / website-data groups must also skip browser
            // bundles, framework-level directories and `.binarycookies`
            // login files.
            if let Some(group) = item.group.as_ref() {
                let name = format::file_name(&path);
                match group.id.as_str() {
                    "webkit" => assert!(
                        accept_webkit_entry(&name),
                        "{} must not be offered under the WebKit group",
                        item.path
                    ),
                    "http-storages" => assert!(
                        accept_http_storage_entry(&name),
                        "{} must not be offered under the website-data group",
                        item.path
                    ),
                    _ => {}
                }
            }
        }
    }

    #[test]
    fn app_junk_items_carry_one_of_the_expected_groups() {
        let known = [
            "app-caches",
            "http-storages",
            "saved-state",
            "webkit",
            "sandbox-caches",
        ];

        for item in scan_category(CleanCategoryId::AppJunk, &CancelToken::default()) {
            let group = item
                .group
                .as_ref()
                .unwrap_or_else(|| panic!("ungrouped app-junk item: {}", item.path));
            assert!(
                known.contains(&group.id.as_str()),
                "unexpected group {} for {}",
                group.id,
                item.path
            );
        }
    }
}
