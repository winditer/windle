//! Windows: a program is a registration under the `Uninstall` registry keys,
//! and the only thing that can take it away is its own uninstaller —
//! `%ProgramFiles%` is protected ground that Windle never deletes from by path.
//!
//! An uninstall therefore has two halves: run the vendor's uninstaller (or, for
//! a registration whose uninstaller is already gone, drop the registration
//! itself), then clean up what it leaves behind — app-data folders, Start-menu
//! shortcuts and `Software` registry keys. Because the uninstaller owns the
//! actual removal, a leftover that has already disappeared by the time we get
//! to it counts as removed: the goal was for it to be gone.

use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use rayon::prelude::*;
use winreg::enums::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_ALL_ACCESS, KEY_READ};
use winreg::RegKey;

use super::super::{CleanOutcome, RiskLevel};
use super::{AppLeftover, InstalledApp, LeftoverKind, UninstallPlan, MIN_NAME_MATCH_LEN};
use crate::scanner::walker;
use crate::utils::fs_ops::{self, RemoveMode};
use crate::utils::{format, permissions, platform, Result, WindleError};

/// Which registry hive an entry lives in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Hive {
    User,
    Machine,
}

impl Hive {
    fn hive(self) -> RegKey {
        RegKey::predef(match self {
            Self::User => HKEY_CURRENT_USER,
            Self::Machine => HKEY_LOCAL_MACHINE,
        })
    }

    /// The name ids use, spelled the way Windows tools spell the hives.
    fn prefix(self) -> &'static str {
        match self {
            Self::User => "HKCU",
            Self::Machine => "HKLM",
        }
    }
}

/// The registry keys that hold the installed-programs list: both hives, and
/// both views of the machine hive — 32-bit installers register under
/// `WOW6432Node`.
const UNINSTALL_ROOTS: [(&str, Hive); 4] = [
    (
        r"SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall",
        Hive::User,
    ),
    (
        r"SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall",
        Hive::Machine,
    ),
    (
        r"SOFTWARE\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall",
        Hive::Machine,
    ),
    (
        r"SOFTWARE\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall",
        Hive::User,
    ),
];

/// Windows-owned folders under the app-data roots. They are never an app's
/// leftover — `Programs` holds per-user installs, `Temp` is the user's scratch
/// space — and are not stepped into even when a publisher name matches.
const APP_DATA_OWNED: [&str; 6] = [
    "Comms",
    "ConnectedDevicesPlatform",
    "Microsoft",
    "Packages",
    "Programs",
    "Temp",
];

/// Branches of `Software` that belong to the OS. They are never offered as an
/// app's settings, and never stepped through on the way to one.
const SOFTWARE_OWNED: [&str; 6] = [
    "Classes",
    "Clients",
    "Microsoft",
    "Policies",
    "RegisteredApplications",
    "WOW6432Node",
];

/// `SW_SHOWNORMAL`: an uninstaller that draws a window gets to draw it.
const SW_SHOWNORMAL: i32 = 1;

/// `HRESULT_FROM_WIN32(ERROR_CANCELLED)`, which is what the shell reports when
/// a UAC prompt raised by the uninstaller is dismissed.
const SHELL_CANCELLED: u32 = 0x8007_04C7;

/// Shell failures that mean the file we were told to run is not there.
fn is_missing_code(code: u32) -> bool {
    // `SE_ERR_FNF` / `SE_ERR_PNF` are the classic ShellExecute codes; the same
    // conditions surface as `HRESULT_FROM_WIN32` through ShellExecuteExW.
    matches!(code, 2 | 3 | 0x8007_0002 | 0x8007_0003)
}

/// One installed program, read from its `Uninstall` key.
struct Entry {
    hive: Hive,
    key_path: String,
    display_name: String,
    display_version: Option<String>,
    publisher: Option<String>,
    install_location: Option<PathBuf>,
    uninstall: Option<String>,
    quiet_uninstall: Option<String>,
    estimated_kb: Option<u64>,
    install_date: Option<String>,
    last_write: Option<u64>,
    no_remove: bool,
}

impl Entry {
    /// Read one registration, or `None` when it does not describe a program the
    /// user should see: entries without a name, components of another product
    /// and updates are all hidden from "Apps & features" by Windows itself.
    fn read(hive: Hive, key_path: &str, key: &RegKey) -> Option<Entry> {
        let display_name = text(key, "DisplayName")?;

        if dword(key, "SystemComponent").is_some_and(|value| value != 0) {
            return None;
        }
        // A child registration ("Mozilla Maintenance Service") is removed with
        // its parent, and updates are not programs of their own.
        if text(key, "ParentKeyName").is_some() || text(key, "ReleaseType").is_some() {
            return None;
        }

        Some(Entry {
            hive,
            key_path: key_path.to_string(),
            display_name,
            display_version: text(key, "DisplayVersion"),
            publisher: text(key, "Publisher"),
            install_location: path_value(key, "InstallLocation"),
            uninstall: text(key, "UninstallString"),
            quiet_uninstall: text(key, "QuietUninstallString"),
            estimated_kb: dword(key, "EstimatedSize").map(u64::from),
            install_date: text(key, "InstallDate"),
            last_write: last_write(key),
            no_remove: dword(key, "NoRemove").is_some_and(|value| value != 0),
        })
    }

