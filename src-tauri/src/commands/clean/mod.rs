//! Deep Clean — caches, logs and other junk that is safe to remove.
//!
//! The rules, the wire types and the commands are the same everywhere; which
//! directories count as junk is not, so each platform has its own scan in a
//! sibling module.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, State};

use super::{CleanOutcome, RiskLevel};
use crate::scanner::walker::{self, CancelToken};
use crate::scanner::ScanProgress;
use crate::utils::fs_ops::{self, RemoveMode};
use crate::utils::{format, history, permissions, WindleError, Result};

#[cfg(target_os = "macos")]
#[path = "macos.rs"]
mod imp;
#[cfg(target_os = "windows")]
#[path = "windows.rs"]
mod imp;

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

/// The sub-groups the deep-clean UI renders inside a category. Ids are part of
/// the frontend contract: the UI localises the ones it knows and falls back to
/// the label sent here for the rest.
const GROUP_CRASH_REPORTS: GroupSpec = ("crash-reports", "Crash Reports");
const GROUP_SYSTEM_LOGS: GroupSpec = ("system-logs", "System Logs");
const GROUP_SYSTEM_CACHES: GroupSpec = ("system-caches", "System Caches");
const GROUP_TEMP_FILES: GroupSpec = ("temp-files", "Temporary Files");
const GROUP_USER_LOGS: GroupSpec = ("user-logs", "User Logs");
const GROUP_APP_CACHES: GroupSpec = ("app-caches", "App Caches");

#[cfg(target_os = "macos")]
const GROUP_HTTP_STORAGES: GroupSpec = ("http-storages", "Website Data");
#[cfg(target_os = "macos")]
const GROUP_SANDBOX_LOGS: GroupSpec = ("sandbox-logs", "Sandboxed App Logs");
#[cfg(target_os = "macos")]
const GROUP_SAVED_STATE: GroupSpec = ("saved-state", "Saved Application State");
#[cfg(target_os = "macos")]
const GROUP_WEBKIT: GroupSpec = ("webkit", "WebKit Data");
#[cfg(target_os = "macos")]
const GROUP_SANDBOX_CACHES: GroupSpec = ("sandbox-caches", "Sandboxed App Caches");

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
        // The Recycle Bin is the one item that is not a path: the shell owns
        // it, so it never reaches the filesystem guard rails (which would
        // rightly refuse a directory that does not exist).
        #[cfg(target_os = "windows")]
        if path == fs_ops::RECYCLE_BIN_SENTINEL {
            match fs_ops::empty_recycle_bin() {
                Ok(freed) => outcome.succeed(path, freed),
                Err(error) => outcome.fail(path, error.to_string()),
            }
            continue;
        }

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
    let mut outcome = CleanOutcome::default();

    imp::empty_trash(&mut outcome)?;

    // An operation that freed nothing is not recorded, so the trash that was
    // already empty needs no special case here.
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

fn describe(path: &Path, category: CleanCategoryId, risk: RiskLevel) -> Option<CleanableItem> {
    describe_in(path, category, risk, None)
}

/// [`describe`] with a sub-group, for whole directories offered as one item.
fn describe_in(
    path: &Path,
    category: CleanCategoryId,
    risk: RiskLevel,
    group: Option<GroupSpec>,
) -> Option<CleanableItem> {
    let size = walker::size_of_any(path);
    if size < MIN_ITEM_SIZE {
        return None;
    }

    let group = group.map(|(id, label)| ItemGroup {
        id: id.to_string(),
        label: label.to_string(),
    });

    Some(item(path, size, category, risk, group.as_ref()))
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

pub(crate) fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Enumerate the items for one category.
fn scan_category(id: CleanCategoryId, cancel: &CancelToken) -> Vec<CleanableItem> {
    imp::scan_category(id, cancel)
}

/// Rough junk total for the dashboard tile, measured by the platform-specific
/// scan.
pub fn quick_junk_estimate() -> u64 {
    imp::quick_junk_estimate()
}

#[cfg(test)]
mod tests {
    use super::*;

    // The tests below only exercise the shared plumbing; the platform-specific
    // scans are tested next to the code that defines them.
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
        let root = crate::utils::test_support::scratch("clean-small");
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
        let root = crate::utils::test_support::scratch("clean-group");
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
    }
}
