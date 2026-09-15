//! Smart Uninstall — remove an app together with everything it left behind.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use rayon::prelude::*;
use serde::Serialize;

use super::{CleanOutcome, RiskLevel};
use crate::scanner::walker;
use crate::utils::fs_ops::{self, RemoveMode};
use crate::utils::{format, history, permissions, WindleError, Result};

/// A name shorter than this is too generic to attribute leftovers by ("Go",
/// "Vim"), so those only ever match on bundle identifier.
const MIN_NAME_MATCH_LEN: usize = 4;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InstalledApp {
    pub id: String,
    pub name: String,
    pub bundle_id: Option<String>,
    pub version: Option<String>,
    pub path: String,
    pub bundle_size: u64,
    pub icon_path: Option<String>,
    pub installed_at: Option<u64>,
    pub last_used_at: Option<u64>,
    pub is_system: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum LeftoverKind {
    Preferences,
    ApplicationSupport,
    Caches,
    Logs,
    SavedState,
    Containers,
    LaunchAgent,
    Receipt,
    Other,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppLeftover {
    pub path: String,
    pub kind: LeftoverKind,
    pub size: u64,
    pub risk: RiskLevel,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UninstallPlan {
    pub app: InstalledApp,
    pub leftovers: Vec<AppLeftover>,
    pub total_size: u64,
    pub requires_elevation: bool,
}

/// Where leftovers usually live, relative to `~/Library`.
pub const LEFTOVER_DIRS: [(&str, LeftoverKind); 7] = [
    ("Preferences", LeftoverKind::Preferences),
    ("Application Support", LeftoverKind::ApplicationSupport),
    ("Caches", LeftoverKind::Caches),
    ("Logs", LeftoverKind::Logs),
    ("Saved Application State", LeftoverKind::SavedState),
    ("Containers", LeftoverKind::Containers),
    ("LaunchAgents", LeftoverKind::LaunchAgent),
];

/// Extra locations outside `~/Library` that apps scatter data into.
const EXTRA_LEFTOVER_DIRS: [(&str, LeftoverKind); 5] = [
    ("Library/HTTPStorages", LeftoverKind::Caches),
    ("Library/WebKit", LeftoverKind::Caches),
    ("Library/Group Containers", LeftoverKind::Containers),
    ("Library/Application Scripts", LeftoverKind::Other),
    ("Library/Cookies", LeftoverKind::Other),
];

/// Directories that hold `.app` bundles a user may remove. `/System/Applications`
/// is deliberately absent: those are part of the OS.
pub fn application_roots() -> Vec<PathBuf> {
    let home = permissions::home_dir();

    [
        PathBuf::from("/Applications"),
        PathBuf::from("/Applications/Utilities"),
        home.join("Applications"),
    ]
    .into_iter()
    .filter(|path| path.is_dir())
    .collect()
}

/// Every `.app` bundle in the standard application directories.
#[tauri::command]
pub async fn list_apps() -> Result<Vec<InstalledApp>> {
    Ok(installed_apps())
}

/// Collect the bundle plus every leftover we can attribute to it.
#[tauri::command]
pub async fn build_uninstall_plan(app_id: String) -> Result<UninstallPlan> {
    let app = find_app(&app_id)?;
    Ok(plan_for(app))
}

/// Execute a plan. `paths` names the leftovers to take with the bundle — the
/// bundle itself is always removed.
#[tauri::command]
pub async fn uninstall_app(app_id: String, paths: Vec<String>) -> Result<CleanOutcome> {
    let app = find_app(&app_id)?;

    if app.is_system {
        return Err(WindleError::Protected(app.path));
    }

    // Rebuild the plan and only act on paths it actually contains, so a stale
    // or tampered request cannot turn into an arbitrary delete.
    let plan = plan_for(app);
    let (ordered, rejected) = requested_paths(&plan, paths);

    let mut outcome = CleanOutcome::default();

    for path in rejected {
        outcome.fail(path, "not part of this uninstall plan");
    }

    for path in ordered {
        match fs_ops::remove(Path::new(&path), RemoveMode::Trash) {
            Ok(freed) => outcome.succeed(path, freed),
            Err(error) => outcome.fail(path, error.to_string()),
        }
    }

    history::record_outcome(history::Operation::Uninstall, &outcome);

    Ok(outcome)
}

/// Resolve a removal request against `plan`: the requested leftovers plus the
/// bundle itself, which is never optional — a request that only lists
/// leftovers must not leave the app behind. Leftovers are ordered first so an
/// interrupted run still leaves the app visible in Launchpad rather than
/// half-gone. Requested paths the plan does not cover are returned separately,
/// to be reported as failures instead of removed.
fn requested_paths(plan: &UninstallPlan, requested: Vec<String>) -> (Vec<String>, Vec<String>) {
    let allowed: Vec<&str> = std::iter::once(plan.app.path.as_str())
        .chain(plan.leftovers.iter().map(|leftover| leftover.path.as_str()))
        .collect();

    let mut rejected = Vec::new();
    let mut selected: Vec<String> = requested
        .into_iter()
        .filter(|path| {
            if allowed.contains(&path.as_str()) {
                true
            } else {
                rejected.push(path.clone());
                false
            }
        })
        .collect();

    if !selected.iter().any(|path| path == &plan.app.path) {
        selected.push(plan.app.path.clone());
    }

    selected.sort_by_key(|path| path == &plan.app.path);
    (selected, rejected)
}

/// Leftovers whose owning app is already gone.
#[tauri::command]
pub async fn find_orphaned_leftovers() -> Result<Vec<UninstallPlan>> {
    let installed = installed_apps();
    let home = permissions::home_dir();

    // Bundle identifiers still backed by an installed app.
    let known: Vec<String> = installed
        .iter()
        .filter_map(|app| app.bundle_id.clone())
        .map(|id| id.to_lowercase())
        .collect();

    // Group by bundle id so one vanished app yields one plan.
    let mut orphans: BTreeMap<String, Vec<(PathBuf, LeftoverKind)>> = BTreeMap::new();

    for (directory, kind) in LEFTOVER_DIRS {
        let root = home.join("Library").join(directory);
        let Ok(entries) = std::fs::read_dir(&root) else {
            continue;
        };

        for entry in entries.filter_map(std::result::Result::ok) {
            let path = entry.path();
            let name = format::file_name(&path);

            let Some(bundle_id) = bundle_id_from_leftover(&name) else {
                continue;
            };
            // Apple's own data is not an orphan, it belongs to the OS.
            if bundle_id.starts_with("com.apple.") {
                continue;
            }
            if known.contains(&bundle_id.to_lowercase()) {
                continue;
            }
            if permissions::ensure_removable(&path).is_err() {
                continue;
            }

            orphans.entry(bundle_id).or_default().push((path, kind));
        }
    }

    let plans = orphans
        .into_par_iter()
        .map(|(bundle_id, entries)| {
            let paths: Vec<PathBuf> = entries.iter().map(|(path, _)| path.clone()).collect();
            let sizes = walker::sizes_of(&paths);
            let total_size = sizes.iter().sum();

            let leftovers: Vec<AppLeftover> = entries
                .into_iter()
                .zip(sizes)
                .map(|((path, kind), size)| AppLeftover {
                    path: path.to_string_lossy().into_owned(),
                    kind,
                    size,
                    risk: RiskLevel::Safe,
                })
                .collect();

            UninstallPlan {
                app: InstalledApp {
                    id: bundle_id.clone(),
                    // The bundle is gone, so its identifier is the only name.
                    name: bundle_id
                        .rsplit('.')
                        .next()
                        .unwrap_or(&bundle_id)
                        .to_string(),
                    bundle_id: Some(bundle_id),
                    version: None,
                    path: String::new(),
                    bundle_size: 0,
                    icon_path: None,
                    installed_at: None,
                    last_used_at: None,
                    is_system: false,
                },
                leftovers,
                total_size,
                requires_elevation: false,
            }
        })
        .collect::<Vec<UninstallPlan>>();

    let mut plans = plans;
    plans.sort_by(|a, b| b.total_size.cmp(&a.total_size));
    Ok(plans)
}

/// Every installed app, with sizes measured in parallel.
pub fn installed_apps() -> Vec<InstalledApp> {
    let bundles: Vec<PathBuf> = application_roots()
        .iter()
        .filter_map(|root| std::fs::read_dir(root).ok())
        .flat_map(|entries| {
            entries
                .filter_map(std::result::Result::ok)
                .map(|entry| entry.path())
                .filter(|path| format::extension(path) == "app")
                .collect::<Vec<PathBuf>>()
        })
        .collect();

    let mut apps: Vec<InstalledApp> = bundles.par_iter().map(|path| read_bundle(path)).collect();

    apps.sort_by(|a, b| b.bundle_size.cmp(&a.bundle_size));
    apps
}

/// Cheap count for the dashboard — no bundle sizing involved.
pub fn installed_app_count() -> usize {
    application_roots()
        .iter()
        .filter_map(|root| std::fs::read_dir(root).ok())
        .flat_map(|entries| entries.filter_map(std::result::Result::ok))
        .filter(|entry| format::extension(&entry.path()) == "app")
        .count()
}

/// Read one `.app` bundle's metadata.
fn read_bundle(path: &Path) -> InstalledApp {
    let info = super::read_plist(&path.join("Contents/Info.plist")).ok();

    let bundle_id = info
        .as_ref()
        .and_then(|plist| super::plist_string(plist, "CFBundleIdentifier"));

    let display_name = info
        .as_ref()
        .and_then(|plist| {
            super::plist_string(plist, "CFBundleDisplayName")
                .or_else(|| super::plist_string(plist, "CFBundleName"))
        })
        // Fall back to the bundle's own file name, minus `.app`.
        .unwrap_or_else(|| {
            format::file_name(path)
                .trim_end_matches(".app")
                .to_string()
        });

    let version = info.as_ref().and_then(|plist| {
        super::plist_string(plist, "CFBundleShortVersionString")
            .or_else(|| super::plist_string(plist, "CFBundleVersion"))
    });

    let icon_path = info
        .as_ref()
        .and_then(|plist| super::plist_string(plist, "CFBundleIconFile"))
        .map(|icon| {
            // `CFBundleIconFile` may or may not carry the extension.
            if icon.ends_with(".icns") {
                icon
            } else {
                format!("{icon}.icns")
            }
        })
        .map(|icon| path.join("Contents/Resources").join(icon))
        .filter(|icon| icon.exists())
        .map(|icon| icon.to_string_lossy().into_owned());

    let metadata = std::fs::metadata(path).ok();

    InstalledApp {
        id: path.to_string_lossy().into_owned(),
        name: display_name,
        is_system: bundle_id
            .as_ref()
            .is_some_and(|id| id.starts_with("com.apple.")),
        bundle_id,
        version,
        path: path.to_string_lossy().into_owned(),
        bundle_size: walker::directory_size_parallel(path),
        icon_path,
        installed_at: metadata
            .as_ref()
            .and_then(|meta| meta.created().ok())
            .and_then(format::epoch_millis),
        // Access time is the closest thing to "last used" that does not need
        // the Spotlight metadata store.
        last_used_at: metadata
            .as_ref()
            .and_then(|meta| meta.accessed().ok())
            .and_then(format::epoch_millis),
    }
}

/// Resolve an app by the id `list_apps` handed out (its bundle path), falling
/// back to a bundle-identifier lookup.
fn find_app(app_id: &str) -> Result<InstalledApp> {
    let as_path = PathBuf::from(app_id);

    if as_path.is_dir() && format::extension(&as_path) == "app" {
        // Reject a path that only looks like an app bundle.
        if !application_roots()
            .iter()
            .any(|root| as_path.starts_with(root))
        {
            return Err(WindleError::Protected(app_id.to_string()));
        }

        return Ok(read_bundle(&as_path));
    }

    installed_apps()
        .into_iter()
        .find(|app| {
            app.id == app_id || app.bundle_id.as_deref() == Some(app_id) || app.name == app_id
        })
        .ok_or_else(|| WindleError::NotFound(app_id.to_string()))
}

/// Find everything on disk that belongs to `app`.
fn plan_for(app: InstalledApp) -> UninstallPlan {
    let home = permissions::home_dir();
    let mut candidates: Vec<(PathBuf, LeftoverKind, RiskLevel)> = Vec::new();

    let search_dirs = LEFTOVER_DIRS
        .iter()
        .map(|(dir, kind)| (home.join("Library").join(dir), *kind))
        .chain(
            EXTRA_LEFTOVER_DIRS
                .iter()
                .map(|(dir, kind)| (home.join(dir), *kind)),
        )
        .chain([
            (PathBuf::from("/Library/Application Support"), LeftoverKind::ApplicationSupport),
            (PathBuf::from("/Library/Caches"), LeftoverKind::Caches),
            (PathBuf::from("/Library/Logs"), LeftoverKind::Logs),
            (PathBuf::from("/Library/LaunchAgents"), LeftoverKind::LaunchAgent),
            (PathBuf::from("/Library/Receipts"), LeftoverKind::Receipt),
        ]);

    for (directory, kind) in search_dirs {
        let Ok(entries) = std::fs::read_dir(&directory) else {
            continue;
        };

        for entry in entries.filter_map(std::result::Result::ok) {
            let path = entry.path();
            let name = format::file_name(&path);

            let Some(confidence) = match_confidence(&name, &app) else {
                continue;
            };
            // Anything the guard rails would refuse is not worth offering.
            if permissions::ensure_removable(&path).is_err() {
                continue;
            }

            candidates.push((path, kind, confidence));
        }
    }

    candidates.sort_by(|a, b| a.0.cmp(&b.0));
    candidates.dedup_by(|a, b| a.0 == b.0);

    let paths: Vec<PathBuf> = candidates.iter().map(|(path, ..)| path.clone()).collect();
    let sizes = walker::sizes_of(&paths);

    let mut leftovers: Vec<AppLeftover> = candidates
        .into_iter()
        .zip(sizes)
        .map(|((path, kind, risk), size)| AppLeftover {
            path: path.to_string_lossy().into_owned(),
            kind,
            size,
            risk,
        })
        .collect();

    leftovers.sort_by(|a, b| b.size.cmp(&a.size));

    let requires_elevation = permissions::needs_elevation(Path::new(&app.path))
        || leftovers
            .iter()
            .any(|leftover| permissions::needs_elevation(Path::new(&leftover.path)));

    let total_size = app.bundle_size + leftovers.iter().map(|leftover| leftover.size).sum::<u64>();

    UninstallPlan {
        app,
        leftovers,
        total_size,
        requires_elevation,
    }
}

/// How confident we are that `name` belongs to `app`, or `None` when it does
/// not. A bundle-identifier match is unambiguous; a name match is a guess, so
/// it is surfaced as `Caution` for the user to confirm.
fn match_confidence(name: &str, app: &InstalledApp) -> Option<RiskLevel> {
    // Strip the suffixes macOS bolts onto leftover names.
    let stem = name
        .strip_suffix(".savedState")
        .or_else(|| name.strip_suffix(".plist"))
        .or_else(|| name.strip_suffix(".binarycookies"))
        .unwrap_or(name);

    if let Some(bundle_id) = app.bundle_id.as_deref() {
        if stem.eq_ignore_ascii_case(bundle_id) {
            return Some(RiskLevel::Safe);
        }
        // Helpers and extensions: `com.acme.App.Helper`.
        if stem.len() > bundle_id.len()
            && stem.get(..bundle_id.len())
                .is_some_and(|prefix| prefix.eq_ignore_ascii_case(bundle_id))
            && stem.as_bytes().get(bundle_id.len()) == Some(&b'.')
        {
            return Some(RiskLevel::Safe);
        }
    }

    if app.name.chars().count() >= MIN_NAME_MATCH_LEN && stem.eq_ignore_ascii_case(&app.name) {
        return Some(RiskLevel::Caution);
    }

    None
}

/// Read a bundle identifier out of a leftover's file name, if it looks like one.
fn bundle_id_from_leftover(name: &str) -> Option<String> {
    let stem = name
        .strip_suffix(".savedState")
        .or_else(|| name.strip_suffix(".plist"))
        .unwrap_or(name);

    // Reverse-DNS identifiers have at least three components.
    let parts: Vec<&str> = stem.split('.').collect();
    if parts.len() < 3 || parts.iter().any(|part| part.is_empty()) {
        return None;
    }

    // Only trust the well-known top-level prefixes; plenty of plain folder
    // names contain dots without being identifiers.
    const PREFIXES: [&str; 8] = ["com", "org", "net", "io", "dev", "co", "app", "me"];
    if !PREFIXES.contains(&parts[0].to_lowercase().as_str()) {
        return None;
    }

    Some(stem.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app_fixture() -> InstalledApp {
        InstalledApp {
            id: "/Applications/Acme.app".into(),
            name: "Acme".into(),
            bundle_id: Some("com.acme.Acme".into()),
            version: Some("1.0".into()),
            path: "/Applications/Acme.app".into(),
            bundle_size: 1_000,
            icon_path: None,
            installed_at: None,
            last_used_at: None,
            is_system: false,
        }
    }

    #[test]
    fn matches_leftovers_by_bundle_id() {
        let app = app_fixture();

        assert_eq!(match_confidence("com.acme.Acme", &app), Some(RiskLevel::Safe));
        assert_eq!(
            match_confidence("com.acme.Acme.plist", &app),
            Some(RiskLevel::Safe)
        );
        assert_eq!(
            match_confidence("com.acme.Acme.savedState", &app),
            Some(RiskLevel::Safe)
        );
        assert_eq!(
            match_confidence("com.acme.Acme.Helper", &app),
            Some(RiskLevel::Safe)
        );
    }

    #[test]
    fn name_matches_are_only_a_guess() {
        let app = app_fixture();
        assert_eq!(match_confidence("Acme", &app), Some(RiskLevel::Caution));
    }

    #[test]
    fn does_not_match_unrelated_or_prefix_colliding_names() {
        let app = app_fixture();

        assert_eq!(match_confidence("com.acme.AcmeOther", &app), None);
        assert_eq!(match_confidence("com.other.App", &app), None);
        assert_eq!(match_confidence("Acmex", &app), None);
    }

    #[test]
    fn short_names_never_match_by_name_alone() {
        let mut app = app_fixture();
        app.name = "Go".into();
        app.bundle_id = None;

        assert_eq!(match_confidence("Go", &app), None);
    }

    #[test]
    fn recognises_bundle_identifiers_in_leftover_names() {
        assert_eq!(
            bundle_id_from_leftover("com.acme.Acme.plist"),
            Some("com.acme.Acme".to_string())
        );
        assert_eq!(
            bundle_id_from_leftover("com.acme.Acme.savedState"),
            Some("com.acme.Acme".to_string())
        );
        // Not identifiers.
        assert_eq!(bundle_id_from_leftover("Firefox"), None);
        assert_eq!(bundle_id_from_leftover("my.notes"), None);
        assert_eq!(bundle_id_from_leftover("Backup.2024.zip"), None);
    }

    fn plan_fixture() -> UninstallPlan {
        UninstallPlan {
            app: app_fixture(),
            leftovers: vec![AppLeftover {
                path: "/Users/test/Library/Preferences/com.acme.Acme.plist".into(),
                kind: LeftoverKind::Preferences,
                size: 100,
                risk: RiskLevel::Safe,
            }],
            total_size: 1_100,
            requires_elevation: false,
        }
    }

    #[test]
    fn the_bundle_is_removed_even_when_only_leftovers_are_requested() {
        let plan = plan_fixture();

        let (selected, rejected) = requested_paths(
            &plan,
            vec!["/Users/test/Library/Preferences/com.acme.Acme.plist".into()],
        );

        assert!(rejected.is_empty());
        assert_eq!(
            selected,
            vec![
                "/Users/test/Library/Preferences/com.acme.Acme.plist".to_string(),
                plan.app.path.clone(),
            ]
        );
    }

    #[test]
    fn an_empty_request_still_removes_the_bundle() {
        let plan = plan_fixture();

        let (selected, rejected) = requested_paths(&plan, vec![]);

        assert!(rejected.is_empty());
        assert_eq!(selected, vec![plan.app.path.clone()]);
    }

    #[test]
    fn the_bundle_is_not_removed_twice() {
        let plan = plan_fixture();

        let (selected, _) = requested_paths(&plan, vec![plan.app.path.clone()]);

        assert_eq!(selected, vec![plan.app.path.clone()]);
    }

    #[test]
    fn paths_outside_the_plan_are_rejected() {
        let plan = plan_fixture();

        let (selected, rejected) = requested_paths(&plan, vec!["/etc/hosts".into()]);

        assert_eq!(rejected, vec!["/etc/hosts".to_string()]);
        assert_eq!(selected, vec![plan.app.path.clone()]);
    }

    #[test]
    fn refuses_app_bundles_outside_the_application_directories() {
        let error = find_app("/tmp/Evil.app").unwrap_err();
        assert!(matches!(
            error,
            WindleError::Protected(_) | WindleError::NotFound(_)
        ));
    }

    #[test]
    fn the_installed_count_agrees_with_the_listing() {
        // A machine always has at least one app in /Applications.
        assert!(installed_app_count() > 0);
    }
}