    /// The id the frontend addresses this entry by: `HKCU\…\Uninstall\App`.
    fn id(&self) -> String {
        registry_path(self.hive, &self.key_path)
    }

    /// What the UI shows as "where the program is": the install folder when the
    /// entry names one, the registry key otherwise.
    fn display_path(&self) -> String {
        self.install_location
            .as_ref()
            .map(|location| location.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.id())
    }

    /// The uninstall command to run, preferring the quiet form: with one it
    /// uninstalls without a wizard, without one the user walks through it.
    fn command(&self) -> Option<&str> {
        self.quiet_uninstall
            .as_deref()
            .or(self.uninstall.as_deref())
            .map(str::trim)
            .filter(|command| !command.is_empty())
    }

    /// A registration whose uninstaller no longer exists: the program is gone
    /// but the entry still sits in Settings → Apps.
    fn is_ghost(&self) -> bool {
        self.command().and_then(uninstaller_presence) == Some(false)
    }

    /// Disk usage: the install folder measured, or the installer's own estimate
    /// when there is no folder to walk.
    fn size(&self) -> u64 {
        if let Some(location) = &self.install_location {
            if location.is_dir() {
                return walker::directory_size_parallel(location);
            }
        }

        self.estimated_kb.unwrap_or(0).saturating_mul(1024)
    }

    fn to_app(&self) -> InstalledApp {
        InstalledApp {
            id: self.id(),
            name: self.display_name.clone(),
            bundle_id: None,
            version: self.display_version.clone(),
            path: self.display_path(),
            bundle_size: self.size(),
            icon_path: None,
            installed_at: self.install_date.as_deref().and_then(install_date_millis),
            // Windows records no per-app launch time. The uninstall key's last
            // write is the closest signal, and moves whenever the app updates
            // or rewrites its registration — which is when it was last seen.
            last_used_at: self.last_write,
            is_system: self.no_remove,
        }
    }
}

/// Every installed program, with sizes measured in parallel.
pub(super) fn installed_apps() -> Vec<InstalledApp> {
    let entries = read_entries();
    let mut apps: Vec<InstalledApp> = entries.par_iter().map(Entry::to_app).collect();

    apps.sort_by(|a, b| b.bundle_size.cmp(&a.bundle_size));
    apps
}

/// Cheap count for the dashboard — no sizing involved.
pub(super) fn installed_app_count() -> usize {
    read_entries().len()
}

/// Lowercased registry display names, for the installer scan. A name like
/// `Docker Desktop 4.35.1` still matches an installer stem of `docker desktop`;
/// vendor decorations such as a parenthesised architecture do not.
pub(super) fn installed_names() -> Vec<String> {
    read_entries()
        .into_iter()
        .map(|entry| entry.display_name.to_lowercase())
        .collect()
}

/// Resolve an app by the id `list_apps` handed out (its registry id), falling
/// back to a display-name lookup.
pub(super) fn find_app(app_id: &str) -> Result<InstalledApp> {
    if let Some(entry) = read_entry(app_id) {
        return Ok(entry.to_app());
    }

    read_entries()
        .into_iter()
        .find(|entry| entry.display_name.eq_ignore_ascii_case(app_id))
        .map(|entry| entry.to_app())
        .ok_or_else(|| WindleError::NotFound(app_id.to_string()))
}

/// Find everything the program left behind.
pub(super) fn plan_for(app: InstalledApp) -> UninstallPlan {
    match read_entry(&app.id) {
        Some(entry) => plan_entry(&entry),
        // The registration is gone from under us — a concurrent uninstall — so
        // plan from what the app record itself says.
        None => plan_with(&app, None, None, false),
    }
}

/// Leftovers whose app is already gone but whose registration still lingers.
pub(super) fn find_orphaned_leftovers() -> Vec<UninstallPlan> {
    let mut plans: Vec<UninstallPlan> = read_entries()
        .into_par_iter()
        .filter(Entry::is_ghost)
        .map(|entry| plan_entry(&entry))
        .collect();

    plans.sort_by(|a, b| b.total_size.cmp(&a.total_size));
    plans
}

/// Run the app's own uninstaller, then clean up what it left behind.
///
/// The app goes first here — the opposite of macOS, where the bundle is the
/// last thing trashed: on Windows the uninstaller is what removes the program,
/// and it usually deletes some of its own data along the way, so leftovers are
/// only gathered after it is done.
pub(super) fn remove_selected(
    plan: &UninstallPlan,
    ordered: &[String],
    outcome: &mut CleanOutcome,
) {
    if ordered.iter().any(|path| path == &plan.app.path) {
        remove_app(plan, outcome);
    }

    for path in ordered.iter().filter(|path| *path != &plan.app.path) {
        remove_leftover(path, outcome);
    }
}

// ---------------------------------------------------------------------------
// Reading the registry
// ---------------------------------------------------------------------------

fn read_entries() -> Vec<Entry> {
    let mut entries = Vec::new();

    for (root, hive) in UNINSTALL_ROOTS {
        // A missing root is normal: the 32-bit view of a hive does not exist
        // everywhere.
        let Ok(key) = hive.hive().open_subkey_with_flags(root, KEY_READ) else {
            continue;
        };

        for name in key.enum_keys().filter_map(std::result::Result::ok) {
            if name.is_empty() {
                continue;
            }

            let Ok(child) = key.open_subkey_with_flags(&name, KEY_READ) else {
                continue;
            };
            let key_path = format!("{root}\\{name}");

            if let Some(entry) = Entry::read(hive, &key_path, &child) {
                entries.push(entry);
            }
        }
    }

    entries
}

