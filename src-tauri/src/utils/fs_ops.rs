//! The only place in Windle that actually removes files. Every entry point runs
//! [`permissions::ensure_removable`] first, then either moves the path to the
//! platform's trash or unlinks it outright.

use std::path::{Path, PathBuf};

use super::{permissions, WindleError, Result};
use crate::scanner::walker;

/// How a path should disappear.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoveMode {
    /// Recoverable: macOS moves it into `~/.Trash`, Windows into the Recycle Bin.
    Trash,
    /// Unlinked immediately, no recovery.
    Permanent,
}

impl RemoveMode {
    /// Mirrors the `permanent` flag the frontend sends.
    pub fn from_permanent(permanent: bool) -> Self {
        if permanent {
            Self::Permanent
        } else {
            Self::Trash
        }
    }
}

/// Size of `path`, resolved before it is removed so we can report freed space.
pub fn size_of(path: &Path) -> u64 {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.is_dir() => walker::directory_size_parallel(path),
        Ok(meta) => meta.len(),
        Err(_) => 0,
    }
}

/// Remove `path` and return how many bytes that freed.
///
/// Refuses protected paths, and never follows a symlink to its target — the
/// link itself is what gets removed.
pub fn remove(path: &Path, mode: RemoveMode) -> Result<u64> {
    permissions::ensure_removable(path)?;

    let metadata = std::fs::symlink_metadata(path)
        .map_err(|error| permissions::classify_io_error(path, &error))?;

    let freed = size_of(path);

    if metadata.is_symlink() {
        // A dangling or looping link has no meaningful size and cannot be
        // trashed reliably, so it is always unlinked directly. Windows reports
        // directory links — junctions and symlinked folders — through this same
        // branch, and those need `remove_dir`; the failing call is what tells
        // the two apart, which keeps the decision out of metadata guesswork.
        std::fs::remove_file(path)
            .or_else(|_| std::fs::remove_dir(path))
            .map_err(|error| permissions::classify_io_error(path, &error))?;
        return Ok(freed);
    }

    match mode {
        RemoveMode::Trash => move_to_trash(path).map(|_| freed),
        RemoveMode::Permanent => {
            if metadata.is_dir() {
                std::fs::remove_dir_all(path)
            } else {
                std::fs::remove_file(path)
            }
            .map_err(|error| permissions::classify_io_error(path, &error))?;
            Ok(freed)
        }
    }
}

/// Delete every direct child of `dir` without deleting `dir` itself. Used for
/// the container directories that [`permissions`] protects, such as
/// `~/Library/Caches`.
pub fn empty_directory(dir: &Path, mode: RemoveMode) -> (u64, Vec<(PathBuf, WindleError)>) {
    let mut freed = 0;
    let mut failures = Vec::new();

    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) => {
            return (0, vec![(dir.to_path_buf(), permissions::classify_io_error(dir, &error))]);
        }
    };

    for entry in entries.filter_map(std::result::Result::ok) {
        let child = entry.path();
        match remove(&child, mode) {
            Ok(bytes) => freed += bytes,
            Err(error) => failures.push((child, error)),
        }
    }

    (freed, failures)
}

/// Move `path` into `~/.Trash`, keeping its name unique.
#[cfg(target_os = "macos")]
pub fn move_to_trash(path: &Path) -> Result<PathBuf> {
    let trash = permissions::home_dir().join(".Trash");
    std::fs::create_dir_all(&trash)
        .map_err(|error| permissions::classify_io_error(&trash, &error))?;

    let destination = unique_trash_path(&trash, path);

    match std::fs::rename(path, &destination) {
        Ok(()) => Ok(destination),
        // Different volume: let Finder do the copy-and-delete dance, which also
        // records the "Put Back" location.
        Err(error) if error.raw_os_error() == Some(CROSS_DEVICE) => {
            finder_trash(path).map(|()| destination)
        }
        Err(error) => Err(permissions::classify_io_error(path, &error)),
    }
}

/// Move `path` into the Recycle Bin.
///
/// Unlike macOS there is no directory to rename into — the bin is shell state —
/// so the operation goes through the shell, which also records the original
/// location for "Restore". The path we return is the one that was removed:
/// Windows offers no way to ask where a recycled item landed.
#[cfg(target_os = "windows")]
pub fn move_to_trash(path: &Path) -> Result<PathBuf> {
    // The shell recycles what it can and permanently deletes what it cannot
    // (network volumes, oversized items), with all prompts suppressed — the
    // user already asked for the item to go away.
    match trash::delete(path) {
        Ok(()) => Ok(path.to_path_buf()),
        // "Could not access" is the crate's way of saying the process lacks
        // the rights to move the item, which is exactly the distinction the
        // error message is built on.
        Err(trash::Error::CouldNotAccess { .. }) => {
            Err(permissions::classify_io_error(path, &std::io::Error::from(
                std::io::ErrorKind::PermissionDenied,
            )))
        }
        Err(error) => Err(WindleError::Command {
            command: "recycle-bin".into(),
            message: error.to_string(),
        }),
    }
}

