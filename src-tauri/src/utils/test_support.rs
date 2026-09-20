//! Helpers the platform-specific tests share.

use std::path::PathBuf;

/// Create a symlink to a file, reporting whether it worked.
///
/// Windows only lets a process create symlinks when it holds
/// `SeCreateSymbolicLinkPrivilege` — i.e. when running elevated, or with
/// Developer Mode enabled — so tests skip their symlink assertions when this
/// returns `false` rather than failing on a machine that cannot make one.
pub fn symlink_file(target: &std::path::Path, link: &std::path::Path) -> bool {
    #[cfg(target_os = "windows")]
    {
        std::os::windows::fs::symlink_file(target, link).is_ok()
    }
    #[cfg(not(target_os = "windows"))]
    {
        std::os::unix::fs::symlink(target, link).is_ok()
    }
}

/// [`symlink_file`], for a link that points at a directory.
pub fn symlink_dir(target: &std::path::Path, link: &std::path::Path) -> bool {
    #[cfg(target_os = "windows")]
    {
        std::os::windows::fs::symlink_dir(target, link).is_ok()
    }
    #[cfg(not(target_os = "windows"))]
    {
        std::os::unix::fs::symlink(target, link).is_ok()
    }
}

/// The directory test fixtures are built in.
///
/// macOS uses `/tmp` rather than `std::env::temp_dir()`, because the latter
/// lives under `/private/var/folders` — a location the guard rails protect, so
/// fixture paths that pass through them would be refused. Windows has no such
/// sandbox, and its temp directory is an ordinary user path.
pub fn scratch_base() -> PathBuf {
    #[cfg(target_os = "windows")]
    {
        std::env::temp_dir()
    }
    #[cfg(not(target_os = "windows"))]
    {
        PathBuf::from("/tmp")
    }
}

/// A private scratch directory named after the test, so parallel runs cannot
/// collide. Anything left over from an earlier run is removed first.
pub fn scratch(name: &str) -> PathBuf {
    let root = scratch_base().join(format!("windle-{name}-{}", std::process::id()));
    std::fs::remove_dir_all(&root).ok();
    root
}
