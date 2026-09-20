//! Permission checks and the guard rails that keep Windle from deleting
//! something it should not.
//!
//! The rules are the same on every platform — refuse system locations, refuse
//! containers we are only allowed to empty — while the three path lists and the
//! permission probes live in the platform modules.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::{platform, WindleError, Result};

#[cfg(target_os = "macos")]
#[path = "macos.rs"]
mod imp;
#[cfg(target_os = "windows")]
#[path = "windows.rs"]
mod imp;

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PermissionState {
    pub full_disk_access: bool,
    pub admin_authorized: bool,
}

/// Whether the "protected location" permission the platform asks for has been
/// granted. macOS gates parts of the disk behind Full Disk Access; Windows has
/// no equivalent, so this is always true there and the dashboard banner that
/// asks for it never appears.
pub fn has_full_disk_access() -> bool {
    imp::has_full_disk_access()
}

/// Current permission snapshot for the dashboard banner.
pub fn state() -> PermissionState {
    PermissionState {
        full_disk_access: has_full_disk_access(),
        admin_authorized: is_root(),
    }
}

/// Walk the user to whatever settings pane grants the missing permission.
pub fn request_permissions() -> Result<()> {
    imp::request_permissions()
}

/// Whether Windle is running with administrative rights.
pub fn is_root() -> bool {
    imp::is_elevated()
}

/// Effective user id, needed to address per-user `launchctl` domains.
#[cfg(target_os = "macos")]
pub fn current_uid() -> u32 {
    imp::current_uid()
}

pub fn home_dir() -> PathBuf {
    dirs::home_dir().unwrap_or_else(|| PathBuf::from(std::path::MAIN_SEPARATOR.to_string()))
}

/// Expand a leading `~` against the current home directory.
pub fn expand_tilde(entry: &str) -> PathBuf {
    match entry.strip_prefix('~') {
        Some("") => home_dir(),
        Some(rest) => home_dir().join(rest.trim_start_matches(platform::SEPARATORS)),
        None => PathBuf::from(entry),
    }
}

/// Expand one entry of a platform path list: `~` for the home directory,
/// `%NAME%` for an environment variable, anything else taken literally.
fn expand_entry(entry: &str) -> PathBuf {
    if entry.starts_with('~') {
        return expand_tilde(entry);
    }

    if let Some(rest) = entry.strip_prefix('%') {
        if let Some((name, tail)) = rest.split_once('%') {
            if let Some(root) = platform::env_path(name) {
                return root.join(tail.trim_start_matches(platform::SEPARATORS));
            }
            // An unset variable leaves the entry unresolvable; an empty path
            // matches nothing, which is the safe direction for a guard rail.
            return PathBuf::new();
        }
    }

    PathBuf::from(entry)
}

/// [`expand_entry`] for the guard rails, where an unset variable means "not
/// listed": an empty path compares equal to nothing, whereas leaving it in the
/// comparison would match every path and lock the guard shut on a machine that
/// happens not to define an optional variable such as `%ProgramFiles(x86)%`.
fn expanded(entry: &str) -> Option<PathBuf> {
    let path = expand_entry(entry);
    (!path.as_os_str().is_empty()).then_some(path)
}

/// Expand a path-list entry — `~` for the home directory, `%NAME%` for an
/// environment variable, anything else taken literally. Entries whose variable
/// is unset expand to an empty path, which callers treat as "not present".
pub fn expand(entry: &str) -> PathBuf {
    expand_entry(entry)
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
    let exempted = imp::EXEMPTED_SUBTREES.iter().any(|root| {
        expanded(root).is_some_and(|root| {
            platform::starts_with(&resolved, &root) && !platform::eq(&resolved, &root)
        })
    });

    if !exempted
        && imp::PROTECTED_PREFIXES.iter().any(|prefix| {
            expanded(prefix).is_some_and(|prefix| platform::starts_with(&resolved, &prefix))
        })
    {
        return true;
    }

    imp::PROTECTED_EXACT
        .iter()
        .any(|entry| expanded(entry).is_some_and(|entry| platform::eq(&resolved, &entry)))
}

/// Resolve symlinks where possible so a link cannot point us at a protected
/// location. Falls back to the literal path when it no longer exists.
pub fn resolve(path: &Path) -> PathBuf {
    platform::canonical(path)
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
    imp::needs_elevation(path)
}

/// Where the volume the system booted from is mounted, used by the analyzer.
pub fn boot_mount_point() -> PathBuf {
    imp::boot_mount_point()
}

/// Map an I/O error to the richer Windle variant, so the UI can suggest granting
/// the missing permission instead of showing a bare "permission denied".
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
    fn rejects_relative_and_climbing_paths() {
        assert!(ensure_removable(Path::new("some/relative")).is_err());
        assert!(ensure_removable(Path::new("/tmp/../etc")).is_err());
    }

    #[test]
    fn rejects_container_directories_but_allows_their_children() {
        let home = home_dir();

        assert!(ensure_removable(&home).is_err());
        assert!(ensure_removable(&home.join("Documents")).is_err());
        assert!(ensure_removable(&home.join("Documents/notes.txt")).is_ok());
    }
}