/// EXDEV — `rename` cannot move between volumes, so the trash move has to fall
/// back to Finder for anything that is not on the home volume.
#[cfg(target_os = "macos")]
const CROSS_DEVICE: i32 = 18;

/// Ask Finder to trash a path. Slower than `rename`, but works across volumes.
#[cfg(target_os = "macos")]
fn finder_trash(path: &Path) -> Result<()> {
    let script = format!(
        "tell application \"Finder\" to delete POSIX file \"{}\"",
        path.to_string_lossy()
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('\n', "\\n")
            .replace('\t', "\\t")
            .replace('\r', "\\r")
    );

    let output = std::process::Command::new("osascript")
        .args(["-e", &script])
        .output()
        .map_err(|error| permissions::classify_io_error(path, &error))?;

    if output.status.success() {
        Ok(())
    } else {
        Err(WindleError::Command {
            command: "osascript".into(),
            message: String::from_utf8_lossy(&output.stderr).trim().to_string(),
        })
    }
}

/// `~/.Trash/name`, suffixed until it does not collide with an existing item.
#[cfg(target_os = "macos")]
fn unique_trash_path(trash: &Path, source: &Path) -> PathBuf {
    let name = source
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "item".to_string());

    let candidate = trash.join(&name);
    if !candidate.exists() {
        return candidate;
    }

    let (stem, extension) = match name.rsplit_once('.') {
        // Leading-dot names like `.DS_Store` have no stem to split off.
        Some((stem, ext)) if !stem.is_empty() => (stem.to_string(), format!(".{ext}")),
        _ => (name.clone(), String::new()),
    };

    for attempt in 2..1_000 {
        let candidate = trash.join(format!("{stem} {attempt}{extension}"));
        if !candidate.exists() {
            return candidate;
        }
    }

    trash.join(format!("{stem} {}{extension}", crate::commands::clean::now_millis()))
}

// ---------------------------------------------------------------------------
// The Recycle Bin
//
// macOS looks and acts like any other directory (`~/.Trash`), so the Trash
// category simply lists its children. Windows keeps its bin behind shell APIs
// with no path to enumerate, so the category is a single synthetic entry and
// the numbers come from the shell itself.
// ---------------------------------------------------------------------------

/// The one entry the Windows Trash category shows.
#[cfg(target_os = "windows")]
pub const RECYCLE_BIN_SENTINEL: &str = "shell:recycle-bin";

/// Bytes and item count currently sitting in the Recycle Bin, across all drives.
#[cfg(target_os = "windows")]
pub fn recycle_bin_report() -> (u64, u64) {
    use windows::core::PCWSTR;
    use windows::Win32::UI::Shell::{SHQueryRecycleBinW, SHQUERYRBINFO};

    let mut info = SHQUERYRBINFO {
        cbSize: std::mem::size_of::<SHQUERYRBINFO>() as u32,
        ..Default::default()
    };

    // SAFETY: the shell writes into `info`, which is sized by `cbSize` and
    // lives on this stack frame for the duration of the call.
    if unsafe { SHQueryRecycleBinW(PCWSTR::null(), &mut info) }.is_err() {
        return (0, 0);
    }

    // The fields are signed because the struct is shared with 32-bit builds;
    // a negative reading only happens if the shell failed, which reads as 0.
    (info.i64Size.max(0) as u64, info.i64NumItems.max(0) as u64)
}

