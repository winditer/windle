//! The Windows Deep Clean scan: the caches, logs and leftovers Windows keeps
//! under `%LOCALAPPDATA%`, `%APPDATA%`, `%ProgramData%` and `%SystemRoot%`.
//!
//! Every path offered here is a *whole* directory or file that Windows or an
//! application rebuilds on demand. Two rules shape the lists:
//!
//! * Nothing is ever guessed: a location only appears once its layout is
//!   known, so a wrong guess can only mean a missed cache, never a deleted
//!   document. Where a directory name is the identifier (`Cache`, `logs`, …)
//!   the scan looks for the name instead of trusting a fixed path.
//! * Anything outside the user's own profile needs administrator rights to
//!   delete. The scan still reports those items — the guard rails turn the
//!   removal into a `needs-elevation` failure the UI already knows how to
//!   explain.

use std::path::{Path, PathBuf};

use super::super::{CleanOutcome, RiskLevel};
use super::{
    children_of, describe, describe_in, list_children, older_than, risk_of, CleanCategoryId,
    CleanableItem, GroupSpec, DOWNLOAD_AGE_DAYS, GROUP_APP_CACHES, GROUP_CRASH_REPORTS,
    GROUP_SYSTEM_CACHES, GROUP_SYSTEM_LOGS, GROUP_TEMP_FILES, GROUP_USER_LOGS, MIN_ITEM_SIZE,
    TEMP_AGE_DAYS,
};
use crate::scanner::walker::{self, CancelToken};
use crate::utils::{format, fs_ops, permissions};

/// Windows-managed caches that belong to the shell rather than to one app.
const GROUP_SHELL_CACHES: GroupSpec = ("shell-caches", "Windows Caches");
/// Compilers, package managers and IDEs keep their download caches here.
const GROUP_DEV_CACHES: GroupSpec = ("dev-caches", "Developer Caches");

/// Directory names that are unambiguously a cache wherever they appear. Kept
/// to names no application uses for real data.
const CACHE_DIR_NAMES: &[&str] = &[
    // Generic names.
    "Cache",
    "Caches",
    // Chromium and Electron: the browser cache, its GPU shader caches, the
    // code cache that holds compiled JavaScript, and the service-worker store.
    "CacheStorage",
    "Code Cache",
    "DawnCache",
    "DawnGraphiteCache",
    "DawnWebGPUCache",
    "GPUCache",
    "GrShaderCache",
    "ShaderCache",
    // Media players buffer decoded frames and audio here.
    "Media Cache",
    // Firefox.
    "cache2",
    "startupCache",
    // Chromium's component and extension download caches.
    "component_crx_cache",
    "extensions_crx_cache",
];

/// Directory names that hold diagnostics rather than data.
const LOG_DIR_NAMES: &[&str] = &["Crashpad", "logs", "Logs"];

/// Browsers keep their caches in these trees, under a profile directory. They
/// are scanned by name so the scan survives layout changes, and they are
/// excluded from the app-junk walk so no path is offered twice.
const BROWSER_ROOTS: &[&str] = &[
    "%LOCALAPPDATA%\\BraveSoftware\\Brave-Browser\\User Data",
    "%LOCALAPPDATA%\\Chromium\\User Data",
    "%LOCALAPPDATA%\\Google\\Chrome\\User Data",
    "%LOCALAPPDATA%\\Microsoft\\Edge\\User Data",
    "%LOCALAPPDATA%\\Vivaldi\\User Data",
    // Opera keeps one profile instead of a `User Data` tree.
    "%LOCALAPPDATA%\\Opera Software\\Opera GX Stable",
    "%LOCALAPPDATA%\\Opera Software\\Opera Stable",
];

/// The first path component of each entry above, so the app walk can skip
/// browser-owned trees (`%LOCALAPPDATA%\Google`, `%LOCALAPPDATA%\Mozilla`, …).
/// Edge is absent from this list because it ships under several channel names
/// below `Microsoft`, which [`is_windows_owned`] covers by prefix.
const BROWSER_VENDORS: &[&str] = &[
    "BraveSoftware",
    "Chromium",
    "Google",
    "Mozilla",
    "Opera Software",
    "Vivaldi",
];

