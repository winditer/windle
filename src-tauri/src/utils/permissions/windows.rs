//! Windows specifics: token elevation and the system prefixes that must never
//! be touched.
//!
//! Windows has no Full Disk Access equivalent — a standard user can read most
//! of their own machine — so the two permission probes report "granted" and the
//! dashboard banner that asks for it never appears. What does exist is UAC:
//! machine-wide locations silently fail to delete without an elevated token,
//! which is what [`needs_elevation`] predicts.

use std::path::{Path, PathBuf};

use super::{expand_entry, home_dir, platform, Result};

/// Whole subtrees that must never be removed, no matter what a scanner reports.
/// Entries beginning with `%` are environment variables.
pub const PROTECTED_PREFIXES: &[&str] = &[
    // The OS itself: System32, WinSxS, Drivers, Fonts, and the `Installer`
    // cache that application repair and uninstall depend on.
    "%SystemRoot%",
    // Installed applications.
    "%ProgramFiles%",
    "%ProgramFiles(x86)%",
    "%ProgramW6432%",
    // Secrets and databases under ProgramData. The rest of that tree is app
    // data we are allowed to clean, so these are listed one by one.
    "%ProgramData%\\Microsoft\\Crypto",
    "%ProgramData%\\Microsoft\\Protect",
    "%ProgramData%\\Microsoft\\Windows\\SystemData",
    "%ProgramData%\\Microsoft\\Windows\\AppRepository",
    "%ProgramData%\\Microsoft\\Windows Defender",
    "%ProgramData%\\Microsoft\\Windows\\Caches",
    "%ProgramData%\\Package Cache",
    // Per-volume system metadata. The Recycle Bin of non-system drives is not
    // covered here; it is emptied through the shell instead of by path.
    "%SystemDrive%\\$Recycle.Bin",
    "%SystemDrive%\\$SysReset",
    "%SystemDrive%\\$WinREAgent",
    "%SystemDrive%\\Boot",
    "%SystemDrive%\\Documents and Settings",
    "%SystemDrive%\\PerfLogs",
    "%SystemDrive%\\Recovery",
    "%SystemDrive%\\System Volume Information",
];

/// Directories we may empty but must never delete outright. Stored relative to
/// the profile when the entry starts with `~`, via `%NAME%` otherwise.
pub const PROTECTED_EXACT: &[&str] = &[
    "%SystemDrive%\\Users",
    "~",
    // A user's own folders: the analyzer may list what is inside them, but
    // never the folders themselves.
    "~\\Contacts",
    "~\\Desktop",
    "~\\Documents",
    "~\\Downloads",
    "~\\Favorites",
    "~\\Links",
    "~\\Music",
    "~\\OneDrive",
    "~\\Pictures",
    "~\\Saved Games",
    "~\\Searches",
    "~\\Videos",
    // Application data containers. Individual caches below them are fair game;
    // wholesale removal of a container is not.
    "~\\AppData",
    "~\\AppData\\Local",
    "~\\AppData\\Local\\Packages",
    "~\\AppData\\LocalLow",
    "~\\AppData\\Roaming",
    // Credentials and keys that are expensive or impossible to recreate.
    "~\\.aws",
    "~\\.azure",
    "~\\.gnupg",
    "~\\.kube",
    "~\\.ssh",
    // The Start menus: app entries below them may be cleaned up after an
    // uninstall, the folders holding them may not.
    "%APPDATA%\\Microsoft\\Windows\\Start Menu",
    "%ProgramData%\\Microsoft\\Windows\\Start Menu",
];

/// Subtrees that sit under an otherwise-protected prefix but whose **strict
/// descendants** (any depth, excluding the root itself) may pass the guard: the
/// subtree root stays protected, while every path below it can be offered for
/// deletion. The `PROTECTED_EXACT` check below still applies in full.
///
/// These are the Windows-managed scratch areas: caches, logs and crash dumps
/// that the OS expects to be trimmed. Everything else under `%SystemRoot%` —
/// System32, WinSxS, the MSI cache — stays untouchable.
pub const EXEMPTED_SUBTREES: &[&str] = &[
    "%SystemRoot%\\LiveKernelReports",
    "%SystemRoot%\\Logs",
    "%SystemRoot%\\Minidump",
    "%SystemRoot%\\Prefetch",
    "%SystemRoot%\\SoftwareDistribution\\DeliveryOptimization",
    "%SystemRoot%\\SoftwareDistribution\\Download",
    "%SystemRoot%\\System32\\LogFiles",
    "%SystemRoot%\\Temp",
    "%ProgramData%\\Microsoft\\Windows\\WER",
    "%ProgramData%\\Temp",
];

/// Machine-wide locations that a standard user cannot write to, and that
/// therefore need a UAC prompt before we try. The current user's own profile is
/// never in this set.
pub const MACHINE_WIDE: &[&str] = &[
    "%SystemRoot%",
    "%ProgramFiles%",
    "%ProgramFiles(x86)%",
    "%ProgramW6432%",
    "%ProgramData%",
    "%SystemDrive%\\$Recycle.Bin",
    "%SystemDrive%\\$SysReset",
    "%SystemDrive%\\$WinREAgent",
    "%SystemDrive%\\Boot",
    "%SystemDrive%\\Documents and Settings",
    "%SystemDrive%\\PerfLogs",
    "%SystemDrive%\\Recovery",
    "%SystemDrive%\\System Volume Information",
    // Other people's profiles, and the shared Public folders.
    "%SystemDrive%\\Users",
];