/// Empty the Recycle Bin on every drive, returning how many bytes that freed.
///
/// This is irreversible, which is why it never appears as a side effect of
/// anything else: only the Trash category calls it, and only when the user
/// asked for the bin to be emptied.
#[cfg(target_os = "windows")]
pub fn empty_recycle_bin() -> Result<u64> {
    use windows::core::PCWSTR;
    use windows::Win32::UI::Shell::{
        SHEmptyRecycleBinW, SHERB_NOCONFIRMATION, SHERB_NOPROGRESSUI, SHERB_NOSOUND,
    };

    let (before, _) = recycle_bin_report();

    // SAFETY: a null window handle and a null root path mean "every drive".
    // No UI is requested, so the call cannot block on a dialog that the user
    // would never see behind the app window.
    let result = unsafe {
        SHEmptyRecycleBinW(
            None,
            PCWSTR::null(),
            SHERB_NOCONFIRMATION | SHERB_NOPROGRESSUI | SHERB_NOSOUND,
        )
    };

    if let Err(error) = result {
        return Err(WindleError::Command {
            command: "empty-recycle-bin".into(),
            message: error.to_string(),
        });
    }

    // The shell reports the size it had before emptying, so the difference is
    // what actually went away; a few files held open by other programs stay.
    let (after, _) = recycle_bin_report();
    Ok(before.saturating_sub(after))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_support::scratch;

    /// A path the guard rails always refuse: the system directory on Windows,
    /// the system tree on macOS.
    fn system_path() -> PathBuf {
        #[cfg(target_os = "windows")]
        {
            crate::utils::platform::env_path("SystemRoot")
                .expect("SystemRoot is set on Windows")
                .join("System32")
        }
        #[cfg(not(target_os = "windows"))]
        {
            PathBuf::from("/System/Library")
        }
    }

    /// A directory the guard rails refuse to delete outright but allow us to
    /// empty: `~/Library` on macOS, `~/AppData` on Windows.
    fn user_container() -> PathBuf {
        #[cfg(target_os = "windows")]
        {
            permissions::home_dir().join("AppData")
        }
        #[cfg(not(target_os = "windows"))]
        {
            permissions::home_dir().join("Library")
        }
    }

    #[test]
    fn refuses_protected_paths() {
        let error = remove(&system_path(), RemoveMode::Permanent).unwrap_err();
        assert_eq!(error.code(), "protected");

        let error = remove(&user_container(), RemoveMode::Trash).unwrap_err();
        assert_eq!(error.code(), "protected");
    }

    #[test]
    fn permanently_removes_a_temporary_tree() {
        let root = scratch("fs-remove");
        let nested = root.join("nested");
        std::fs::create_dir_all(&nested).expect("create temp tree");
        std::fs::write(nested.join("file.bin"), vec![7u8; 2_048]).expect("write temp file");

        let freed = remove(&root, RemoveMode::Permanent).expect("remove temp tree");

        assert!(!root.exists());
        assert_eq!(freed, 2_048);
    }

    #[test]
    fn empty_directory_keeps_the_directory_itself() {
        let root = scratch("fs-empty");
        std::fs::create_dir_all(root.join("child-dir")).expect("create tree");
        std::fs::write(root.join("child-dir/data.bin"), vec![0u8; 128]).unwrap();
        std::fs::write(root.join("loose.bin"), vec![0u8; 64]).unwrap();

        let (freed, failures) = empty_directory(&root, RemoveMode::Permanent);

        assert!(failures.is_empty(), "{failures:?}");
        assert_eq!(freed, 192);
        assert!(root.is_dir(), "the container must survive");
        assert_eq!(std::fs::read_dir(&root).unwrap().count(), 0);

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn removes_a_symlink_without_touching_its_target() {
        let root = scratch("fs-symlink");
        std::fs::create_dir_all(&root).expect("create tree");
        let target = root.join("target.bin");
        std::fs::write(&target, vec![0u8; 32]).unwrap();
        let link = root.join("link.bin");

        // Creating a symlink on Windows needs a privilege that is not always
        // granted; the test steps aside rather than failing on such a machine.
        if !crate::utils::test_support::symlink_file(&target, &link) {
            std::fs::remove_dir_all(&root).ok();
            return;
        }

        remove(&link, RemoveMode::Permanent).expect("remove symlink");

        assert!(!link.exists());
        assert!(target.exists(), "the target must survive");

        std::fs::remove_dir_all(&root).ok();
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn unique_trash_path_avoids_collisions() {
        let trash = scratch("fs-trash");
        std::fs::create_dir_all(&trash).expect("create fake trash");
        std::fs::write(trash.join("App.dmg"), b"x").expect("seed collision");

        let picked = unique_trash_path(&trash, Path::new("/tmp/App.dmg"));
        assert_eq!(picked, trash.join("App 2.dmg"));

        std::fs::remove_dir_all(&trash).ok();
    }

    /// Recycling is handled by the shell, so this can only be exercised on
    /// Windows itself. A machine without an interactive shell (a service, a
    /// container) cannot recycle anything, and the test steps aside there.
    #[cfg(target_os = "windows")]
    #[test]
    fn recycling_hands_the_item_to_the_shell() {
        let root = scratch("fs-recycle");
        std::fs::create_dir_all(&root).expect("create tree");
        let doomed = root.join("windle-recycle-me.bin");
        std::fs::write(&doomed, vec![0u8; 512]).unwrap();

        let (_, before_items) = recycle_bin_report();

        if let Err(error) = move_to_trash(&doomed) {
            eprintln!("skipping: this session cannot recycle files ({error})");
            std::fs::remove_dir_all(&root).ok();
            return;
        }

        assert!(!doomed.exists(), "the item left the working tree");

        let (bytes, items) = recycle_bin_report();
        assert!(items > before_items, "the shell should list the new item");
        assert!(bytes >= 512, "the bin should have grown, saw {bytes} bytes");

        std::fs::remove_dir_all(&root).ok();
    }
}