/// First-level data directories that are not application trees: Windows' own
/// state (its categories name the pieces that are junk), installed programs,
/// and the shared temp area.
const SYSTEM_OWNED_DIRS: &[&str] = &[
    "Comms",
    "ConnectedDevicesPlatform",
    "ElevatedDiagnostics",
    "Packages",
    "Programs",
    "Publishers",
    "Temp",
];

/// Developer-tool trees whose caches the dev-cache list names exactly, so the
/// generic walks step around them and no path is offered twice.
const DEV_TOOL_DIRS: &[&str] = &["NuGet", "Yarn", "pip"];

/// Children of `Microsoft` that belong to Windows rather than to one of its
/// applications: credentials, certificates and the runtime state of the shell
/// itself. Everything else below `Microsoft` is application data.
const WINDOWS_OWNED: &[&str] = &[
    "Crypto",
    "Credentials",
    "Internet Explorer",
    "OneAuth",
    "Passport",
    "Protect",
    "SystemCertificates",
    "TokenBroker",
    "Vault",
    "Windows",
];

/// Whether a `Microsoft` child is Windows' own state. Edge ships as `Edge`,
/// `Edge Beta`, `Edge Dev` and `Edge SxS`, so its channel names are matched by
/// prefix; the browser scan names each profile's caches directly.
fn is_windows_owned(name: &str) -> bool {
    WINDOWS_OWNED.contains(&name) || name.starts_with("Edge")
}

/// Which browser-profile directories hold a cache: the profile itself
/// (`Default`, `Profile 1`, …) nests them one level down.
fn browser_caches(profile: &Path) -> Vec<PathBuf> {
    CACHE_DIR_NAMES
        .iter()
        .map(|name| profile.join(name))
        .filter(|path| path.is_dir())
        .collect()
}

/// The application directories under a data root that the junk walks may
/// descend into.
///
/// `Microsoft` needs the extra step: Windows and its applications share the
/// tree, and only the caches below the applications may be touched. The other
/// excluded names are documented next to their lists.
fn third_party_app_dirs(root: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();

    for child in list_children(root) {
        if !child.is_dir() {
            continue;
        }

        let name = format::file_name(&child);

        if name == "Microsoft" {
            for app in list_children(&child) {
                let app_name = format::file_name(&app);
                if app.is_dir() && !is_windows_owned(&app_name) {
                    found.push(app);
                }
            }
            continue;
        }

        if BROWSER_VENDORS.contains(&name.as_str())
            || SYSTEM_OWNED_DIRS.contains(&name.as_str())
            || DEV_TOOL_DIRS.contains(&name.as_str())
        {
            continue;
        }

        found.push(child);
    }

    found
}

/// Cache and log directories belonging to one application's data directory:
/// the directory itself (`<app>\Cache`) and one level down
/// (`<app>\<version>\Cache`), matching the two layouts apps use.
fn app_data_junk(app_dir: &Path, names: &[&str]) -> Vec<PathBuf> {
    let mut found = Vec::new();

    for child in list_children(app_dir) {
        if !child.is_dir() {
            continue;
        }

        let name = format::file_name(&child);
        if names.contains(&name.as_str()) {
            found.push(child);
            continue;
        }

        for grandchild in list_children(&child) {
            if grandchild.is_dir() && names.contains(&format::file_name(&grandchild).as_str()) {
                found.push(grandchild);
            }
        }
    }

    found
}

/// Collect the named directories under every application directory, offering
/// each as one item.
fn app_data_items(
    names: &[&str],
    category: CleanCategoryId,
    risk: RiskLevel,
    group: GroupSpec,
) -> Vec<CleanableItem> {
    let local = permissions::expand("%LOCALAPPDATA%");
    let roaming = permissions::expand("%APPDATA%");

    [local, roaming]
        .iter()
        .flat_map(|root| third_party_app_dirs(root))
        .flat_map(|app_dir| app_data_junk(&app_dir, names))
        .filter_map(|dir| describe_in(&dir, category, risk, Some(group)))
        .collect()
}