/// There is nothing to grant: everything a standard user may read, we may read.
pub fn has_full_disk_access() -> bool {
    true
}

/// The dashboard banner offers this as a fix; on Windows there is no such
/// permission, so it only reports success.
pub fn request_permissions() -> Result<()> {
    Ok(())
}

/// Whether the process holds an elevated (administrator) token.
pub fn is_elevated() -> bool {
    use windows::Win32::Foundation::{CloseHandle, HANDLE};
    use windows::Win32::Security::{
        GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY,
    };
    use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    // SAFETY: the process handle and token are owned by this thread, the output
    // buffer is exactly the size we pass, and the token is closed on both paths.
    unsafe {
        let mut token = HANDLE::default();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).is_err() {
            return false;
        }

        let mut elevation = TOKEN_ELEVATION::default();
        let result = GetTokenInformation(
            token,
            TokenElevation,
            Some(&mut elevation as *mut _ as *mut core::ffi::c_void),
            std::mem::size_of::<TOKEN_ELEVATION>() as u32,
            std::ptr::null_mut(),
        );
        let _ = CloseHandle(token);

        result.is_ok() && elevation.TokenIsElevated != 0
    }
}

/// Whether removing `path` will raise a UAC prompt.
pub fn needs_elevation(path: &Path) -> bool {
    let path = platform::strip_verbatim(path);

    // The user's own profile is always writable without a prompt.
    if platform::starts_with(&path, &home_dir()) {
        return false;
    }

    MACHINE_WIDE.iter().any(|entry| {
        let root = expand_entry(entry);
        !root.as_os_str().is_empty() && platform::starts_with(&path, &root)
    })
}

/// Kept for the analyzer's boot-volume pick.
pub fn boot_mount_point() -> PathBuf {
    platform::env_path("SystemDrive")
        .map(|drive| {
            PathBuf::from(format!(
                "{}\\",
                drive.to_string_lossy().trim_end_matches(platform::SEPARATORS)
            ))
        })
        .unwrap_or_else(|| PathBuf::from("C:\\"))
}

#[cfg(test)]
mod tests {
    use super::super::{ensure_removable, home_dir};
    use super::*;

    #[test]
    fn rejects_system_subtrees() {
        let windows = expand_entry("%SystemRoot%");

        for path in [
            windows.join("System32"),
            windows.join("System32").join("config"),
            windows.join("Installer"),
            windows.join("WinSxS"),
            expand_entry("%ProgramFiles%").join("Some App"),
            expand_entry("%ProgramData%").join("Microsoft\\Crypto"),
        ] {
            assert!(
                ensure_removable(&path).is_err(),
                "{} must be protected",
                path.display()
            );
        }
    }

    #[test]
    fn rejects_container_directories_but_allows_their_children() {
        let home = home_dir();

        assert!(ensure_removable(&home).is_err());
        assert!(ensure_removable(&home.join("AppData")).is_err());
        assert!(ensure_removable(&home.join("AppData\\Local")).is_err());
        assert!(ensure_removable(&home.join("AppData\\Local\\Packages")).is_err());
        assert!(ensure_removable(&home.join("Documents")).is_err());

        // A cache belonging to one app is fair game.
        assert!(
            ensure_removable(&home.join("AppData\\Local\\com.example.App\\Cache")).is_ok()
        );
    }

    #[test]
    fn strict_descendants_of_exempted_subtrees_are_removable() {
        let temp = expand_entry("%SystemRoot%\\Temp");

        // Files left behind in Windows' scratch areas may be cleaned …
        assert!(ensure_removable(&temp.join("stale.tmp")).is_ok());
        assert!(ensure_removable(&temp.join("sub\\stale.tmp")).is_ok());
        assert!(ensure_removable(
            &expand_entry("%SystemRoot%\\SoftwareDistribution\\Download").join("update.cab")
        )
        .is_ok());

        // … but the scratch directories themselves stay.
        assert!(ensure_removable(&temp).is_err());
        assert!(ensure_removable(&expand_entry("%SystemRoot%\\SoftwareDistribution\\Download")).is_err());

        // A sibling that merely shares the prefix is not exempted: this locks
        // in the component-level semantics of the comparison, so a future
        // refactor to a plain string-prefix check fails here.
        assert!(ensure_removable(&expand_entry("%SystemRoot%\\Tempfoo").join("x")).is_err());
    }

    #[test]
    fn protection_ignores_path_case() {
        // `C:\WINDOWS\SYSTEM32\EVIL.DLL` and `C:\Windows\System32\evil.dll` are
        // the same file, so the guard has to treat them the same way.
        let shouty = PathBuf::from(
            expand_entry("%SystemRoot%")
                .join("System32\\evil.dll")
                .to_string_lossy()
                .to_uppercase(),
        );

        assert!(
            ensure_removable(&shouty).is_err(),
            "{} must be protected",
            shouty.display()
        );
    }

    #[test]
    fn only_machine_wide_paths_need_elevation() {
        assert!(!needs_elevation(&home_dir().join("AppData\\Local\\Temp\\x")));
        assert!(needs_elevation(&expand_entry("%SystemRoot%\\Temp\\x")));
        assert!(needs_elevation(&expand_entry("%ProgramData%").join("Vendor\\App")));
        assert!(needs_elevation(&expand_entry("%ProgramFiles%").join("App\\leftover")));
    }

    #[test]
    fn standard_users_are_never_told_to_grant_a_permission() {
        assert!(has_full_disk_access());
        assert!(request_permissions().is_ok());
    }
}
