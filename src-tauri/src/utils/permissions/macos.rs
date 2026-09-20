//! macOS specifics: Full Disk Access, `geteuid`, and the system prefixes that
//! must never be touched.

use std::path::{Path, PathBuf};

use super::{home_dir, WindleError, Result};

/// Whole subtrees that must never be removed, no matter what a scanner reports.
pub const PROTECTED_PREFIXES: &[&str] = &[
    "/System",
    "/bin",
    "/sbin",
    "/usr",
    "/dev",
    "/etc",
    "/private/etc",
    "/private/var/db",
    // Per-boot temp/cache sandboxes: cheap for the OS to manage, dangerous for
    // us to touch while apps are running.
    "/private/var/folders",
    "/Library/Apple",
    "/Library/Developer/CommandLineTools",
    "/Library/Frameworks",
    "/Library/Extensions",
    "/Library/LaunchDaemons",
    "/opt/homebrew",
];

/// Directories we may empty but must never delete outright. Stored relative to
/// `$HOME` when the entry starts with `~`, absolute otherwise.
pub const PROTECTED_EXACT: &[&str] = &[
    "/",
    "/Applications",
    "/Applications/Utilities",
    "/Library",
    "/Library/Application Support",
    "/Library/Caches",
    "/Library/Logs",
    "/Library/Preferences",
    "/Users",
    "/Volumes",
    "/tmp",
    "/private/tmp",
    "/private/var",
    "/private/var/tmp",
    "/private/var/log",
    "~",
    "~/.Trash",
    "~/Applications",
    "~/Desktop",
    "~/Documents",
    "~/Downloads",
    "~/Library",
    "~/Library/Application Support",
    "~/Library/Caches",
    "~/Library/Containers",
    "~/Library/Group Containers",
    "~/Library/Logs",
    "~/Library/Preferences",
    "~/Library/Saved Application State",
    "~/Movies",
    "~/Music",
    "~/Pictures",
];

/// Subtrees that sit under an otherwise-protected prefix but whose **strict
/// descendants** (any depth, excluding the root itself) may pass the guard: the
/// subtree root stays protected, while every path below it can be offered for
/// deletion. The `PROTECTED_EXACT` check below still applies in full.
///
/// Used to clear archived unified logs (`tracev3`) out from under
/// `/private/var/db` without opening up the rest of that database-laden
/// prefix. Depth is deliberately not limited: the whole subtree is nothing but
/// logd log data, and deleting a listed directory entry (`remove_dir_all`)
/// never re-checks the guard for the files inside it, so restricting the guard
/// to a particular depth would add no extra safety.
pub const EXEMPTED_SUBTREES: &[&str] = &["/private/var/db/diagnostics"];

/// Probing a directory that is only readable with Full Disk Access tells us
/// whether the user has granted it.
pub fn has_full_disk_access() -> bool {
    let probe = home_dir().join("Library/Mail");
    match std::fs::read_dir(&probe) {
        Ok(_) => true,
        // A missing Mail directory is not a permission problem — fall back to
        // the TCC database, which is always FDA-gated.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            std::fs::File::open("/Library/Application Support/com.apple.TCC/TCC.db").is_ok()
        }
        Err(_) => false,
    }
}

/// Open System Settings on the pane that grants the missing permission.
pub fn request_permissions() -> Result<()> {
    open_settings_pane("Privacy_AllFiles")
}

/// Open one Privacy & Security pane by its anchor, e.g. `Privacy_AllFiles`.
pub fn open_settings_pane(anchor: &str) -> Result<()> {
    let url = format!("x-apple.systempreferences:com.apple.preference.security?{anchor}");
    let status = std::process::Command::new("open").arg(&url).status()?;

    if status.success() {
        Ok(())
    } else {
        Err(WindleError::Command {
            command: "open".into(),
            message: format!("could not open the {anchor} settings pane"),
        })
    }
}

pub fn is_elevated() -> bool {
    // SAFETY: `geteuid` is always safe to call and cannot fail.
    unsafe { libc_geteuid() == 0 }
}

/// Effective user id, needed to address per-user `launchctl` domains.
pub fn current_uid() -> u32 {
    // SAFETY: see `is_elevated`.
    unsafe { libc_geteuid() }
}

// Declared locally so we don't pull in the whole `libc` crate for one call.
extern "C" {
    #[link_name = "geteuid"]
    fn libc_geteuid() -> u32;
}

/// Any path outside the home directory needs an admin prompt to remove.
pub fn needs_elevation(path: &Path) -> bool {
    !path.starts_with(home_dir())
}

/// Kept for the analyzer's boot-volume pick.
pub fn boot_mount_point() -> PathBuf {
    PathBuf::from("/")
}

#[cfg(test)]
mod tests {
    use super::super::{ensure_removable, home_dir};
    use super::*;

    #[test]
    fn rejects_system_subtrees() {
        for path in [
            "/System/Library",
            "/usr/bin/env",
            "/bin/sh",
            "/Library/Apple/usr",
        ] {
            assert!(
                ensure_removable(Path::new(path)).is_err(),
                "{path} must be protected"
            );
        }
    }

    #[test]
    fn rejects_container_directories_but_allows_their_children() {
        let home = home_dir();

        assert!(ensure_removable(&home).is_err());
        assert!(ensure_removable(&home.join("Library")).is_err());
        assert!(ensure_removable(&home.join("Library/Caches")).is_err());
        assert!(ensure_removable(Path::new("/Applications")).is_err());

        // A cache belonging to one app is fair game.
        assert!(ensure_removable(&home.join("Library/Caches/com.example.App")).is_ok());
    }

    #[test]
    fn strict_descendants_of_exempted_subtrees_are_removable() {
        // The archived unified-log directories directly under
        // `/private/var/db/diagnostics` may be offered for deletion …
        assert!(ensure_removable(Path::new("/private/var/db/diagnostics/Persist")).is_ok());

        // … and so may every deeper path: the scanner offers the individual
        // `tracev3` archives inside those directories, so the guard must not
        // stop at depth one.
        assert!(ensure_removable(Path::new("/private/var/db/diagnostics/Persist/x.tracev3")).is_ok());
        assert!(
            ensure_removable(Path::new("/private/var/db/diagnostics/Persist/sub/x.tracev3")).is_ok()
        );

        // The exempted subtree root itself stays protected …
        assert!(ensure_removable(Path::new("/private/var/db/diagnostics")).is_err());
        // … as does every other path under the same protected prefix.
        assert!(ensure_removable(Path::new("/private/var/db/uuidtext")).is_err());
        assert!(ensure_removable(Path::new("/private/var/db/uuidtext/xxx")).is_err());
        assert!(ensure_removable(Path::new("/private/var/db/ConfigurationProfiles")).is_err());
        // A same-prefix sibling directory must NOT be exempted: this locks in
        // the component-level semantics of `Path::starts_with`, so a future
        // refactor to a plain string-prefix comparison fails here.
        assert!(ensure_removable(Path::new("/private/var/db/diagnosticsfoo")).is_err());
    }

    #[test]
    fn home_directories_need_no_elevation() {
        assert!(!needs_elevation(&home_dir().join("Library/Caches")));
        assert!(needs_elevation(Path::new("/Library/Caches")));
    }
}
