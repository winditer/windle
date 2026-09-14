//! The only place in Windle that actually removes files. Every entry point runs
//! [`permissions::ensure_removable`] first, then either moves the path to the
//! user's trash or unlinks it outright.

use std::path::{Path, PathBuf};

use super::{permissions, WindleError, Result};
use crate::scanner::walker;

/// EXDEV — `rename` cannot move between volumes, so the trash move has to fall
/// back to Finder for anything that is not on the home volume.
const CROSS_DEVICE: i32 = 18;

/// How a path should disappear.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoveMode {
    /// Recoverable: move into `~/.Trash`.
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
        // trashed reliably, so it is always unlinked directly.
        std::fs::remove_file(path).map_err(|error| permissions::classify_io_error(path, &error))?;
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

/// Ask Finder to trash a path. Slower than `rename`, but works across volumes.
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

#[cfg(test)]
mod tests {
    use super::*;

    /// `std::env::temp_dir()` lives under `/private/var/folders`, which is
    /// protected on purpose — scratch files for these tests go in `/tmp`.
    fn scratch(name: &str) -> PathBuf {
        let root = PathBuf::from("/tmp").join(format!("windle-{name}-{}", std::process::id()));
        std::fs::remove_dir_all(&root).ok();
        root
    }

    #[test]
    fn refuses_protected_paths() {
        let error = remove(Path::new("/System/Library"), RemoveMode::Permanent).unwrap_err();
        assert_eq!(error.code(), "protected");

        let home = permissions::home_dir();
        let error = remove(&home.join("Library"), RemoveMode::Trash).unwrap_err();
        assert_eq!(error.code(), "protected");
    }

    #[test]
    fn permanently_removes_a_temporary_tree() {
        let root = scratch("remove");
        let nested = root.join("nested");
        std::fs::create_dir_all(&nested).expect("create temp tree");
        std::fs::write(nested.join("file.bin"), vec![7u8; 2_048]).expect("write temp file");

        let freed = remove(&root, RemoveMode::Permanent).expect("remove temp tree");

        assert!(!root.exists());
        assert_eq!(freed, 2_048);
    }

    #[test]
    fn empty_directory_keeps_the_directory_itself() {
        let root = scratch("empty");
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
        let root = scratch("symlink");
        std::fs::create_dir_all(&root).expect("create tree");
        let target = root.join("target.bin");
        std::fs::write(&target, vec![0u8; 32]).unwrap();
        let link = root.join("link.bin");
        std::os::unix::fs::symlink(&target, &link).expect("create symlink");

        remove(&link, RemoveMode::Permanent).expect("remove symlink");

        assert!(!link.exists());
        assert!(target.exists(), "the target must survive");

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn unique_trash_path_avoids_collisions() {
        let trash = scratch("trash");
        std::fs::create_dir_all(&trash).expect("create fake trash");
        std::fs::write(trash.join("App.dmg"), b"x").expect("seed collision");

        let picked = unique_trash_path(&trash, Path::new("/tmp/App.dmg"));
        assert_eq!(picked, trash.join("App 2.dmg"));

        std::fs::remove_dir_all(&trash).ok();
    }
}