/// Read the registration an id addresses, refusing ids that do not sit in one
/// of the Uninstall keys — the same guard as the run-key lookup, so a forged id
/// cannot read an arbitrary part of the registry.
fn read_entry(id: &str) -> Option<Entry> {
    let (hive, key_path) = split_id(id)?;
    if !is_uninstall_key(hive, key_path) {
        return None;
    }

    let key = hive
        .hive()
        .open_subkey_with_flags(key_path, KEY_READ)
        .ok()?;
    Entry::read(hive, key_path, &key)
}

/// Split an id into its hive and the key path inside it.
fn split_id(id: &str) -> Option<(Hive, &str)> {
    let (prefix, key_path) = id.split_once('\\')?;

    let hive = match prefix {
        "HKCU" => Hive::User,
        "HKLM" => Hive::Machine,
        _ => return None,
    };

    if key_path.is_empty() {
        return None;
    }

    Some((hive, key_path))
}

/// Whether a key path is a registration — one of the Uninstall keys or a
/// subkey of one, never the container itself.
fn is_uninstall_key(hive: Hive, key_path: &str) -> bool {
    let path = Path::new(key_path);

    UNINSTALL_ROOTS.iter().any(|(root, root_hive)| {
        *root_hive == hive
            && platform::starts_with(path, Path::new(root))
            && !platform::eq(path, Path::new(root))
    })
}

/// The id-shaped path of a registry key.
fn registry_path(hive: Hive, key_path: &str) -> String {
    format!("{}\\{key_path}", hive.prefix())
}

fn text(key: &RegKey, name: &str) -> Option<String> {
    key.get_value::<String, _>(name)
        .ok()
        .filter(|value| !value.trim().is_empty())
}

fn dword(key: &RegKey, name: &str) -> Option<u32> {
    key.get_value::<u32, _>(name).ok()
}

/// A path-valued REG_SZ, with the quotes installers like to wrap it in removed.
fn path_value(key: &RegKey, name: &str) -> Option<PathBuf> {
    text(key, name).map(|value| PathBuf::from(value.trim_matches('"')))
}

/// When the registration was last written, as milliseconds since the epoch.
fn last_write(key: &RegKey) -> Option<u64> {
    let info = key.query_info().ok()?;
    let time = info.get_last_write_time_system();

    civil_millis(
        i64::from(time.wYear),
        u32::from(time.wMonth),
        u32::from(time.wDay),
        u32::from(time.wHour),
        u32::from(time.wMinute),
        u32::from(time.wSecond),
    )
}

/// `InstallDate` is `YYYYMMDD` in UTC, when it is written at all.
fn install_date_millis(value: &str) -> Option<u64> {
    if value.len() != 8 || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }

    civil_millis(
        value[0..4].parse().ok()?,
        value[4..6].parse().ok()?,
        value[6..8].parse().ok()?,
        0,
        0,
        0,
    )
}

/// Milliseconds since the Unix epoch for a UTC civil date, or `None` when the
/// fields are not a real date. Days-from-civil counting, so no calendar tables.
fn civil_millis(
    year: i64,
    month: u32,
    day: u32,
    hour: u32,
    minute: u32,
    second: u32,
) -> Option<u64> {
    if !(1..=12).contains(&month)
        || day == 0
        || day > days_in_month(year, month)
        || hour > 23
        || minute > 59
        || second > 59
    {
        return None;
    }

    let year = if month <= 2 { year - 1 } else { year };
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400;
    // March is month 0, which makes the leap day the last day of the year.
    let month_index = i64::from((month + 9) % 12);
    let day_of_year = (153 * month_index + 2) / 5 + i64::from(day) - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    let days = era * 146_097 + day_of_era - 719_468;

    let millis = days * 86_400_000
        + i64::from(hour) * 3_600_000
        + i64::from(minute) * 60_000
        + i64::from(second) * 1_000;

    u64::try_from(millis).ok()
}

fn days_in_month(year: i64, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if year % 4 == 0 && (year % 100 != 0 || year % 400 == 0) => 29,
        2 => 28,
        _ => 0,
    }
}

// ---------------------------------------------------------------------------
// Planning the leftovers
// ---------------------------------------------------------------------------

/// One leftover we may offer, before its size is measured.
struct Candidate {
    path: String,
    kind: LeftoverKind,
    risk: RiskLevel,
    /// Registry keys are removed with the registry API, so the filesystem
    /// guard rails do not apply to them.
    registry: bool,
}

/// Where apps scatter data, and what the folders are called on the wire.
const DATA_ROOTS: [(&str, LeftoverKind); 3] = [
    ("%APPDATA%", LeftoverKind::ApplicationSupport),
    ("%LOCALAPPDATA%", LeftoverKind::Caches),
    ("%PROGRAMDATA%", LeftoverKind::ApplicationSupport),
];

fn plan_entry(entry: &Entry) -> UninstallPlan {
    let app = entry.to_app();

    plan_with(
        &app,
        entry.publisher.as_deref(),
        entry.install_location.as_deref(),
        entry.hive == Hive::Machine,
    )
}