/// The directory holding the user's temporary files.
fn user_temp() -> PathBuf {
    permissions::expand("%TEMP%")
}

pub(super) fn scan_category(id: CleanCategoryId, cancel: &CancelToken) -> Vec<CleanableItem> {
    use CleanCategoryId::*;

    let home = permissions::home_dir();
    let risk = risk_of(id);

    match id {
        UserCache => {
            let mut items = Vec::new();

            // The user's temp directory is shared with every running program,
            // so only entries nothing has touched for a week are offered.
            items.extend(children_of(
                &user_temp(),
                id,
                risk,
                Some(GROUP_TEMP_FILES),
                cancel,
                &|path| older_than(path, TEMP_AGE_DAYS),
            ));

            // The internet cache and the shell's thumbnail caches: scratch that
            // Windows rebuilds without asking.
            items.extend(children_of(
                &permissions::expand("%LOCALAPPDATA%\\Microsoft\\Windows\\INetCache"),
                id,
                risk,
                Some(GROUP_SHELL_CACHES),
                cancel,
                // Outlook's attachment cache is the mail category's business.
                &|path| format::file_name(path) != "Content.Outlook",
            ));
            items.extend(children_of(
                &permissions::expand("%LOCALAPPDATA%\\Microsoft\\Windows\\Explorer"),
                id,
                risk,
                Some(GROUP_SHELL_CACHES),
                cancel,
                &|path| {
                    let name = format::file_name(path);
                    name.starts_with("thumbcache_") || name.starts_with("iconcache_")
                },
            ));

            // Store applications keep their scratch state beside their data.
            // `Content.Outlook` is skipped here as well: it belongs to the mail
            // category whichever package it sits in.
            for package in list_children(&permissions::expand("%LOCALAPPDATA%\\Packages")) {
                for sub in [
                    "AC\\INetCache",
                    "AC\\Temp",
                    "LocalCache\\Local\\Microsoft\\Windows\\INetCache",
                    "TempState",
                ] {
                    let dir = package.join(sub);
                    if dir.is_dir() {
                        items.extend(children_of(
                            &dir,
                            id,
                            risk,
                            Some(GROUP_APP_CACHES),
                            cancel,
                            &|path| format::file_name(path) != "Content.Outlook",
                        ));
                    }
                }
            }

            items
        }
        SystemCache => {
            let mut items = Vec::new();

            // Machine-wide scratch areas. They are shared with services and
            // running installers, so only stale entries are offered.
            for entry in ["%SystemRoot%\\Temp", "%ProgramData%\\Temp"] {
                items.extend(children_of(
                    &permissions::expand(entry),
                    id,
                    risk,
                    Some(GROUP_TEMP_FILES),
                    cancel,
                    &|path| older_than(path, TEMP_AGE_DAYS),
                ));
            }

            // The prefetcher's lookup data. Windows rebuilds it as programs
            // start, at the cost of a slower first launch.
            items.extend(children_of(
                &permissions::expand("%SystemRoot%\\Prefetch"),
                id,
                risk,
                Some(GROUP_SYSTEM_CACHES),
                cancel,
                &|_| true,
            ));

            items
        }
        AppLogs => {
            let mut items = Vec::new();

            // Crash reports and the Windows Error Reporting queue, for this
            // user and for the machine.
            for (entry, group) in [
                ("%LOCALAPPDATA%\\CrashDumps", GROUP_CRASH_REPORTS),
                ("%LOCALAPPDATA%\\Microsoft\\Windows\\WER\\ReportArchive", GROUP_CRASH_REPORTS),
                ("%LOCALAPPDATA%\\Microsoft\\Windows\\WER\\ReportQueue", GROUP_CRASH_REPORTS),
                ("%ProgramData%\\Microsoft\\Windows\\WER\\ReportArchive", GROUP_CRASH_REPORTS),
                ("%ProgramData%\\Microsoft\\Windows\\WER\\ReportQueue", GROUP_CRASH_REPORTS),
                ("%ProgramData%\\Microsoft\\Windows\\WER\\Temp", GROUP_CRASH_REPORTS),
                ("%SystemRoot%\\LiveKernelReports", GROUP_CRASH_REPORTS),
                ("%SystemRoot%\\Minidump", GROUP_CRASH_REPORTS),
                // Component and servicing logs: CBS, DISM, Windows Update.
                ("%SystemRoot%\\Logs", GROUP_SYSTEM_LOGS),
                ("%SystemRoot%\\System32\\LogFiles", GROUP_SYSTEM_LOGS),
            ] {
                items.extend(children_of(
                    &permissions::expand(entry),
                    id,
                    risk,
                    Some(group),
                    cancel,
                    &|_| true,
                ));
            }

            // Logs the applications themselves keep in their data directories.
            items.extend(app_data_items(LOG_DIR_NAMES, id, risk, GROUP_USER_LOGS));

            items
        }
        AppJunk => {
            let mut items = Vec::new();

            // Cache directories inside application data, in both the roaming
            // and the local tree.
            items.extend(app_data_items(CACHE_DIR_NAMES, id, risk, GROUP_APP_CACHES));

            // WebView2 — the Chromium runtime inside Windows apps — keeps a
            // full browser profile per application, nested deeper than the
            // generic walk reaches. Only the cache directories inside those
            // profiles are offered, so sign-ins and site data survive.
            let local = permissions::expand("%LOCALAPPDATA%");
            for root in walker::find_dirs_named(&local, &["EBWebView"], 3) {
                for profile in list_children(&root) {
                    items.extend(
                        browser_caches(&profile)
                            .into_iter()
                            .filter_map(|dir| describe_in(&dir, id, risk, Some(GROUP_APP_CACHES))),
                    );
                }
            }

            items
        }
        BrowserCache => {
            let mut items = Vec::new();

            for entry in BROWSER_ROOTS {
                for profile in list_children(&permissions::expand(entry)) {
                    items.extend(
                        browser_caches(&profile)
                            .into_iter()
                            .filter_map(|dir| describe(&dir, id, risk)),
                    );
                }
            }

            // Firefox names its cache directories after the POSIX convention
            // rather than Chromium's, and stores them per profile.
            for entry in [
                "%LOCALAPPDATA%\\Mozilla\\Firefox\\Profiles",
                "%APPDATA%\\Mozilla\\Firefox\\Profiles",
            ] {
                for profile in list_children(&permissions::expand(entry)) {
                    for name in ["cache2", "startupCache"] {
                        let dir = profile.join(name);
                        if dir.is_dir() {
                            items.extend(describe(&dir, id, risk));
                        }
                    }
                }
            }

            items
        }
        Trash => {
            let (bytes, count) = fs_ops::recycle_bin_report();

            if count == 0 || bytes < MIN_ITEM_SIZE {
                return Vec::new();
            }

            // Windows ships the bin with a single synthetic entry: the bin has
            // no directory to enumerate, so the shell reports its size instead.
            vec![CleanableItem {
                id: fs_ops::RECYCLE_BIN_SENTINEL.to_string(),
                path: fs_ops::RECYCLE_BIN_SENTINEL.to_string(),
                size: bytes,
                modified_at: None,
                category: id,
                risk,
                description: if count == 1 {
                    "1 item".into()
                } else {
                    format!("{count} items")
                },
                group: None,
            }]
        }
        Downloads => children_of(&home.join("Downloads"), id, risk, None, cancel, &|path| {
            older_than(path, DOWNLOAD_AGE_DAYS)
        }),
        MailAttachments => {
            let mut items = Vec::new();

            // Outlook writes attachment previews into the internet cache, which
            // is why the user-cache scan steps around this one directory.
            let outlook = permissions::expand(
                "%LOCALAPPDATA%\\Microsoft\\Windows\\INetCache\\Content.Outlook",
            );
            if outlook.is_dir() {
                items.extend(describe(&outlook, id, risk));
            }

            // Store-installed mail clients — Mail, the new Outlook — keep the
            // same `Content.Outlook` folder inside their package data. Only
            // packages that say they are mail clients are walked, and only the
            // folder Outlook itself names is offered.
            let packages = permissions::expand("%LOCALAPPDATA%\\Packages");
            for package in list_children(&packages) {
                let name = format::file_name(&package).to_lowercase();
                let is_mail_client = ["outlook", "mail", "communicationsapps"]
                    .iter()
                    .any(|hint| name.contains(hint));
                if !is_mail_client {
                    continue;
                }

                for dir in walker::find_dirs_named(&package, &["Content.Outlook"], 8) {
                    items.extend(describe(&dir, id, risk));
                }
            }

            items
        }
        XcodeDerivedData => {
            let mut items = Vec::new();

            // Compiler and package-manager caches: everything here is either
            // downloaded or derived, and is rebuilt — or re-downloaded — on the
            // next build. Each tree is named in full rather than searched for,
            // because these are the directories the tools document.
            for entry in [
                "%LOCALAPPDATA%\\NuGet\\Cache",
                "%LOCALAPPDATA%\\NuGet\\v3-cache",
                "%LOCALAPPDATA%\\npm-cache",
                "%LOCALAPPDATA%\\pip\\Cache",
                "%LOCALAPPDATA%\\Yarn\\Cache",
                "%USERPROFILE%\\.cargo\\registry\\cache",
                "%USERPROFILE%\\.gradle\\caches",
            ] {
                let dir = permissions::expand(entry);
                if dir.is_dir() {
                    items.extend(describe_in(&dir, id, risk, Some(GROUP_DEV_CACHES)));
                }
            }

            // Visual Studio keeps one directory per installed version, and each
            // holds the design-time and language-service caches.
            let studio = permissions::expand("%LOCALAPPDATA%\\Microsoft\\VisualStudio");
            for version in list_children(&studio) {
                let names = [
                    "ComponentModelCache",
                    "Designer",
                    "ImageLibrary",
                    "Roslyn",
                    "VTCache",
                ];

                for name in names {
                    let dir = version.join(name);
                    if dir.is_dir() {
                        items.extend(describe_in(&dir, id, risk, Some(GROUP_DEV_CACHES)));
                    }
                }
            }

            items
        }
        IosBackups => {
            let mut items = Vec::new();

            // Apple's device backup trees: iTunes, the Apple Devices app and
            // its store-installed flavour keep the same `MobileSync` layout.
            let mut roots = vec![
                permissions::expand("%APPDATA%\\Apple Computer"),
                permissions::expand("%USERPROFILE%\\Apple"),
            ];
            for package in list_children(&permissions::expand("%LOCALAPPDATA%\\Packages")) {
                if format::file_name(&package).starts_with("AppleInc.") {
                    roots.push(package.join("LocalCache\\Roaming\\Apple Computer"));
                }
            }

            for root in roots {
                for backup in walker::find_dirs_named(&root, &["Backup"], 3) {
                    items.extend(children_of(&backup, id, risk, None, cancel, &|_| true));
                }
            }

            items
        }
        // Windows has no counterpart for per-application localisation bundles:
        // language resources ship inside the installation directory (which the
        // guard rails refuse to touch) or as separate system packages, and the
        // files in between belong to running applications. Reporting nothing is
        // the honest answer.
        LanguageFiles => Vec::new(),
        BrokenSymlinks => broken_links(),
    }
}

