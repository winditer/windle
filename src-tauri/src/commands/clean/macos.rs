//! The macOS Deep Clean scan: caches, logs and leftovers under `~/Library`,
//! `/Library` and the system temp trees.
//!
//! The parent module owns the categories, the item plumbing and the commands;
//! the paths and the scan arms live here.

use std::path::{Path, PathBuf};

use super::super::{CleanOutcome, RiskLevel};
use super::{
    children_of, describe, item, list_children, older_than, risk_of, CleanCategoryId,
    CleanableItem, DOWNLOAD_AGE_DAYS, GROUP_APP_CACHES, GROUP_CRASH_REPORTS, GROUP_HTTP_STORAGES,
    GROUP_SANDBOX_CACHES, GROUP_SANDBOX_LOGS, GROUP_SAVED_STATE, GROUP_SYSTEM_CACHES,
    GROUP_SYSTEM_LOGS, GROUP_TEMP_FILES, GROUP_USER_LOGS, GROUP_WEBKIT, MIN_ITEM_SIZE,
    TEMP_AGE_DAYS,
};
use crate::scanner::walker::{self, CancelToken};
use crate::utils::{format, permissions};

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
pub(super) fn scan_category(id: CleanCategoryId, cancel: &CancelToken) -> Vec<CleanableItem> {
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
    let mut languages: Vec<String> = super::super::run_tool("defaults", &["read", "-g", "AppleLanguages"])
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

/// Rough junk total for the dashboard tile. Deliberately limited to the
/// biggest, cheapest-to-measure locations so the dashboard stays responsive.
///
/// Only `~/Library/Caches` and `~/Library/Logs` are measured — `~/.Trash` is
/// excluded because QuickClean now permanently deletes junk (instead of moving
/// it to Trash), so counting Trash would create a circular dependency.
///
/// The same permission and minimum-size filters as `scan_junk` are applied so
/// the estimate only reports junk that can actually be cleaned.
pub(super) fn quick_junk_estimate() -> u64 {
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

/// Empty `~/.Trash`. Items in the trash were already discarded once, so they go
/// for good rather than being trashed again.
pub(super) fn empty_trash(outcome: &mut CleanOutcome) -> crate::utils::Result<()> {
    let trash = permissions::home_dir().join(".Trash");

    if !trash.is_dir() {
        return Ok(());
    }

    let (freed, failures) = crate::utils::fs_ops::empty_directory(
        &trash,
        crate::utils::fs_ops::RemoveMode::Permanent,
    );

    outcome.freed_bytes = freed;
    outcome.removed_paths.push(trash.to_string_lossy().into_owned());

    for (path, error) in failures {
        outcome.fail(path.to_string_lossy().into_owned(), error.to_string());
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;


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