/// Collect the leftovers a program of this name and publisher would have
/// scattered around, then weigh what survives the guard rails.
fn plan_with(
    app: &InstalledApp,
    publisher: Option<&str>,
    install: Option<&Path>,
    machine: bool,
) -> UninstallPlan {
    let mut candidates: Vec<Candidate> = Vec::new();

    for (root, kind) in DATA_ROOTS {
        if let Some(root) = platform::env_path(root) {
            collect_data_dirs(&root, kind, app, publisher, &mut candidates);
        }
    }

    for root in shortcut_roots() {
        collect_shortcuts(&root, app, &mut candidates);
    }

    collect_settings_keys(app, publisher, &mut candidates);
    collapse(&mut candidates, install);

    let paths: Vec<PathBuf> = candidates
        .iter()
        .map(|candidate| PathBuf::from(&candidate.path))
        .collect();
    let sizes = walker::sizes_of(&paths);

    let mut leftovers: Vec<AppLeftover> = candidates
        .into_iter()
        .zip(sizes)
        .map(|(candidate, size)| AppLeftover {
            path: candidate.path,
            kind: candidate.kind,
            // A registry key occupies no disk worth counting.
            size: if candidate.registry { 0 } else { size },
            risk: candidate.risk,
        })
        .collect();

    leftovers.sort_by(|a, b| b.size.cmp(&a.size));

    let requires_elevation = machine
        || leftovers.iter().any(|leftover| {
            split_id(&leftover.path).map_or_else(
                // Filesystem leftovers are checked against the machine-wide
                // roots; registry leftovers by the hive they live in.
                || permissions::needs_elevation(Path::new(&leftover.path)),
                |(hive, _)| hive == Hive::Machine,
            )
        });

    let total_size = app.bundle_size + leftovers.iter().map(|leftover| leftover.size).sum::<u64>();

    UninstallPlan {
        app: app.clone(),
        leftovers,
        total_size,
        requires_elevation,
    }
}

/// Drop candidates the guard rails would refuse, anything inside the install
/// location — those files are the program, not leftovers — and duplicates.
fn collapse(candidates: &mut Vec<Candidate>, install: Option<&Path>) {
    candidates.retain(|candidate| {
        if candidate.registry {
            return true;
        }

        let path = Path::new(&candidate.path);
        !overlaps_install(path, install) && permissions::ensure_removable(path).is_ok()
    });

    candidates.sort_by_key(|candidate| candidate.path.to_lowercase());
    candidates.dedup_by(|a, b| a.path.eq_ignore_ascii_case(&b.path));
}

/// Whether a candidate and the install location cover each other, in either
/// direction. An app that installs into its own app-data folder must not have
/// that folder offered as a "leftover".
fn overlaps_install(path: &Path, install: Option<&Path>) -> bool {
    install.is_some_and(|install| {
        platform::starts_with(path, install) || platform::starts_with(install, path)
    })
}

/// Look for `App` and, when the publisher is known, `Publisher\App` folders
/// under an app-data root.
fn collect_data_dirs(
    root: &Path,
    kind: LeftoverKind,
    app: &InstalledApp,
    publisher: Option<&str>,
    candidates: &mut Vec<Candidate>,
) {
    for path in read_dir_paths(root) {
        if !path.is_dir() {
            continue;
        }

        let name = format::file_name(&path);
        if matches_any(&name, &APP_DATA_OWNED) {
            continue;
        }

        if name_matches_app(&name, app) {
            candidates.push(Candidate {
                path: path.to_string_lossy().into_owned(),
                kind,
                risk: RiskLevel::Caution,
                registry: false,
            });
            continue;
        }

        // A folder named after the publisher: the app's own folder sits one
        // level in. Two registry fields agreeing makes it a strong match.
        if publisher.is_some_and(|publisher| name.eq_ignore_ascii_case(publisher)) {
            collect_publisher_dirs(&path, kind, app, candidates);
        }
    }
}

fn collect_publisher_dirs(
    dir: &Path,
    kind: LeftoverKind,
    app: &InstalledApp,
    candidates: &mut Vec<Candidate>,
) {
    for path in read_dir_paths(dir) {
        if !path.is_dir() {
            continue;
        }

        let name = format::file_name(&path);
        if matches_any(&name, &APP_DATA_OWNED) || !name_matches_app(&name, app) {
            continue;
        }

        candidates.push(Candidate {
            path: path.to_string_lossy().into_owned(),
            kind,
            risk: RiskLevel::Safe,
            registry: false,
        });
    }
}

/// The places an app's shortcuts live: the Start menus of both hives, and the
/// two desktop folders.
fn shortcut_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();

    for variable in ["APPDATA", "ProgramData"] {
        if let Some(base) = platform::env_path(variable) {
            roots.push(base.join(r"Microsoft\Windows\Start Menu\Programs"));
        }
    }
    if let Some(public) = platform::env_path("PUBLIC") {
        roots.push(public.join("Desktop"));
    }
    roots.push(permissions::home_dir().join("Desktop"));

    roots
}

