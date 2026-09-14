//! Disk Analyzer — where the space actually went.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::Serialize;
use sysinfo::Disks;
use tauri::{AppHandle, Emitter};

use crate::scanner::{walker, ScanOptions, ScanProgress};
use crate::utils::{format, WindleError, Result};

pub const PROGRESS_EVENT: &str = "analyze://progress";

/// Cap on how many volumes deep a single `analyze_path` call will build nodes.
const MAX_TREE_DEPTH: usize = 12;

/// Largest files kept alongside the tree by `analyze_path`.
const LARGEST_FILE_COUNT: usize = 25;

/// Extensions below this share of the total are folded into "Other".
const TYPE_BREAKDOWN_LIMIT: usize = 12;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiskUsage {
    pub mount_point: String,
    pub name: String,
    pub file_system: String,
    pub total_bytes: u64,
    pub available_bytes: u64,
    pub used_bytes: u64,
    pub is_removable: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TreeNode {
    pub name: String,
    pub path: String,
    pub size: u64,
    pub is_directory: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub children: Option<Vec<TreeNode>>,
    /// Fraction of the parent's size, 0–1.
    pub share: f32,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileTypeBreakdown {
    pub extension: String,
    pub label: String,
    pub size: u64,
    pub file_count: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AnalyzeResult {
    pub root: TreeNode,
    pub largest_files: Vec<TreeNode>,
    pub by_type: Vec<FileTypeBreakdown>,
    pub scanned_at: u64,
    pub duration_ms: u64,
}

/// Everything one walk accumulates, before it is shaped into an `AnalyzeResult`.
#[derive(Default)]
struct Analysis {
    /// Directory path → total bytes below it, for directories within the depth
    /// limit. Deeper files still contribute to their ancestors' totals.
    directory_sizes: HashMap<PathBuf, u64>,
    /// Extension → (bytes, file count).
    by_extension: HashMap<String, (u64, u64)>,
    total_bytes: u64,
}

/// Capacity and usage for every mounted volume.
#[tauri::command]
pub async fn list_volumes() -> Result<Vec<DiskUsage>> {
    Ok(volumes())
}

/// Build a size tree `depth` levels deep, rooted at `path`.
#[tauri::command]
pub async fn analyze_path(app: AppHandle, path: String, depth: usize) -> Result<AnalyzeResult> {
    let started = std::time::Instant::now();
    let root_path = crate::utils::permissions::expand_tilde(&path);

    if !root_path.exists() {
        return Err(WindleError::NotFound(path));
    }
    if !root_path.is_dir() {
        return Err(WindleError::Command {
            command: "analyze_path".into(),
            message: format!("{path} is not a directory"),
        });
    }

    let depth = depth.clamp(1, MAX_TREE_DEPTH);
    let options = ScanOptions {
        // The analyzer reports on what is there, so nothing is skipped and no
        // symlink is followed (that would double-count and risk cycles).
        skip_dirs: Vec::new(),
        ..Default::default()
    };

    // One pass feeds the directory totals, the type breakdown and the
    // largest-file heap.
    let cancel = crate::scanner::walker::CancelToken::default();
    let mut analysis = Analysis::default();
    let mut largest = LargestFiles::new(LARGEST_FILE_COUNT);

    walker::for_each_file(
        &root_path,
        &options,
        &cancel,
        |file, metadata, _file_depth| {
            let size = metadata.len();
            analysis.total_bytes += size;
            analysis.record_extension(file, size);
            analysis.attribute(&root_path, file, size);
            largest.offer(file, size);
        },
        |current, items, bytes| {
            let _ = app.emit(
                PROGRESS_EVENT,
                ScanProgress::indeterminate(current.to_string_lossy().into_owned(), items, bytes),
            );
        },
    );

    let root = analysis.build_node(&root_path, depth, 0, analysis.total_bytes);

    let _ = app.emit(
        PROGRESS_EVENT,
        ScanProgress {
            progress: Some(1.0),
            current_path: path,
            items_scanned: analysis.directory_sizes.len() as u64,
            bytes_found: analysis.total_bytes,
        },
    );

    Ok(AnalyzeResult {
        root,
        largest_files: largest.into_nodes(analysis.total_bytes),
        by_type: analysis.into_breakdown(),
        scanned_at: super::clean::now_millis(),
        duration_ms: started.elapsed().as_millis() as u64,
    })
}

/// Expand one directory node on demand.
#[tauri::command]
pub async fn expand_node(path: String) -> Result<Vec<TreeNode>> {
    let parent = crate::utils::permissions::expand_tilde(&path);

    let entries = std::fs::read_dir(&parent)
        .map_err(|error| crate::utils::permissions::classify_io_error(&parent, &error))?;

    // Collect first, then size the children in parallel — one `node_modules`
    // among them would otherwise dominate the wall clock.
    let paths: Vec<PathBuf> = entries
        .filter_map(std::result::Result::ok)
        .map(|entry| entry.path())
        .collect();
    let sizes = walker::sizes_of(&paths);

    let total: u64 = sizes.iter().sum();

    // Use `symlink_metadata` (lstat) so symlinks are identified as such and
    // filtered out — `child.is_dir()` follows the link and would mislabel a
    // symlink-to-directory as a real directory with size 0.
    let mut children: Vec<TreeNode> = paths
        .into_iter()
        .zip(sizes)
        .filter_map(|(child, size)| {
            let meta = std::fs::symlink_metadata(&child).ok()?;
            if meta.is_symlink() {
                return None;
            }
            Some(TreeNode {
                name: format::file_name(&child),
                is_directory: meta.is_dir(),
                path: child.to_string_lossy().into_owned(),
                size,
                children: None,
                share: format::ratio(size, total),
            })
        })
        .collect();

    children.sort_by(|a, b| b.size.cmp(&a.size));
    Ok(children)
}

/// The `limit` biggest files below `path`.
#[tauri::command]
pub async fn find_largest_files(app: AppHandle, path: String, limit: usize) -> Result<Vec<TreeNode>> {
    let root = crate::utils::permissions::expand_tilde(&path);
    if !root.exists() {
        return Err(WindleError::NotFound(path));
    }

    let options = ScanOptions {
        skip_dirs: Vec::new(),
        // Anything under a megabyte is noise in a "largest files" list, and
        // filtering early keeps the heap small.
        min_size: 1_000_000,
        ..Default::default()
    };

    let files = walker::largest_files(
        &root,
        limit.min(1_000),
        &options,
        &crate::scanner::walker::CancelToken::default(),
        |current, items, bytes| {
            let _ = app.emit(
                PROGRESS_EVENT,
                ScanProgress::indeterminate(current.to_string_lossy().into_owned(), items, bytes),
            );
        },
    );

    let biggest = files.first().map(|entry| entry.size).unwrap_or(0);

    Ok(files
        .into_iter()
        .map(|entry| TreeNode {
            name: format::file_name(&entry.path),
            path: entry.path.to_string_lossy().into_owned(),
            size: entry.size,
            is_directory: false,
            children: None,
            // Relative to the biggest file, which is what the bar chart wants.
            share: format::ratio(entry.size, biggest),
        })
        .collect())
}

impl Analysis {
    fn record_extension(&mut self, file: &Path, size: u64) {
        let extension = format::extension(file);
        let key = if extension.is_empty() {
            String::new()
        } else {
            extension
        };

        let entry = self.by_extension.entry(key).or_insert((0, 0));
        entry.0 += size;
        entry.1 += 1;
    }

    /// Add `size` to every ancestor of `file` up to and including the scan
    /// root, so a directory's total covers files nested below the depth limit.
    fn attribute(&mut self, root: &Path, file: &Path, size: u64) {
        let mut current = file.parent();

        while let Some(directory) = current {
            *self
                .directory_sizes
                .entry(directory.to_path_buf())
                .or_insert(0) += size;

            if directory == root {
                break;
            }

            current = directory.parent();
        }
    }

    /// Turn the accumulated totals into nodes, descending until `depth`.
    fn build_node(&self, path: &Path, depth: usize, level: usize, parent_size: u64) -> TreeNode {
        let size = self.directory_sizes.get(path).copied().unwrap_or(0);

        let children = if level < depth {
            let mut nodes: Vec<TreeNode> = std::fs::read_dir(path)
                .map(|entries| {
                    entries
                        .filter_map(std::result::Result::ok)
                        .filter_map(|entry| {
                            let child = entry.path();
                            let metadata = entry.metadata().ok()?;

                            // Symlinks are listed but never followed, so they
                            // cannot inflate a directory's total.
                            if metadata.is_symlink() {
                                return None;
                            }

                            Some(if metadata.is_dir() {
                                self.build_node(&child, depth, level + 1, size)
                            } else {
                                TreeNode {
                                    name: format::file_name(&child),
                                    path: child.to_string_lossy().into_owned(),
                                    size: metadata.len(),
                                    is_directory: false,
                                    children: None,
                                    share: format::ratio(metadata.len(), size),
                                }
                            })
                        })
                        .collect()
                })
                .unwrap_or_default();

            nodes.sort_by(|a, b| b.size.cmp(&a.size));
            Some(nodes)
        } else {
            None
        };

        TreeNode {
            name: format::file_name(path),
            path: path.to_string_lossy().into_owned(),
            size,
            is_directory: true,
            children,
            share: if level == 0 {
                1.0
            } else {
                format::ratio(size, parent_size)
            },
        }
    }

    fn into_breakdown(self) -> Vec<FileTypeBreakdown> {
        let mut types: Vec<FileTypeBreakdown> = self
            .by_extension
            .into_iter()
            .map(|(extension, (size, file_count))| FileTypeBreakdown {
                label: label_for_extension(&extension),
                extension,
                size,
                file_count,
            })
            .collect();

        types.sort_by(|a, b| b.size.cmp(&a.size));

        // Fold the long tail into a single "Other" row.
        if types.len() > TYPE_BREAKDOWN_LIMIT {
            let tail: Vec<FileTypeBreakdown> = types.split_off(TYPE_BREAKDOWN_LIMIT);
            let size = tail.iter().map(|entry| entry.size).sum();
            let file_count = tail.iter().map(|entry| entry.file_count).sum();

            if size > 0 {
                types.push(FileTypeBreakdown {
                    extension: String::new(),
                    label: "Other".into(),
                    size,
                    file_count,
                });
            }
        }

        types
    }
}

/// Bounded min-heap of the biggest files seen so far.
struct LargestFiles {
    limit: usize,
    heap: std::collections::BinaryHeap<std::cmp::Reverse<(u64, PathBuf)>>,
}

impl LargestFiles {
    fn new(limit: usize) -> Self {
        Self {
            limit,
            heap: std::collections::BinaryHeap::new(),
        }
    }

    fn offer(&mut self, path: &Path, size: u64) {
        if self.limit == 0 {
            return;
        }

        if self.heap.len() < self.limit {
            self.heap.push(std::cmp::Reverse((size, path.to_path_buf())));
        } else if self
            .heap
            .peek()
            .is_some_and(|std::cmp::Reverse((smallest, _))| size > *smallest)
        {
            self.heap.pop();
            self.heap.push(std::cmp::Reverse((size, path.to_path_buf())));
        }
    }

    fn into_nodes(self, total: u64) -> Vec<TreeNode> {
        let mut nodes: Vec<TreeNode> = self
            .heap
            .into_iter()
            .map(|std::cmp::Reverse((size, path))| TreeNode {
                name: format::file_name(&path),
                path: path.to_string_lossy().into_owned(),
                size,
                is_directory: false,
                children: None,
                share: format::ratio(size, total),
            })
            .collect();

        nodes.sort_by(|a, b| b.size.cmp(&a.size));
        nodes
    }
}

/// Human label for an extension group.
fn label_for_extension(extension: &str) -> String {
    let label = match extension {
        "" => "No extension",
        "jpg" | "jpeg" | "png" | "gif" | "heic" | "webp" | "tiff" | "bmp" | "svg" => "Images",
        "mp4" | "mov" | "avi" | "mkv" | "m4v" | "webm" => "Video",
        "mp3" | "aac" | "flac" | "wav" | "m4a" | "aiff" => "Audio",
        "pdf" | "doc" | "docx" | "pages" | "txt" | "rtf" | "md" => "Documents",
        "xls" | "xlsx" | "numbers" | "csv" => "Spreadsheets",
        "zip" | "gz" | "bz2" | "xz" | "tar" | "7z" | "rar" => "Archives",
        "dmg" | "pkg" | "iso" => "Disk images",
        "app" | "framework" | "dylib" | "so" | "o" | "a" => "Code & binaries",
        "rs" | "ts" | "tsx" | "js" | "jsx" | "py" | "go" | "swift" | "c" | "h" | "cpp" | "java" => {
            "Source code"
        }
        "sqlite" | "db" | "realm" => "Databases",
        "log" => "Logs",
        other => return format!(".{other}"),
    };

    label.to_string()
}

/// Usage for every volume `sysinfo` can see.
pub fn volumes() -> Vec<DiskUsage> {
    Disks::new_with_refreshed_list()
        .list()
        .iter()
        .map(|disk| {
            let total = disk.total_space();
            let available = disk.available_space();

            DiskUsage {
                mount_point: disk.mount_point().to_string_lossy().into_owned(),
                name: disk.name().to_string_lossy().into_owned(),
                file_system: disk.file_system().to_string_lossy().into_owned(),
                total_bytes: total,
                available_bytes: available,
                used_bytes: total.saturating_sub(available),
                is_removable: disk.is_removable(),
            }
        })
        .collect()
}

/// The volume the system booted from, used by the dashboard gauge.
pub fn boot_volume() -> Option<DiskUsage> {
    let mut disks = volumes();
    disks.sort_by_key(|disk| disk.mount_point != "/");
    disks
        .into_iter()
        .find(|disk| Path::new(&disk.mount_point).exists())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Tests run in parallel, so each one gets its own tree.
    fn fixture(name: &str) -> PathBuf {
        let root =
            PathBuf::from("/tmp").join(format!("windle-analyze-{name}-{}", std::process::id()));
        std::fs::remove_dir_all(&root).ok();
        std::fs::create_dir_all(root.join("a/deep")).unwrap();
        std::fs::create_dir_all(root.join("b")).unwrap();
        std::fs::write(root.join("a/one.txt"), vec![0u8; 1_000]).unwrap();
        std::fs::write(root.join("a/deep/two.log"), vec![0u8; 2_000]).unwrap();
        std::fs::write(root.join("b/three.txt"), vec![0u8; 400]).unwrap();
        root
    }

    /// Run the same accumulation `analyze_path` does, without needing an
    /// `AppHandle` to emit progress to.
    fn analyse(root: &Path, depth: usize) -> (Analysis, TreeNode) {
        let mut analysis = Analysis::default();

        walker::for_each_file(
            root,
            &ScanOptions {
                skip_dirs: Vec::new(),
                ..Default::default()
            },
            &crate::scanner::walker::CancelToken::default(),
            |file, metadata, _| {
                let size = metadata.len();
                analysis.total_bytes += size;
                analysis.record_extension(file, size);
                analysis.attribute(root, file, size);
            },
            |_, _, _| {},
        );

        let node = analysis.build_node(root, depth, 0, analysis.total_bytes);
        (analysis, node)
    }

    #[test]
    fn directory_totals_include_files_below_the_depth_limit() {
        let root = fixture("totals");
        let (_, tree) = analyse(&root, 1);

        assert_eq!(tree.size, 3_400);
        assert_eq!(tree.share, 1.0);

        let children = tree.children.expect("depth 1 must have children");
        let a = children.iter().find(|node| node.name == "a").unwrap();

        // `a/deep/two.log` sits below the depth limit but still counts.
        assert_eq!(a.size, 3_000);
        // Children stop at the requested depth.
        assert!(a.children.is_none());

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn groups_files_by_type() {
        let root = fixture("types");
        let (analysis, _) = analyse(&root, 2);
        let breakdown = analysis.into_breakdown();

        let documents = breakdown
            .iter()
            .find(|entry| entry.label == "Documents")
            .expect("txt files are documents");

        assert_eq!(documents.size, 1_400);
        assert_eq!(documents.file_count, 2);

        let logs = breakdown.iter().find(|entry| entry.label == "Logs").unwrap();
        assert_eq!(logs.size, 2_000);

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn largest_files_are_capped_and_ordered() {
        let mut largest = LargestFiles::new(2);
        largest.offer(Path::new("/tmp/small"), 10);
        largest.offer(Path::new("/tmp/big"), 900);
        largest.offer(Path::new("/tmp/medium"), 500);

        let nodes = largest.into_nodes(1_410);

        assert_eq!(nodes.len(), 2);
        assert_eq!(nodes[0].name, "big");
        assert_eq!(nodes[1].name, "medium");
    }

    #[test]
    fn boot_volume_is_discoverable() {
        let boot = boot_volume().expect("a mounted boot volume");
        assert!(boot.total_bytes > 0);
    }

    /// Fixture with a symlink to a file and a symlink to a directory, so we
    /// can verify the analyzer never counts or follows them.
    fn symlink_fixture(name: &str) -> PathBuf {
        let root =
            PathBuf::from("/tmp").join(format!("windle-analyze-symlink-{name}-{}", std::process::id()));
        std::fs::remove_dir_all(&root).ok();
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::fs::write(root.join("sub/data.bin"), vec![0u8; 5_000]).unwrap();
        std::fs::write(root.join("plain.bin"), vec![0u8; 3_000]).unwrap();

        std::os::unix::fs::symlink(
            root.join("plain.bin"),
            root.join("link_to_file"),
        )
        .unwrap();
        std::os::unix::fs::symlink(
            root.join("sub"),
            root.join("link_to_dir"),
        )
        .unwrap();

        root
    }

    #[test]
    fn analysis_excludes_symlinks_from_totals() {
        let root = symlink_fixture("totals");
        let (analysis, tree) = analyse(&root, 2);

        // Only two real files: plain.bin (3_000) + sub/data.bin (5_000).
        assert_eq!(analysis.total_bytes, 8_000);
        assert_eq!(tree.size, 8_000);

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn build_node_excludes_symlinks_from_children() {
        let root = symlink_fixture("buildnode");
        let (_, tree) = analyse(&root, 1);

        let children = tree.children.expect("depth 1 must have children");
        let names: Vec<&str> = children.iter().map(|c| c.name.as_str()).collect();

        // Symlinks must not appear as children.
        assert!(
            !names.contains(&"link_to_file"),
            "file symlink must be filtered out"
        );
        assert!(
            !names.contains(&"link_to_dir"),
            "dir symlink must be filtered out"
        );

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn expand_node_filters_symlinks() {
        let root = symlink_fixture("expand");

        let children = tauri::async_runtime::block_on(expand_node(
            root.to_string_lossy().into_owned(),
        ))
        .expect("expand_node should succeed");

        let names: Vec<&str> = children.iter().map(|c| c.name.as_str()).collect();

        assert!(
            !names.contains(&"link_to_file"),
            "file symlink must not appear in expanded children"
        );
        assert!(
            !names.contains(&"link_to_dir"),
            "dir symlink must not appear in expanded children"
        );

        // The real directory and file must still be present.
        assert!(names.contains(&"sub"), "real directory must be listed");
        assert!(names.contains(&"plain.bin"), "real file must be listed");

        std::fs::remove_dir_all(&root).ok();
    }
}
