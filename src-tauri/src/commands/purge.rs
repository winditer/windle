//! Project Purge — reclaim space from build artifacts and dependency caches.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use rayon::prelude::*;
use serde::Serialize;
use tauri::{AppHandle, Emitter};

use super::CleanOutcome;
use crate::scanner::{self, walker, ScanProgress};
use crate::utils::fs_ops::{self, RemoveMode};
use crate::utils::{format, history, permissions, Result};

pub const PROGRESS_EVENT: &str = "purge://progress";

/// How deep below a scan root a project may sit.
const MAX_PROJECT_DEPTH: usize = 6;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ProjectKind {
    Node,
    Rust,
    Python,
    Go,
    Java,
    Xcode,
    Flutter,
    Unity,
    Unknown,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectArtifact {
    pub path: String,
    pub label: String,
    pub size: u64,
    pub file_count: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectInfo {
    pub id: String,
    pub name: String,
    pub path: String,
    pub kind: ProjectKind,
    pub artifacts: Vec<ProjectArtifact>,
    pub reclaimable_size: u64,
    pub last_modified_at: Option<u64>,
    pub has_remote: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PurgeScanResult {
    pub projects: Vec<ProjectInfo>,
    pub total_reclaimable: u64,
    pub scanned_at: u64,
    /// The roots that were actually scanned (after validation), in `~/` form.
    pub scanned_roots: Vec<String>,
}

/// Directory names that are always regenerable from source.
pub const ARTIFACT_DIRS: [&str; 13] = [
    "node_modules",
    "target",
    ".venv",
    "venv",
    "__pycache__",
    "build",
    ".build",
    "dist",
    ".next",
    ".nuxt",
    ".gradle",
    "Pods",
    "Carthage",
];

/// Artifact names that mean the same thing everywhere, so finding one is proof
/// enough on its own.
const UNAMBIGUOUS_ARTIFACTS: [&str; 8] = [
    "node_modules",
    ".venv",
    "venv",
    "__pycache__",
    ".next",
    ".nuxt",
    "Pods",
    "Carthage",
];

/// Marker files that tell us what kind of project a directory is.
pub const PROJECT_MARKERS: [(&str, ProjectKind); 13] = [
    ("package.json", ProjectKind::Node),
    ("Cargo.toml", ProjectKind::Rust),
    ("pyproject.toml", ProjectKind::Python),
    ("requirements.txt", ProjectKind::Python),
    ("setup.py", ProjectKind::Python),
    ("go.mod", ProjectKind::Go),
    ("pom.xml", ProjectKind::Java),
    ("build.gradle", ProjectKind::Java),
    ("build.gradle.kts", ProjectKind::Java),
    ("Podfile", ProjectKind::Xcode),
    ("Package.swift", ProjectKind::Xcode),
    ("pubspec.yaml", ProjectKind::Flutter),
    ("ProjectSettings", ProjectKind::Unity),
];

/// Look for reclaimable artifacts under the given roots.
#[tauri::command]
pub async fn scan_projects(app: AppHandle, roots: Option<Vec<String>>) -> Result<PurgeScanResult> {
    let roots: Vec<PathBuf> = match roots {
        Some(roots) => roots
            .into_iter()
            .map(|raw| expand_tilde(&raw))
            .filter(|path| path.is_dir())
            .collect(),
        None => scanner::default_project_roots(),
    };

    // Report the validated roots back to the UI in `~/` form.
    let scanned_roots: Vec<String> = roots.iter().map(|r| format::tilde(r)).collect();

    // Group artifacts by the project that owns them.
    let mut grouped: BTreeMap<PathBuf, Vec<PathBuf>> = BTreeMap::new();
    let mut scanned = 0u64;

    for root in &roots {
        for artifact in walker::find_dirs_named(root, &ARTIFACT_DIRS, MAX_PROJECT_DEPTH) {
            let Some(project) = owning_project(&artifact) else {
                // An ambiguous `build`/`dist` with no project around it stays
                // untouched — we cannot prove it is regenerable.
                continue;
            };

            scanned += 1;
            grouped.entry(project).or_default().push(artifact);
        }

        let _ = app.emit(
            PROGRESS_EVENT,
            ScanProgress::indeterminate(root.to_string_lossy().into_owned(), scanned, 0),
        );
    }

    // Sizing dominates the runtime, so every project is measured in parallel.
    let mut projects: Vec<ProjectInfo> = grouped
        .into_par_iter()
        .map(|(project, artifacts)| describe_project(project, artifacts))
        .collect();

    projects.sort_by(|a, b| b.reclaimable_size.cmp(&a.reclaimable_size));

    let total_reclaimable = projects
        .iter()
        .map(|project| project.reclaimable_size)
        .sum();

    let _ = app.emit(
        PROGRESS_EVENT,
        ScanProgress {
            progress: Some(1.0),
            current_path: String::new(),
            items_scanned: scanned,
            bytes_found: total_reclaimable,
        },
    );

    Ok(PurgeScanResult {
        projects,
        total_reclaimable,
        scanned_at: super::clean::now_millis(),
        scanned_roots,
    })
}

/// Expand a leading `~` against the current home directory. Paths without `~`
/// are returned unchanged.
fn expand_tilde(raw: &str) -> PathBuf {
    match raw.strip_prefix('~') {
        Some("") => permissions::home_dir(),
        Some(rest) => permissions::home_dir().join(rest.trim_start_matches('/')),
        None => PathBuf::from(raw),
    }
}

/// Delete the selected artifact directories.
#[tauri::command]
pub async fn purge_artifacts(paths: Vec<String>) -> Result<CleanOutcome> {
    let mut outcome = CleanOutcome::default();

    for path in paths {
        let target = PathBuf::from(&path);

        // Re-check rather than trusting the request: the frontend's list may be
        // stale, and this is a permanent delete.
        if !is_purgeable(&target) {
            outcome.fail(path, "not a recognised build artifact directory");
            continue;
        }

        // Artifacts are rebuilt from source, so they skip the trash — moving
        // gigabytes of `node_modules` there would just relocate the problem.
        match fs_ops::remove(&target, RemoveMode::Permanent) {
            Ok(freed) => outcome.succeed(path, freed),
            Err(error) => outcome.fail(path, error.to_string()),
        }
    }

    history::record_outcome(history::Operation::Purge, &outcome);

    Ok(outcome)
}

/// Measure a project and its artifacts.
fn describe_project(project: PathBuf, artifact_paths: Vec<PathBuf>) -> ProjectInfo {
    let stats: Vec<(u64, u64)> = artifact_paths
        .par_iter()
        .map(|path| walker::directory_stats(path))
        .collect();

    let artifacts: Vec<ProjectArtifact> = artifact_paths
        .iter()
        .zip(&stats)
        .map(|(path, (size, file_count))| ProjectArtifact {
            path: path.to_string_lossy().into_owned(),
            label: format::file_name(path),
            size: *size,
            file_count: *file_count,
        })
        .collect();

    let reclaimable_size = stats.iter().map(|(size, _)| size).sum();

    ProjectInfo {
        id: project.to_string_lossy().into_owned(),
        name: format::file_name(&project),
        kind: project_kind(&project),
        // A project whose source is pushed somewhere is trivial to restore.
        has_remote: has_git_remote(&project),
        last_modified_at: last_source_change(&project),
        path: project.to_string_lossy().into_owned(),
        artifacts,
        reclaimable_size,
    }
}

/// The nearest ancestor of `artifact` that looks like a project, when the
/// artifact's name needs that proof.
fn owning_project(artifact: &Path) -> Option<PathBuf> {
    let parent = artifact.parent()?;
    let name = format::file_name(artifact);

    if UNAMBIGUOUS_ARTIFACTS.contains(&name.as_str()) {
        return Some(parent.to_path_buf());
    }

    // `target` is only a build directory when Cargo owns it.
    if name == "target" {
        return parent.join("Cargo.toml").is_file().then(|| parent.to_path_buf());
    }

    // `build`, `dist`, `.build` and `.gradle` are common words; require a
    // marker file next to them before offering to delete anything.
    has_marker(parent).then(|| parent.to_path_buf())
}

/// Whether `directory` holds any recognised project marker.
fn has_marker(directory: &Path) -> bool {
    PROJECT_MARKERS
        .iter()
        .any(|(marker, _)| directory.join(marker).exists())
}

fn project_kind(directory: &Path) -> ProjectKind {
    PROJECT_MARKERS
        .iter()
        .find(|(marker, _)| directory.join(marker).exists())
        .map(|(_, kind)| *kind)
        .unwrap_or(ProjectKind::Unknown)
}

/// True when the directory is a git repo with at least one remote.
fn has_git_remote(directory: &Path) -> bool {
    let config = directory.join(".git/config");

    std::fs::read_to_string(config)
        .map(|contents| contents.contains("[remote "))
        .unwrap_or(false)
}

/// When the project's own files (not its artifacts) last changed, so the UI can
/// point at the projects nobody has touched in months.
fn last_source_change(directory: &Path) -> Option<u64> {
    let entries = std::fs::read_dir(directory).ok()?;

    entries
        .filter_map(std::result::Result::ok)
        .filter(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            !ARTIFACT_DIRS.contains(&name.as_str())
        })
        .filter_map(|entry| entry.metadata().ok())
        .filter_map(|meta| meta.modified().ok())
        .max()
        .and_then(format::epoch_millis)
}

/// Guard against purging anything that is not a known artifact directory that
/// still belongs to a real project.
fn is_purgeable(path: &Path) -> bool {
    if !path.is_dir() {
        return false;
    }
    if permissions::ensure_removable(path).is_err() {
        return false;
    }

    let is_artifact_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| ARTIFACT_DIRS.contains(&name));

    is_artifact_name && owning_project(path).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        crate::utils::test_support::scratch(&format!("purge-{name}"))
    }

    #[test]
    fn node_modules_needs_no_marker_file() {
        let root = scratch("node");
        let artifact = root.join("anything/node_modules");
        std::fs::create_dir_all(&artifact).unwrap();

        assert_eq!(owning_project(&artifact), Some(root.join("anything")));
        assert!(is_purgeable(&artifact));

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn an_ambiguous_build_directory_is_left_alone_without_a_project() {
        let root = scratch("ambiguous");
        let artifact = root.join("holiday-photos/build");
        std::fs::create_dir_all(&artifact).unwrap();

        assert_eq!(owning_project(&artifact), None);
        assert!(
            !is_purgeable(&artifact),
            "a bare build/ folder must not be purgeable"
        );

        // Adding a marker makes it a real project artifact.
        std::fs::write(root.join("holiday-photos/package.json"), b"{}").unwrap();
        assert!(is_purgeable(&artifact));

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn target_is_only_an_artifact_for_cargo_projects() {
        let root = scratch("target");
        let artifact = root.join("thing/target");
        std::fs::create_dir_all(&artifact).unwrap();

        assert_eq!(owning_project(&artifact), None);

        std::fs::write(root.join("thing/Cargo.toml"), b"[package]").unwrap();
        assert_eq!(owning_project(&artifact), Some(root.join("thing")));

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn detects_the_project_kind_from_its_marker() {
        let root = scratch("kind");
        std::fs::create_dir_all(&root).unwrap();

        assert_eq!(project_kind(&root), ProjectKind::Unknown);

        std::fs::write(root.join("Cargo.toml"), b"[package]").unwrap();
        assert_eq!(project_kind(&root), ProjectKind::Rust);

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn reads_the_git_remote_flag() {
        let root = scratch("git");
        std::fs::create_dir_all(root.join(".git")).unwrap();

        assert!(!has_git_remote(&root));

        std::fs::write(
            root.join(".git/config"),
            b"[remote \"origin\"]\n\turl = git@example.com:acme/thing.git\n",
        )
        .unwrap();
        assert!(has_git_remote(&root));

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn refuses_to_purge_a_protected_or_unknown_directory() {
        assert!(!is_purgeable(Path::new("/System")));
        assert!(!is_purgeable(Path::new("/Applications")));
        assert!(!is_purgeable(Path::new("/tmp/does-not-exist")));
    }

    #[test]
    fn purge_rejects_paths_that_are_not_artifacts() {
        let root = scratch("reject");
        std::fs::create_dir_all(&root).unwrap();

        let outcome = tauri::async_runtime::block_on(purge_artifacts(vec![root
            .to_string_lossy()
            .into_owned()]))
        .expect("purge should report per-path failures, not error out");

        assert!(outcome.removed_paths.is_empty());
        assert_eq!(outcome.failed_paths.len(), 1);
        assert!(root.exists(), "the directory must survive");

        std::fs::remove_dir_all(&root).ok();
    }
}