/// Shortcuts sit directly in a root or inside one folder named after the
/// vendor, which is all the depth Explorer creates on its own.
fn collect_shortcuts(root: &Path, app: &InstalledApp, candidates: &mut Vec<Candidate>) {
    let dirs = std::iter::once(root.to_path_buf()).chain(
        read_dir_paths(root)
            .into_iter()
            .filter(|path| path.is_dir()),
    );

    for dir in dirs {
        for path in read_dir_paths(&dir) {
            if !path.is_file() || !matches_shortcut(&format::file_name(&path), app) {
                continue;
            }

            candidates.push(Candidate {
                path: path.to_string_lossy().into_owned(),
                kind: LeftoverKind::Other,
                risk: RiskLevel::Caution,
                registry: false,
            });
        }
    }
}

/// The settings an app keeps for itself: `Software\App` or, under a publisher
/// folder, `Software\Publisher\App` in either hive — what the uninstaller
/// usually leaves behind when its own cleanup falls short.
fn collect_settings_keys(
    app: &InstalledApp,
    publisher: Option<&str>,
    candidates: &mut Vec<Candidate>,
) {
    for hive in [Hive::User, Hive::Machine] {
        let Ok(software) = hive.hive().open_subkey_with_flags("Software", KEY_READ) else {
            continue;
        };

        for name in software.enum_keys().filter_map(std::result::Result::ok) {
            if name.is_empty() || matches_any(&name, &SOFTWARE_OWNED) {
                continue;
            }

            if name_matches_app(&name, app) {
                push_settings_key(candidates, hive, &name, RiskLevel::Caution);
                continue;
            }

            if !publisher.is_some_and(|publisher| name.eq_ignore_ascii_case(publisher)) {
                continue;
            }

            let Ok(vendor) = software.open_subkey_with_flags(&name, KEY_READ) else {
                continue;
            };

            for child in vendor.enum_keys().filter_map(std::result::Result::ok) {
                if child.is_empty() || matches_any(&child, &SOFTWARE_OWNED) {
                    continue;
                }
                if name_matches_app(&child, app) {
                    let key_path = format!("{name}\\{child}");
                    push_settings_key(candidates, hive, &key_path, RiskLevel::Safe);
                }
            }
        }
    }
}

fn push_settings_key(candidates: &mut Vec<Candidate>, hive: Hive, key_path: &str, risk: RiskLevel) {
    candidates.push(Candidate {
        path: registry_path(hive, &format!("Software\\{key_path}")),
        kind: LeftoverKind::Preferences,
        risk,
        registry: true,
    });
}

/// A folder name is only the app's own when it matches exactly and is long
/// enough to attribute — "Go" and "Vim" would otherwise catch unrelated data.
fn name_matches_app(name: &str, app: &InstalledApp) -> bool {
    let wanted = app.name.trim();

    wanted.chars().count() >= MIN_NAME_MATCH_LEN && name.eq_ignore_ascii_case(wanted)
}

/// Whether a shortcut's file name is the app's, ignoring the `.lnk` suffix and
/// the " - Shortcut" Explorer appends to copies.
fn matches_shortcut(name: &str, app: &InstalledApp) -> bool {
    let Some(stem) = name.strip_suffix(".lnk") else {
        return false;
    };
    let stem = stem.strip_suffix(" - Shortcut").unwrap_or(stem);

    name_matches_app(stem, app)
}

fn matches_any(name: &str, names: &[&str]) -> bool {
    names
        .iter()
        .any(|candidate| name.eq_ignore_ascii_case(candidate))
}

/// Every path in `dir`, or nothing when it cannot be read.
fn read_dir_paths(dir: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .filter_map(std::result::Result::ok)
        .map(|entry| entry.path())
        .collect()
}

// ---------------------------------------------------------------------------
// Removal
// ---------------------------------------------------------------------------

/// Take the program away: run its own uninstaller when it has one, and drop its
/// registration when the uninstaller is already gone.
fn remove_app(plan: &UninstallPlan, outcome: &mut CleanOutcome) {
    let path = plan.app.path.clone();

    let Some(entry) = read_entry(&plan.app.id) else {
        // The registration is already gone — there is nothing left to do.
        outcome.succeed(path, 0);
        return;
    };

    let Some(command) = entry.command() else {
        // A registration without an uninstaller: the entry itself is the only
        // thing left to remove.
        remove_registration(&entry, &path, outcome);
        return;
    };

    match run_uninstaller(command) {
        Ok(()) => {
            // Freed is the difference the uninstaller made to its own folder;
            // leftovers are counted separately as they are removed.
            let after = entry
                .install_location
                .as_deref()
                .filter(|location| location.is_dir())
                .map_or(0, walker::directory_size_parallel);
            outcome.succeed(path, plan.app.bundle_size.saturating_sub(after));
        }
        Err(UninstallError::Missing) => remove_registration(&entry, &path, outcome),
        Err(UninstallError::Cancelled) => {
            outcome.fail(path, "administrator privileges were not granted")
        }
        Err(UninstallError::Failed(message)) => outcome.fail(path, message),
    }
}

/// Delete the leftover the plan offered — a filesystem path, or a registry key.
fn remove_leftover(path: &str, outcome: &mut CleanOutcome) {
    if split_id(path).is_some() {
        remove_settings_key(path, outcome);
    } else {
        remove_path(path, outcome);
    }
}