/// Symbols and junctions in the user's own data whose target has gone away.
///
/// Windows needs administrator rights or developer mode to create symlinks, so
/// these are rare; a mod manager that links files into a game directory leaves
/// them behind, and nothing else cleans them up.
fn broken_links() -> Vec<CleanableItem> {
    let mut items = Vec::new();

    for root in [
        permissions::expand("%APPDATA%"),
        permissions::expand("%LOCALAPPDATA%"),
    ] {
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
            items.push(super::item(
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

/// Empty the Recycle Bin. The shell owns the bin, so there is nothing to
/// enumerate: the category's one synthetic entry names the operation and the
/// shell reports how many bytes that freed.
pub(super) fn empty_trash(outcome: &mut CleanOutcome) -> crate::utils::Result<()> {
    let freed = fs_ops::empty_recycle_bin()?;
    outcome.succeed(fs_ops::RECYCLE_BIN_SENTINEL, freed);
    Ok(())
}

/// Rough junk total for the dashboard tile: the same locations the full scan
/// reports, restricted to the ones that are cheap to measure. The filters match
/// the full scan, so the tile never promises bytes the scan would not offer.
/// The Recycle Bin is left out on purpose — QuickClean deletes junk for good
/// rather than moving it to the bin, so counting the bin would count the same
/// bytes twice.
pub(super) fn quick_junk_estimate() -> u64 {
    let local = permissions::expand("%LOCALAPPDATA%");

    // The user's own scratch: stale temp entries, then the cache directories
    // inside application data, which is where the bulk of the junk sits and
    // where measurement is a plain directory walk.
    let mut candidates: Vec<PathBuf> = list_children(&user_temp())
        .into_iter()
        .filter(|path| older_than(path, TEMP_AGE_DAYS))
        .collect();

    for root in [local.clone(), permissions::expand("%APPDATA%")] {
        for app_dir in third_party_app_dirs(&root) {
            candidates.extend(app_data_junk(&app_dir, CACHE_DIR_NAMES));
        }
    }

    candidates.extend(
        list_children(&local.join("Microsoft\\Windows\\INetCache"))
            .into_iter()
            .filter(|path| format::file_name(path) != "Content.Outlook"),
    );

    // Only count what the guard rails would let the user remove.
    candidates.retain(|path| permissions::ensure_removable(path).is_ok());

    walker::sizes_of(&candidates)
        .into_iter()
        .filter(|size| *size >= MIN_ITEM_SIZE)
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::platform;

    /// Every category must name a risk, and the Trash entry is the only item
    /// whose path is not a path.
    #[test]
    fn the_recycle_bin_entry_is_the_only_synthetic_item() {
        for id in [
            CleanCategoryId::UserCache,
            CleanCategoryId::AppJunk,
            CleanCategoryId::BrowserCache,
        ] {
            for item in scan_category(id, &CancelToken::default()) {
                assert_ne!(item.path, fs_ops::RECYCLE_BIN_SENTINEL);
            }
        }

        for item in scan_category(CleanCategoryId::Trash, &CancelToken::default()) {
            assert_eq!(item.path, fs_ops::RECYCLE_BIN_SENTINEL);
            assert!(item.size >= MIN_ITEM_SIZE);
        }
    }

    /// Nothing the scan offers may be something the guard rails refuse, and
    /// nothing may sit in a category other than the one that reported it.
    #[test]
    fn every_offered_path_is_removable() {
        for id in [
            CleanCategoryId::UserCache,
            CleanCategoryId::SystemCache,
            CleanCategoryId::AppLogs,
            CleanCategoryId::AppJunk,
            CleanCategoryId::BrowserCache,
            CleanCategoryId::XcodeDerivedData,
            CleanCategoryId::MailAttachments,
            CleanCategoryId::BrokenSymlinks,
        ] {
            for item in scan_category(id, &CancelToken::default()) {
                if item.path == fs_ops::RECYCLE_BIN_SENTINEL {
                    continue;
                }

                assert_eq!(item.category, id, "{} is in the wrong category", item.path);
                assert!(
                    permissions::ensure_removable(Path::new(&item.path)).is_ok(),
                    "{} must pass the guard rails",
                    item.path
                );
            }
        }
    }

    /// Paths only ever come out of the trees Windows calls scratch space, so a
    /// typo in one of the lists cannot point the scan at a user document.
    #[test]
    fn the_scan_stays_inside_the_scratch_roots() {
        let allowed = [
            permissions::expand("%TEMP%"),
            permissions::expand("%LOCALAPPDATA%"),
            permissions::expand("%APPDATA%"),
            permissions::expand("%USERPROFILE%"),
            permissions::expand("%ProgramData%"),
            permissions::expand("%SystemRoot%"),
        ];

        for id in [
            CleanCategoryId::UserCache,
            CleanCategoryId::SystemCache,
            CleanCategoryId::AppLogs,
            CleanCategoryId::AppJunk,
            CleanCategoryId::BrowserCache,
            CleanCategoryId::MailAttachments,
            CleanCategoryId::XcodeDerivedData,
            CleanCategoryId::IosBackups,
            CleanCategoryId::BrokenSymlinks,
        ] {
            for item in scan_category(id, &CancelToken::default()) {
                // Nothing under the profile may be offered wholesale, only the
                // named sub-directories of it.
                assert!(
                    allowed
                        .iter()
                        .any(|root| platform::starts_with(Path::new(&item.path), root)),
                    "{} is outside the scratch areas",
                    item.path
                );
                assert!(
                    !platform::eq(Path::new(&item.path), &permissions::expand("%USERPROFILE%")),
                    "the profile itself must never be offered"
                );
            }
        }
    }

    #[test]
    fn browser_vendors_stay_out_of_the_app_junk_walk() {
        let root = crate::utils::test_support::scratch("clean-win-vendors");
        for name in ["Google", "Mozilla", "SomeApp"] {
            std::fs::create_dir_all(root.join(name).join("Cache")).unwrap();
        }

        let mut found: Vec<String> = third_party_app_dirs(&root)
            .into_iter()
            .map(|path| format::file_name(&path))
            .collect();
        found.sort();

        assert_eq!(found, vec!["SomeApp".to_string()]);

        std::fs::remove_dir_all(&root).ok();
    }

    /// `%LOCALAPPDATA%\Microsoft` holds Windows' own state next to the data of
    /// applications that happen to ship under the vendor's name, so the walk
    /// must enter the applications and never the system directories.
    #[test]
    fn the_microsoft_tree_is_split_into_apps_and_windows_state() {
        let root = crate::utils::test_support::scratch("clean-win-microsoft");
        for name in ["Crypto", "Credentials", "Edge", "Edge Beta", "Windows", "Teams"] {
            std::fs::create_dir_all(root.join("Microsoft").join(name)).unwrap();
        }
        std::fs::create_dir_all(root.join("Contoso")).unwrap();

        let mut found: Vec<String> = third_party_app_dirs(&root)
            .into_iter()
            .map(|path| format::file_name(&path))
            .collect();
        found.sort();

        assert_eq!(found, vec!["Contoso".to_string(), "Teams".to_string()]);

        std::fs::remove_dir_all(&root).ok();
    }

    /// The dev-cache trees are named in the dev list, so the generic walks must
    /// not report their caches a second time under another category.
    #[test]
    fn dev_tool_trees_stay_out_of_the_generic_walks() {
        let root = crate::utils::test_support::scratch("clean-win-devtools");
        for name in ["NuGet", "Yarn", "pip", "SomeApp"] {
            std::fs::create_dir_all(root.join(name).join("Cache")).unwrap();
        }

        let mut found: Vec<String> = third_party_app_dirs(&root)
            .into_iter()
            .map(|path| format::file_name(&path))
            .collect();
        found.sort();

        assert_eq!(found, vec!["SomeApp".to_string()]);

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn app_data_junk_finds_both_known_layouts() {
        let root = crate::utils::test_support::scratch("clean-win-layouts");

        // `<app>\Cache` and `<app>\<version>\Code Cache` …
        std::fs::create_dir_all(root.join("Editor/Cache")).unwrap();
        std::fs::create_dir_all(root.join("Editor/1.2.3/Code Cache")).unwrap();
        // … while an application's own documents stay put.
        std::fs::create_dir_all(root.join("Editor/Documents")).unwrap();
        // A file that happens to carry a cache name is not a cache.
        std::fs::create_dir_all(root.join("Editor/Other")).unwrap();
        std::fs::write(root.join("Editor/Other/Cache"), b"not a directory").unwrap();

        let mut found = app_data_junk(&root.join("Editor"), CACHE_DIR_NAMES);
        found.sort();

        assert_eq!(
            found,
            vec![
                root.join("Editor/1.2.3/Code Cache"),
                root.join("Editor/Cache"),
            ]
        );

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn language_files_report_nothing_on_windows() {
        assert!(scan_category(CleanCategoryId::LanguageFiles, &CancelToken::default()).is_empty());
    }

    #[test]
    fn a_cancelled_scan_reports_nothing() {
        let cancel = CancelToken::default();
        cancel.cancel();

        assert!(scan_category(CleanCategoryId::UserCache, &cancel).is_empty());
    }
}
