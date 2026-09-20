pub mod walker;

use std::path::PathBuf;

use serde::Serialize;

/// Progress payload emitted while a scan is running. Mirrors `ProgressEvent`
/// on the frontend.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanProgress {
    /// 0–1, or `None` when the total is unknown.
    pub progress: Option<f32>,
    pub current_path: String,
    pub items_scanned: u64,
    pub bytes_found: u64,
}

impl ScanProgress {
    pub fn indeterminate(current_path: impl Into<String>, items: u64, bytes: u64) -> Self {
        Self {
            progress: None,
            current_path: current_path.into(),
            items_scanned: items,
            bytes_found: bytes,
        }
    }
}

/// Options shared by every scanner.
#[derive(Debug, Clone)]
pub struct ScanOptions {
    pub roots: Vec<PathBuf>,
    /// `None` walks the tree to the bottom.
    pub max_depth: Option<usize>,
    pub follow_symlinks: bool,
    /// Skip anything smaller than this, in bytes.
    pub min_size: u64,
    /// Directory names to skip entirely, e.g. `.git`.
    pub skip_dirs: Vec<String>,
}

impl Default for ScanOptions {
    fn default() -> Self {
        Self {
            roots: Vec::new(),
            max_depth: None,
            follow_symlinks: false,
            min_size: 0,
            // Matched against a single path component, so no separators here.
            skip_dirs: vec![
                ".git".into(),
                ".Trash".into(),
                "Mobile Documents".into(),
                // Windows: the per-volume system folders. They are protected,
                // unreadable, or both, so descending into them only produces
                // permission errors and meaningless numbers.
                "$RECYCLE.BIN".into(),
                "$SysReset".into(),
                "$WinREAgent".into(),
                "Config.Msi".into(),
                "System Volume Information".into(),
            ],
        }
    }
}

/// One filesystem entry a scanner found.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanEntry {
    pub path: PathBuf,
    pub size: u64,
    pub modified_at: Option<u64>,
    pub is_directory: bool,
}

/// Directories that are worth scanning for junk on this platform: the caches,
/// logs and crash reports the OS and its applications rebuild on demand.
pub fn default_cache_roots() -> Vec<PathBuf> {
    let home = crate::utils::permissions::home_dir();

    #[cfg(target_os = "windows")]
    let roots = vec![
        home.join("AppData/Local/Temp"),
        home.join("AppData/Local/CrashDumps"),
        home.join("AppData/Local/Microsoft/Windows/INetCache"),
        home.join("AppData/Local/Microsoft/Windows/WER"),
        crate::utils::permissions::expand("%SystemRoot%\\Temp"),
    ];

    #[cfg(not(target_os = "windows"))]
    let roots = vec![
        home.join("Library/Caches"),
        home.join("Library/Logs"),
        home.join("Library/Application Support/CrashReporter"),
        home.join("Library/Developer/Xcode/DerivedData"),
        PathBuf::from("/Library/Caches"),
        PathBuf::from("/Library/Logs"),
    ];

    roots.into_iter().filter(|path| path.exists()).collect()
}

/// Directories where a developer's projects usually live, so the purge scanner
/// has somewhere to start when the user does not name a folder.
pub fn default_project_roots() -> Vec<PathBuf> {
    let home = crate::utils::permissions::home_dir();

    #[cfg(target_os = "windows")]
    let candidates = ["Documents", "source/repos", "Projects"];

    #[cfg(not(target_os = "windows"))]
    let candidates = ["Documents"];

    candidates
        .iter()
        .map(|dir| home.join(dir))
        .filter(|path| path.is_dir())
        .collect()
}
