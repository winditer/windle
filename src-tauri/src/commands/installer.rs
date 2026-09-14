//! Installer Cleanup — old disk images and packages sitting in Downloads.

use std::path::{Path, PathBuf};

use serde::Serialize;

use super::CleanOutcome;
use crate::scanner::walker;
use crate::utils::fs_ops::{self, RemoveMode};
use crate::utils::{format, history, permissions, Result};

/// Installers below this size are not worth listing.
const MIN_INSTALLER_SIZE: u64 = 1_000_000;

/// How far below each root to look; installers usually sit at the top, but
/// browsers sometimes drop them in a subfolder.
const MAX_DEPTH: usize = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum InstallerKind {
    Dmg,
    Pkg,
    Zip,
    Iso,
    AppArchive,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InstallerFile {
    pub id: String,
    pub path: String,
    pub name: String,
    pub kind: InstallerKind,
    pub size: u64,
    pub created_at: Option<u64>,
    pub matched_app: Option<String>,
    pub is_redundant: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InstallerScanResult {
    pub installers: Vec<InstallerFile>,
    pub total_size: u64,
    pub redundant_size: u64,
    pub scanned_at: u64,
}

/// Folders installers tend to pile up in.
pub fn default_roots() -> Vec<std::path::PathBuf> {
    let home = permissions::home_dir();

    ["Downloads", "Desktop", "Documents"]
        .iter()
        .map(|dir| home.join(dir))
        .filter(|path| path.is_dir())
        .collect()
}

/// Find installer files, pairing them with installed apps where possible.
#[tauri::command]
pub async fn scan_installers(roots: Option<Vec<String>>) -> Result<InstallerScanResult> {
    let roots: Vec<PathBuf> = match roots {
        Some(roots) => roots
            .into_iter()
            .map(PathBuf::from)
            .filter(|path| path.is_dir())
            .collect(),
        None => default_roots(),
    };

    // Installed app names, used to flag an installer as already applied.
    let installed = installed_app_names();

    let mut installers = Vec::new();
    let mut total_size = 0u64;
    let mut redundant_size = 0u64;

    for root in roots {
        let walk = walkdir::WalkDir::new(&root)
            .max_depth(MAX_DEPTH)
            .follow_links(false)
            .into_iter();

        for entry in walk.filter_map(std::result::Result::ok) {
            let path = entry.path();

            let Ok(metadata) = entry.metadata() else {
                continue;
            };
            if !metadata.is_file() {
                continue;
            }

            let Some(kind) = classify(path) else { continue };
            if metadata.len() < MIN_INSTALLER_SIZE {
                continue;
            }
            if permissions::ensure_removable(path).is_err() {
                continue;
            }

            let name = format::file_name(path);
            let matched_app = match_installed_app(&name, &installed);
            let is_redundant = matched_app.is_some();

            total_size += metadata.len();
            if is_redundant {
                redundant_size += metadata.len();
            }

            installers.push(InstallerFile {
                id: path.to_string_lossy().into_owned(),
                path: path.to_string_lossy().into_owned(),
                name,
                kind,
                size: metadata.len(),
                created_at: metadata
                    .created()
                    .or_else(|_| metadata.modified())
                    .ok()
                    .and_then(format::epoch_millis),
                matched_app,
                is_redundant,
            });
        }
    }

    installers.sort_by(|a, b| b.size.cmp(&a.size));

    Ok(InstallerScanResult {
        installers,
        total_size,
        redundant_size,
        scanned_at: super::clean::now_millis(),
    })
}

/// Remove the selected installers, trashing them by default.
#[tauri::command]
pub async fn remove_installers(paths: Vec<String>, permanent: bool) -> Result<CleanOutcome> {
    let mode = RemoveMode::from_permanent(permanent);
    let mut outcome = CleanOutcome::default();

    for path in paths {
        let target = PathBuf::from(&path);

        // Only ever remove things that still look like installers.
        if classify(&target).is_none() {
            outcome.fail(path, "not an installer file");
            continue;
        }
        if !target.is_file() {
            outcome.fail(path, "not a file");
            continue;
        }

        match fs_ops::remove(&target, mode) {
            Ok(freed) => outcome.succeed(path, freed),
            Err(error) => outcome.fail(path, error.to_string()),
        }
    }

    history::record_outcome(history::Operation::Installers, &outcome);

    Ok(outcome)
}

/// Detach volumes that were mounted from a disk image.
#[tauri::command]
pub async fn detach_mounted_images() -> Result<Vec<String>> {
    let info = super::run_tool("hdiutil", &["info", "-plist"])?;
    let parsed = super::plist_text_to_json(&info)?;

    let mut detached = Vec::new();

    let images = parsed
        .get("images")
        .and_then(serde_json::Value::as_array)
        .cloned()
        .unwrap_or_default();

    for image in images {
        // `dev-entry` is the whole-disk node, which detaches every partition.
        let Some(device) = image
            .get("system-entities")
            .and_then(serde_json::Value::as_array)
            .and_then(|entities| {
                entities
                    .iter()
                    .filter_map(|entity| super::plist_string(entity, "dev-entry"))
                    // The shortest node is the parent disk (diskN vs diskNsM).
                    .min_by_key(String::len)
            })
        else {
            continue;
        };

        let label = super::plist_string(&image, "image-path").unwrap_or_else(|| device.clone());

        // A busy image is a normal outcome (something is still reading it), so
        // it is skipped rather than failing the whole call.
        if super::run_tool("hdiutil", &["detach", &device]).is_ok() {
            detached.push(label);
        }
    }

    Ok(detached)
}

/// Lowercased names of installed apps, without the `.app` suffix.
fn installed_app_names() -> Vec<String> {
    super::uninstall::application_roots()
        .iter()
        .filter_map(|root| std::fs::read_dir(root).ok())
        .flat_map(|entries| entries.filter_map(std::result::Result::ok))
        .map(|entry| entry.path())
        .filter(|path| format::extension(path) == "app")
        .map(|path| {
            format::file_name(&path)
                .trim_end_matches(".app")
                .to_lowercase()
        })
        .collect()
}

/// Pair `Acme-2.1.dmg` with an installed `Acme.app`.
fn match_installed_app(file_name: &str, installed: &[String]) -> Option<String> {
    let stem = installer_stem(file_name);
    if stem.is_empty() {
        return None;
    }

    installed
        .iter()
        .find(|app| {
            // Either side may carry the version, so accept a prefix match in
            // whichever direction, as long as it is not a trivially short one.
            (stem.starts_with(app.as_str()) || app.starts_with(&stem)) && app.len() >= 3
        })
        .cloned()
}

/// Strip the extension, version numbers and common decorations from an
/// installer's file name: `Acme_v2.1.4-arm64.dmg` becomes `acme`.
fn installer_stem(file_name: &str) -> String {
    let without_extension = file_name
        .rsplit_once('.')
        .map(|(stem, _)| stem)
        .unwrap_or(file_name);

    let lowered = without_extension.to_lowercase();
    // `.app.zip` and `.tar.gz` leave a second extension behind.
    let lowered = lowered
        .trim_end_matches(".app")
        .trim_end_matches(".tar")
        .to_string();

    let cleaned: String = lowered
        .chars()
        .map(|character| match character {
            '_' | '-' | '+' => ' ',
            other => other,
        })
        .collect();

    // Keep the leading words that are not versions or architectures.
    let mut words = Vec::new();
    for word in cleaned.split_whitespace() {
        let is_noise = word.chars().next().is_some_and(|c| c.is_ascii_digit())
            || word.starts_with('v') && word[1..].starts_with(|c: char| c.is_ascii_digit())
            || matches!(
                word,
                "arm64" | "x86" | "x64" | "amd64" | "universal" | "installer" | "setup" | "mac"
                    | "macos" | "osx" | "darwin"
            );

        if is_noise {
            break;
        }
        words.push(word);
    }

    words.join(" ").trim().to_string()
}

fn classify(path: &Path) -> Option<InstallerKind> {
    let name = format::file_name(path).to_lowercase();

    // `.app.zip` is an archived bundle rather than a plain archive.
    if name.ends_with(".app.zip") {
        return Some(InstallerKind::AppArchive);
    }

    match format::extension(path).as_str() {
        "dmg" | "sparseimage" | "sparsebundle" => Some(InstallerKind::Dmg),
        "pkg" | "mpkg" => Some(InstallerKind::Pkg),
        "zip" => Some(InstallerKind::Zip),
        "iso" | "cdr" => Some(InstallerKind::Iso),
        "tar" | "gz" | "xz" | "bz2" => Some(InstallerKind::AppArchive),
        _ => None,
    }
}

/// Total size of the installers sitting in the default locations, for callers
/// that only need the number.
pub fn installer_total() -> u64 {
    default_roots()
        .iter()
        .map(|root| walker::directory_size(root))
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_installer_extensions() {
        assert_eq!(classify(Path::new("/a/App.dmg")), Some(InstallerKind::Dmg));
        assert_eq!(classify(Path::new("/a/App.DMG")), Some(InstallerKind::Dmg));
        assert_eq!(classify(Path::new("/a/Tool.pkg")), Some(InstallerKind::Pkg));
        assert_eq!(
            classify(Path::new("/a/Thing.app.zip")),
            Some(InstallerKind::AppArchive)
        );
        assert_eq!(classify(Path::new("/a/Plain.zip")), Some(InstallerKind::Zip));
        assert_eq!(classify(Path::new("/a/notes.txt")), None);
        assert_eq!(classify(Path::new("/a/Photos")), None);
    }

    #[test]
    fn strips_versions_from_installer_names() {
        assert_eq!(installer_stem("Acme-2.1.4.dmg"), "acme");
        assert_eq!(installer_stem("Acme_v3.0-arm64.dmg"), "acme");
        assert_eq!(installer_stem("Visual Studio Code.app.zip"), "visual studio code");
        assert_eq!(installer_stem("Docker.dmg"), "docker");
    }

    #[test]
    fn pairs_installers_with_installed_apps() {
        let installed = vec!["docker".to_string(), "visual studio code".to_string()];

        assert_eq!(
            match_installed_app("Docker-4.2.0.dmg", &installed),
            Some("docker".to_string())
        );
        assert_eq!(
            match_installed_app("Visual Studio Code.app.zip", &installed),
            Some("visual studio code".to_string())
        );
        assert_eq!(match_installed_app("Firefox 120.dmg", &installed), None);
    }

    #[test]
    fn removal_refuses_non_installers() {
        let outcome = tauri::async_runtime::block_on(remove_installers(
            vec!["/tmp/notes.txt".to_string()],
            false,
        ))
        .expect("per-path failures are reported in the outcome");

        assert_eq!(outcome.failed_paths.len(), 1);
        assert_eq!(outcome.freed_bytes, 0);
    }
}
