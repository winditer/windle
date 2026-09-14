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
            skip_dirs: vec![".git".into(), ".Trash".into(), "Mobile Documents".into()],
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

/// Directories that are worth scanning for junk on every Mac.
pub fn default_cache_roots() -> Vec<PathBuf> {
    let home = crate::utils::permissions::home_dir();

    vec![
        home.join("Library/Caches"),
        home.join("Library/Logs"),
        home.join("Library/Application Support/CrashReporter"),
        home.join("Library/Developer/Xcode/DerivedData"),
        PathBuf::from("/Library/Caches"),
        PathBuf::from("/Library/Logs"),
    ]
    .into_iter()
    .filter(|path| path.exists())
    .collect()
}

/// Directories developers usually keep their projects in.
pub fn default_project_roots() -> Vec<PathBuf> {
    let home = crate::utils::permissions::home_dir();

    ["Documents"]
        .iter()
        .map(|dir| home.join(dir))
        .filter(|path| path.is_dir())
        .collect()
}