/// Trash a filesystem leftover. A path that is already gone counts as removed:
/// the uninstaller has usually deleted its own data by the time we get here,
/// and the goal — the data being gone — has been achieved either way.
fn remove_path(path: &str, outcome: &mut CleanOutcome) {
    match fs_ops::remove(Path::new(path), RemoveMode::Trash) {
        Ok(freed) => outcome.succeed(path, freed),
        Err(WindleError::NotFound(_)) => outcome.succeed(path, 0),
        Err(error) => outcome.fail(path, error.to_string()),
    }
}

/// Remove one of the registry leftovers the plan offered. Validated again here,
/// component by component, so a path that somehow escaped the plan still cannot
/// reach a branch of `Software` the OS owns.
fn remove_settings_key(path: &str, outcome: &mut CleanOutcome) {
    let Some((hive, key_path)) = split_id(path) else {
        outcome.fail(path, "not a registry path");
        return;
    };
    if settings_key_parts(key_path).is_none() {
        outcome.fail(path, "not an application settings key");
        return;
    }

    if hive == Hive::Machine && !permissions::is_root() {
        outcome.fail(
            path,
            "administrator privileges are required to change machine-wide settings",
        );
        return;
    }

    match delete_tree(hive, key_path) {
        Ok(()) => outcome.succeed(path, 0),
        Err(error) => outcome.fail(path, error.to_string()),
    }
}

/// Split a settings path into its components, refusing anything that is not a
/// settings subtree: it must sit below `Software` and pass through none of the
/// OS-owned branches, at any depth.
fn settings_key_parts(key_path: &str) -> Option<Vec<String>> {
    let parts: Vec<String> = key_path.split('\\').map(str::to_string).collect();

    if parts.len() < 2
        || !parts[0].eq_ignore_ascii_case("Software")
        || parts
            .iter()
            .any(|part| part.is_empty() || matches_any(part, &SOFTWARE_OWNED))
    {
        return None;
    }

    Some(parts)
}

/// Delete a registration whose uninstaller is gone, which is what makes a ghost
/// entry disappear from Settings → Apps.
fn remove_registration(entry: &Entry, path: &str, outcome: &mut CleanOutcome) {
    if entry.hive == Hive::Machine && !permissions::is_root() {
        outcome.fail(
            path,
            "administrator privileges are required to change machine-wide registrations",
        );
        return;
    }

    match delete_tree(entry.hive, &entry.key_path) {
        Ok(()) => outcome.succeed(path, 0),
        Err(error) => outcome.fail(path, error.to_string()),
    }
}

/// Delete a registry key and its subkeys. A key that is already gone is not an
/// error: the goal is for it to be gone.
fn delete_tree(hive: Hive, key_path: &str) -> Result<()> {
    let Some((parent_path, name)) = key_path.rsplit_once('\\') else {
        return Err(WindleError::Protected(key_path.to_string()));
    };

    // `RegDeleteTree` needs write access to the parent, which for the machine
    // hive by itself requires the administrator token checked by callers.
    let parent = hive
        .hive()
        .open_subkey_with_flags(parent_path, KEY_ALL_ACCESS)
        .map_err(registry_error(key_path))?;

    match parent.delete_subkey_all(name) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(registry_error(key_path)(error)),
    }
}

/// Why running an uninstaller did not work.
enum UninstallError {
    /// The command names a file that is not there — a ghost registration.
    Missing,
    /// A UAC prompt raised by the uninstaller was dismissed.
    Cancelled,
    /// The uninstaller could not be started for some other reason.
    Failed(String),
}

/// Run a vendor uninstaller through the shell and wait for it to finish, so the
/// leftover pass does not race a program still deleting its own files.
fn run_uninstaller(command: &str) -> std::result::Result<(), UninstallError> {
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::{WaitForSingleObject, INFINITE};
    use windows::Win32::UI::Shell::{
        ShellExecuteExW, SEE_MASK_FLAG_NO_UI, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW,
    };

    let (file, parameters) = split_command(command);

    let file: Vec<u16> = OsStr::new(&file)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let parameters: Vec<u16> = parameters
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    let verb: Vec<u16> = "open".encode_utf16().chain(std::iter::once(0)).collect();

    let mut info = SHELLEXECUTEINFOW {
        cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
        // No error dialog: the batch's failures are reported through the
        // outcome, and a modal box would sit behind the app window.
        fMask: SEE_MASK_NOCLOSEPROCESS | SEE_MASK_FLAG_NO_UI,
        lpVerb: PCWSTR(verb.as_ptr()),
        lpFile: PCWSTR(file.as_ptr()),
        lpParameters: PCWSTR(parameters.as_ptr()),
        nShow: SW_SHOWNORMAL,
        ..Default::default()
    };

    // SAFETY: every pointer in `info` refers to a buffer that outlives the
    // call, and the process handle the shell returns is closed below.
    if let Err(error) = unsafe { ShellExecuteExW(&mut info) } {
        let code = error.code().0 as u32;

        return Err(if is_missing_code(code) {
            UninstallError::Missing
        } else if code == SHELL_CANCELLED {
            UninstallError::Cancelled
        } else {
            UninstallError::Failed(error.message())
        });
    }

    // SAFETY: `hProcess` is a process handle the shell handed to us, valid
    // because of `SEE_MASK_NOCLOSEPROCESS`, and closed exactly once.
    if !info.hProcess.is_invalid() {
        unsafe {
            WaitForSingleObject(info.hProcess, INFINITE);
            let _ = CloseHandle(info.hProcess);
        }
    }

    // The exit code is deliberately not judged: installers report 3010 and 1641
    // for "done, reboot pending", and vendor uninstallers invent codes of their
    // own. The proof is whether the program is gone, which the caller measures.
    Ok(())
}

