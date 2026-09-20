//! Path helpers for the places where Windows and Unix disagree.
//!
//! Path comparison is the big one: `Path::starts_with` is case-sensitive on
//! every platform, while Windows resolves `C:\Windows` and `c:\WINDOWS` to the
//! same directory. Everything that decides whether a path sits inside another
//! one goes through [`starts_with`] so the guard rails cannot be side-stepped
//! by a differently-cased path.

use std::ffi::OsStr;
use std::path::{Component, Path, PathBuf};

/// Platform name handed to the frontend so it can adapt window chrome and
/// wording. Mirrors the `#[cfg]` branches in this file.
pub fn name() -> &'static str {
    if cfg!(target_os = "windows") {
        "windows"
    } else if cfg!(target_os = "macos") {
        "macos"
    } else {
        "linux"
    }
}

/// Compare two path components the way this platform's filesystem does.
fn component_eq(left: &OsStr, right: &OsStr) -> bool {
    #[cfg(target_os = "windows")]
    {
        left.to_string_lossy().eq_ignore_ascii_case(&right.to_string_lossy())
    }
    #[cfg(not(target_os = "windows"))]
    {
        left == right
    }
}

/// Whether `path` is `prefix` itself or sits below it.
///
/// Component-wise, so `/a/bc` is not inside `/a/b`, and case-insensitive on
/// Windows. Both sides are expected to be absolute.
pub fn starts_with(path: &Path, prefix: &Path) -> bool {
    let mut path_parts = path.components();
    for wanted in prefix.components() {
        match path_parts.next() {
            Some(found) if component_eq(found.as_os_str(), wanted.as_os_str()) => {}
            _ => return false,
        }
    }
    true
}

/// Whether both paths point at the same location.
pub fn eq(left: &Path, right: &Path) -> bool {
    starts_with(left, right) && starts_with(right, left)
}

/// Drop the `\\?\` prefix `std::fs::canonicalize` adds on Windows, so paths
/// print the way Explorer shows them and compare against `%SystemRoot%`-style
/// constants. Without this, a canonicalized `\\?\C:\Windows` would not match
/// the plain `C:\Windows` in the protection lists.
pub fn strip_verbatim(path: &Path) -> PathBuf {
    #[cfg(target_os = "windows")]
    {
        let text = path.as_os_str().to_string_lossy();
        if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
            return PathBuf::from(format!(r"\\{rest}"));
        }
        if let Some(rest) = text.strip_prefix(r"\\?\") {
            return PathBuf::from(rest);
        }
    }
    path.to_path_buf()
}

/// [`std::fs::canonicalize`] with the verbatim prefix removed. Falls back to
/// the literal path when it cannot be resolved (missing files, denied
/// directories), which keeps the guard rails usable for paths that are about
/// to be created or removed.
pub fn canonical(path: &Path) -> PathBuf {
    match std::fs::canonicalize(path) {
        Ok(resolved) => strip_verbatim(&resolved),
        Err(_) => strip_verbatim(path),
    }
}

/// Characters that separate path components on this platform, so display
/// helpers can elide either flavour of separator.
pub const SEPARATORS: [char; 2] = ['/', '\\'];

/// Path to an environment variable, when it is set to a non-empty value.
pub fn env_path(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

/// Whether a component is a Windows drive or UNC prefix, used by the display
/// helpers to keep the root of a path intact when eliding.
pub fn is_root_component(component: &Component<'_>) -> bool {
    matches!(
        component,
        Component::Prefix(_) | Component::RootDir | Component::CurDir | Component::ParentDir
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefix_matching_stops_at_component_boundaries() {
        assert!(starts_with(Path::new("/a/b/c"), Path::new("/a/b")));
        assert!(starts_with(Path::new("/a/b"), Path::new("/a/b")));
        assert!(!starts_with(Path::new("/a/bc"), Path::new("/a/b")));
        assert!(!starts_with(Path::new("/a"), Path::new("/a/b")));
    }

    #[test]
    fn equality_holds_in_both_directions() {
        assert!(eq(Path::new("/a/b"), Path::new("/a/b/")));
        assert!(!eq(Path::new("/a/b"), Path::new("/a/bc")));
    }

    #[test]
    fn canonical_resolves_existing_paths() {
        let resolved = canonical(Path::new("/tmp"));
        assert!(resolved.is_absolute());
        assert!(!resolved.to_string_lossy().starts_with(r"\\?\"));
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn windows_comparisons_ignore_case() {
        assert!(starts_with(
            Path::new(r"C:\Windows\Temp\file.tmp"),
            Path::new(r"c:\windows")
        ));
        assert!(eq(Path::new(r"C:\Users\X"), Path::new(r"c:\users\x")));
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn verbatim_prefixes_are_removed() {
        assert_eq!(
            strip_verbatim(Path::new(r"\\?\C:\Windows")),
            PathBuf::from(r"C:\Windows")
        );
        assert_eq!(
            strip_verbatim(Path::new(r"\\?\UNC\server\share")),
            PathBuf::from(r"\\server\share")
        );
    }
}
