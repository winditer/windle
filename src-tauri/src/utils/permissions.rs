//! Permission checks and the guard rails that keep Windle from deleting
//! something it should not.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::{WindleError, Result};

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PermissionState {
    pub full_disk_access: bool,
    pub admin_authorized: bool,
}

/// Whole subtrees that must never be removed, no matter what a scanner reports.
const PROTECTED_PREFIXES: [&str; 15] = [
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
const PROTECTED_EXACT: [&str; 32] = [
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
const EXEMPTED_SUBTREES: [&str; 1] = ["/private/var/db/diagnostics"];

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

/// Current permission snapshot for the dashboard banner.
pub fn state() -> PermissionState {
    PermissionState {
        full_disk_access: has_full_disk_access(),
        admin_authorized: is_root(),
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

pub fn is_root() -> bool {
    // SAFETY: `geteuid` is always safe to call and cannot fail.
    unsafe { libc_geteuid() == 0 }
}

/// Effective user id, needed to address per-user `launchctl` domains.
pub fn current_uid() -> u32 {
    // SAFETY: see `is_root`.
    unsafe { libc_geteuid() }
}

// Declared locally so we don't pull in the whole `libc` crate for one call.
extern "C" {
    #[link_name = "geteuid"]
    fn libc_geteuid() -> u32;
}

pub fn home_dir() -> PathBuf {
    dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"))
}

/// Expand a leading `~` against the current home directory.
pub fn expand_tilde(entry: &str) -> PathBuf {
    match entry.strip_prefix('~') {
        Some("") => home_dir(),
        Some(rest) => home_dir().join(rest.trim_start_matches('/')),
        None => PathBuf::from(entry),
    }
}

/// True when `path` is one of the directories we are only allowed to empty.
pub fn is_protected(path: &Path) -> bool {
    let resolved = resolve(path);

    // A strict-descendant check — the exempted root itself is excluded, while
    // every deeper path below it passes. The subtree root (whose parent is
    // the protected prefix) stays protected. Deeper descendants are allowed
    // because the scanner offers individual archived files inside the subtree,
    // and deleting a listed directory never re-checks the guard for its
    // contents, so limiting the depth would add no safety.
    let exempted = EXEMPTED_SUBTREES
        .iter()
        .any(|root| resolved != Path::new(root) && resolved.starts_with(root));

    if !exempted
        && PROTECTED_PREFIXES
            .iter()
            .any(|prefix| resolved.starts_with(prefix))
    {
        return true;
    }

    PROTECTED_EXACT
        .iter()
        .any(|entry| resolved == expand_tilde(entry))
}

/// Resolve symlinks where possible so a link cannot point us at a protected
/// location. Falls back to the literal path when it no longer exists.
fn resolve(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

/// Reject anything that is not a concrete, non-protected path we are willing to
/// delete. Every removal in Windle goes through this first.
pub fn ensure_removable(path: &Path) -> Result<()> {
    let display = path.to_string_lossy().into_owned();

    if !path.is_absolute() {
        return Err(WindleError::Protected(display));
    }

    // A `..` component could climb out of an otherwise safe subtree.
    if path
        .components()
        .any(|part| matches!(part, std::path::Component::ParentDir))
    {
        return Err(WindleError::Protected(display));
    }

    let resolved = resolve(path);

    // The filesystem root has no parent; nothing else should reach this.
    if resolved.parent().is_none() {
        return Err(WindleError::Protected(display));
    }

    if is_protected(&resolved) {
        return Err(WindleError::Protected(display));
    }

    Ok(())
}

/// Whether removing `path` will need an admin prompt.
pub fn needs_elevation(path: &Path) -> bool {
    !path.starts_with(home_dir())
}

/// Map an I/O error to the richer Windle variant, so the UI can suggest granting
/// Full Disk Access instead of showing a bare "permission denied".
pub fn classify_io_error(path: &Path, error: &std::io::Error) -> WindleError {
    let display = path.to_string_lossy().into_owned();

    match error.kind() {
        std::io::ErrorKind::NotFound => WindleError::NotFound(display),
        std::io::ErrorKind::PermissionDenied => {
            if needs_elevation(path) {
                WindleError::NeedsElevation(display)
            } else {
                WindleError::AccessDenied(display)
            }
        }
        _ => WindleError::AccessDenied(display),
    }
}

#[cfg(test)]
mod tests {
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
    fn rejects_relative_and_climbing_paths() {
        assert!(ensure_removable(Path::new("Library/Caches")).is_err());
        assert!(ensure_removable(Path::new("/tmp/../System")).is_err());
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
}