/// Split an uninstall command into the executable and its arguments.
///
/// Quoted commands are unambiguous. Unquoted ones are not — `C:\Program
/// Files\App\unins000.exe /S` contains spaces in the path — so the end of the
/// executable is found by scanning for the `.exe` boundary; only when there is
/// none does the first space win.
fn split_command(command: &str) -> (String, String) {
    let command = command.trim();

    if let Some(rest) = command.strip_prefix('"') {
        return match rest.split_once('"') {
            Some((file, arguments)) => (file.to_string(), arguments.trim().to_string()),
            None => (rest.to_string(), String::new()),
        };
    }

    match exe_end(command) {
        Some(end) => (
            command[..end].to_string(),
            command[end..].trim().to_string(),
        ),
        None => match command.split_once(' ') {
            Some((file, arguments)) => (file.to_string(), arguments.trim().to_string()),
            None => (command.to_string(), String::new()),
        },
    }
}

/// Where the `.exe` of an unquoted command ends, if it names one. The scan is
/// byte-wise and ASCII, so multi-byte characters cannot cause a mid-character
/// slice — UTF-8 continuation bytes are never ASCII.
fn exe_end(command: &str) -> Option<usize> {
    let bytes = command.as_bytes();
    if bytes.len() < 4 {
        return None;
    }

    for index in 0..=bytes.len() - 4 {
        if !bytes[index..index + 4].eq_ignore_ascii_case(b".exe") {
            continue;
        }

        // The name must end there: a folder that happens to be called
        // `something.exe` does not end the executable.
        let end = index + 4;
        match bytes.get(end) {
            None => return Some(end),
            Some(b' ') => return Some(end),
            Some(_) => {}
        }
    }

    None
}

/// Whether the uninstaller's own file is still there, when the command names a
/// path we can probe. A bare command name (`MsiExec.exe`) is resolved by the
/// shell, and a `%VAR%` path needs expanding before it means anything — for
/// both, `None` says "cannot tell from the string alone".
fn uninstaller_presence(command: &str) -> Option<bool> {
    let (file, _) = split_command(command);

    if !file.contains(platform::SEPARATORS) || file.contains('%') {
        return None;
    }

    Some(Path::new(&file).is_file())
}

fn registry_error(id: &str) -> impl FnOnce(std::io::Error) -> WindleError + '_ {
    move |error| WindleError::Command {
        command: "registry".into(),
        message: format!("{id}: {error}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app_fixture() -> InstalledApp {
        InstalledApp {
            id: r"HKCU\SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\Acme".into(),
            name: "Acme".into(),
            bundle_id: None,
            version: Some("1.0".into()),
            path: r"C:\Program Files\Acme".into(),
            bundle_size: 1_000,
            icon_path: None,
            installed_at: None,
            last_used_at: None,
            is_system: false,
        }
    }

    #[test]
    fn splits_quoted_and_unquoted_uninstall_commands() {
        assert_eq!(
            split_command(r#""C:\Program Files\Acme\unins000.exe" /S"#),
            (
                r"C:\Program Files\Acme\unins000.exe".to_string(),
                "/S".to_string()
            )
        );
        assert_eq!(
            split_command(r"C:\Acme\uninstall.exe /quiet"),
            (r"C:\Acme\uninstall.exe".to_string(), "/quiet".to_string())
        );
        // Unquoted with spaces: the `.exe` boundary beats the first space.
        assert_eq!(
            split_command(r"C:\Program Files\Acme\unins000.exe /S"),
            (
                r"C:\Program Files\Acme\unins000.exe".to_string(),
                "/S".to_string()
            )
        );
        // A folder that looks like an executable does not end the name.
        assert_eq!(
            split_command(r"C:\tools.exe\Acme\uninstall.exe /S"),
            (
                r"C:\tools.exe\Acme\uninstall.exe".to_string(),
                "/S".to_string()
            )
        );
        assert_eq!(
            split_command("MsiExec.exe /X{GUID}"),
            ("MsiExec.exe".to_string(), "/X{GUID}".to_string())
        );
        assert_eq!(
            split_command(r#""C:\Acme\uninstall.exe""#),
            (r"C:\Acme\uninstall.exe".to_string(), String::new())
        );
    }

    #[test]
    fn only_probes_uninstallers_that_name_a_path() {
        // A bare command is resolved by the shell; we cannot probe it.
        assert_eq!(uninstaller_presence("MsiExec.exe /X{GUID}"), None);
        // Environment variables need expanding before they mean anything.
        assert_eq!(
            uninstaller_presence(r"%ProgramFiles%\Acme\uninstall.exe /S"),
            None
        );
        // A plain path is probed either way.
        assert_eq!(
            uninstaller_presence(r"C:\Acme\definitely-not-installed.exe /S"),
            Some(false)
        );

        let probe = permissions::expand("%SystemRoot%").join(r"System32");
        assert_eq!(
            uninstaller_presence(&probe.to_string_lossy()),
            Some(probe.is_dir())
        );
    }

    #[test]
    fn ids_split_into_hive_and_path() {
        let (hive, path) =
            split_id(r"HKLM\SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\Acme")
                .expect("a well-formed id");

        assert_eq!(hive, Hive::Machine);
        assert_eq!(
            path,
            r"SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\Acme"
        );

        // Anything that is not one of the two hives is not an id.
        assert!(split_id(r"C:\Program Files\Acme").is_none());
        assert!(split_id(r"HKCR\Software\Acme").is_none());
        assert!(split_id("HKCU").is_none());
    }

    #[test]
    fn only_uninstall_keys_are_registrations() {
        assert!(is_uninstall_key(
            Hive::User,
            r"SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\Acme"
        ));
        assert!(is_uninstall_key(
            Hive::Machine,
            r"SOFTWARE\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall\Acme"
        ));
        // Case does not matter: the registry does not care, so neither do we.
        assert!(is_uninstall_key(
            Hive::User,
            r"software\microsoft\windows\currentversion\uninstall\acme"
        ));

        // The container itself is not an app.
        assert!(!is_uninstall_key(
            Hive::User,
            r"SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall"
        ));
        // Anything outside the table is refused.
        assert!(!is_uninstall_key(Hive::User, r"SOFTWARE\Microsoft\Windows"));
        assert!(!is_uninstall_key(Hive::User, r"Software\Acme"));
        assert!(!is_uninstall_key(
            Hive::User,
            r"SOFTWARE\WOW6432Node\SomethingElse"
        ));
    }

    #[test]
    fn only_strict_software_children_are_settings_keys() {
        assert!(settings_key_parts(r"Software\Acme").is_some());
        assert!(settings_key_parts(r"Software\Acme\Widget").is_some());

        // The OS-owned branches are off limits at any depth.
        assert!(settings_key_parts(r"Software\Microsoft\Edge").is_none());
        assert!(settings_key_parts(r"Software\Policies\Acme").is_none());
        assert!(settings_key_parts(r"Software\WOW6432Node\Acme").is_none());
        assert!(settings_key_parts(r"Software\Classes\Acme.Document").is_none());
        assert!(settings_key_parts(r"Software\Acme\Policies").is_none());

        // `Software` itself, and anything beside it, is not an app's key.
        assert!(settings_key_parts("Software").is_none());
        assert!(settings_key_parts(r"Microsoft\Windows").is_none());
        assert!(settings_key_parts(r"Software\\Acme").is_none());
    }

    #[test]
    fn name_matching_needs_a_long_enough_exact_name() {
        let app = app_fixture();

        assert!(name_matches_app("Acme", &app));
        assert!(name_matches_app("acme", &app));
        assert!(!name_matches_app("Acmex", &app));
        assert!(!name_matches_app("Acme Tools", &app));

        let mut short = app_fixture();
        short.name = "Go".into();
        assert!(!name_matches_app("Go", &short));
    }

    #[test]
    fn shortcuts_match_by_stem() {
        let app = app_fixture();

        assert!(matches_shortcut("Acme.lnk", &app));
        assert!(matches_shortcut("Acme - Shortcut.lnk", &app));
        assert!(!matches_shortcut("Acme.exe", &app));
        assert!(!matches_shortcut("Acme Helper.lnk", &app));
        assert!(!matches_shortcut("Acme", &app));
    }

    #[test]
    fn install_folder_overlap_is_never_a_leftover() {
        let install = Path::new(r"C:\Program Files\Acme");

        assert!(overlaps_install(install, Some(install)));
        assert!(overlaps_install(
            Path::new(r"C:\Program Files\Acme\resources"),
            Some(install)
        ));
        assert!(!overlaps_install(
            Path::new(r"C:\Users\Test\AppData\Roaming\Acme"),
            Some(install)
        ));
        // Without a known install location there is nothing to overlap.
        assert!(!overlaps_install(install, None));
    }

    #[test]
    fn owned_folders_are_matched_case_insensitively() {
        assert!(matches_any("Microsoft", &APP_DATA_OWNED));
        assert!(matches_any("programs", &APP_DATA_OWNED));
        assert!(!matches_any("Mozilla", &APP_DATA_OWNED));

        assert!(matches_any("policies", &SOFTWARE_OWNED));
        assert!(!matches_any("Acme", &SOFTWARE_OWNED));
    }

    #[test]
    fn dates_parse_to_unix_millis() {
        assert_eq!(
            civil_millis(1970, 1, 1, 0, 0, 0),
            Some(0),
            "the epoch itself"
        );
        assert_eq!(civil_millis(2024, 1, 15, 0, 0, 0), Some(1_705_276_800_000));
        assert_eq!(
            civil_millis(2024, 2, 29, 0, 0, 0),
            Some(1_709_164_800_000),
            "a leap day"
        );

        // Fields that are not a real date.
        assert_eq!(civil_millis(2023, 2, 29, 0, 0, 0), None);
        assert_eq!(civil_millis(2024, 13, 1, 0, 0, 0), None);
        assert_eq!(civil_millis(2024, 1, 32, 0, 0, 0), None);
        assert_eq!(civil_millis(2024, 1, 1, 24, 0, 0), None);

        assert_eq!(install_date_millis("20240115"), Some(1_705_276_800_000));
        assert_eq!(install_date_millis("2024-01-15"), None);
        assert_eq!(install_date_millis("20241315"), None);
        assert_eq!(install_date_millis(""), None);
    }
}
