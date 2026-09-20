//! Filesystem traversal built on `walkdir`, parallelised with `rayon` where
//! the work is worth splitting up.

use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use rayon::prelude::*;
use walkdir::{DirEntry, WalkDir};

use super::{ScanEntry, ScanOptions};
use crate::utils::format;

/// Shared cancellation flag handed to long-running scans.
#[derive(Debug, Clone, Default)]
pub struct CancelToken(Arc<AtomicBool>);

impl CancelToken {
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Relaxed);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }

    pub fn reset(&self) {
        self.0.store(false, Ordering::Relaxed);
    }
}

/// Running totals a scan reports back through its progress callback.
#[derive(Debug, Default)]
pub struct ScanCounters {
    pub items: AtomicU64,
    pub bytes: AtomicU64,
}

/// What a completed (or cancelled) walk saw.
#[derive(Debug, Default, Clone, Copy)]
pub struct WalkSummary {
    pub files: u64,
    pub directories: u64,
    pub bytes: u64,
    /// Entries we could not stat — almost always a permissions problem.
    pub unreadable: u64,
    pub cancelled: bool,
}

/// Emit a progress callback roughly this often, to keep the IPC channel quiet.
const PROGRESS_INTERVAL: u64 = 512;

fn is_hidden(entry: &DirEntry) -> bool {
    entry
        .file_name()
        .to_str()
        .is_some_and(|name| name.starts_with('.') && name != ".")
}

fn should_skip(entry: &DirEntry, options: &ScanOptions) -> bool {
    let name = entry.file_name().to_string_lossy();
    options.skip_dirs.iter().any(|skip| skip == name.as_ref())
}

/// Build a configured walker for one root.
fn walker(root: &Path, options: &ScanOptions) -> WalkDir {
    let mut walk = WalkDir::new(root).follow_links(options.follow_symlinks);

    if let Some(depth) = options.max_depth {
        walk = walk.max_depth(depth);
    }

    walk
}

/// Total size of everything below `path`. Unreadable entries are skipped
/// rather than failing the whole scan.
pub fn directory_size(path: &Path) -> u64 {
    WalkDir::new(path)
        .follow_links(false)
        .into_iter()
        .filter_map(std::result::Result::ok)
        .filter(|entry| entry.file_type().is_file())
        .filter_map(|entry| entry.metadata().ok())
        .map(|meta| meta.len())
        .sum()
}

/// Size and file count in one pass, for callers that show both.
pub fn directory_stats(path: &Path) -> (u64, u64) {
    WalkDir::new(path)
        .follow_links(false)
        .into_iter()
        .filter_map(std::result::Result::ok)
        .filter(|entry| entry.file_type().is_file())
        .filter_map(|entry| entry.metadata().ok())
        .fold((0, 0), |(bytes, count), meta| {
            (bytes + meta.len(), count + 1)
        })
}

/// [`directory_size`], but gives up as soon as `cancel` is tripped.
pub fn directory_size_cancellable(path: &Path, cancel: &CancelToken) -> u64 {
    let mut total = 0;

    for (seen, entry) in WalkDir::new(path)
        .follow_links(false)
        .into_iter()
        .enumerate()
    {
        // Checking every 256th entry keeps the atomic load off the hot path.
        if seen % 256 == 0 && cancel.is_cancelled() {
            break;
        }

        let Ok(entry) = entry else { continue };
        if !entry.file_type().is_file() {
            continue;
        }
        if let Ok(meta) = entry.metadata() {
            total += meta.len();
        }
    }

    total
}

/// Same as [`directory_size`], but splits the top-level children across the
/// rayon pool — worth it for large trees like `node_modules`.
pub fn directory_size_parallel(path: &Path) -> u64 {
    let children: Vec<PathBuf> = match std::fs::read_dir(path) {
        Ok(entries) => entries
            .filter_map(std::result::Result::ok)
            .map(|entry| entry.path())
            .collect(),
        Err(_) => return 0,
    };

    // `size_of_any` uses `symlink_metadata`, so symlinks contribute 0 and are
    // never followed — preventing double-counting when a link targets a path
    // that is already in the tree.
    children
        .par_iter()
        .map(|child| size_of_any(child))
        .sum()
}

/// Size every path in `paths` in parallel, preserving the input order. Used
/// wherever we have a known set of directories to weigh, such as app bundles
/// or build artifacts.
pub fn sizes_of(paths: &[PathBuf]) -> Vec<u64> {
    paths.par_iter().map(|path| size_of_any(path)).collect()
}

/// Size of a file or a whole directory, whichever `path` turns out to be.
pub fn size_of_any(path: &Path) -> u64 {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.is_dir() => directory_size(path),
        // A symlink's own size is meaningless and its target is somebody
        // else's disk usage, so it contributes nothing.
        Ok(meta) if meta.is_symlink() => 0,
        Ok(meta) => meta.len(),
        Err(_) => 0,
    }
}

/// Walk every file below `root`, handing each one to `visit`. Directories are
/// counted but not passed on. Returns early when `cancel` is tripped, and
/// reports progress every [`PROGRESS_INTERVAL`] entries.
///
/// This is the single traversal primitive the analyzer builds on: one pass
/// feeds the size tree, the largest-file heap and the per-type breakdown.
pub fn for_each_file<V, P>(
    root: &Path,
    options: &ScanOptions,
    cancel: &CancelToken,
    mut visit: V,
    mut on_progress: P,
) -> WalkSummary
where
    V: FnMut(&Path, &std::fs::Metadata, usize),
    P: FnMut(&Path, u64, u64),
{
    let mut summary = WalkSummary::default();

    let entries = walker(root, options)
        .into_iter()
        .filter_entry(|entry| entry.depth() == 0 || !should_skip(entry, options));

    for entry in entries {
        if cancel.is_cancelled() {
            summary.cancelled = true;
            break;
        }

        // An `Err` here is an unreadable directory or a broken link; note it
        // and keep walking rather than failing the whole scan.
        let Ok(entry) = entry else {
            summary.unreadable += 1;
            continue;
        };

        let Ok(metadata) = entry.metadata() else {
            summary.unreadable += 1;
            continue;
        };

        if metadata.is_dir() {
            summary.directories += 1;
        } else if metadata.is_file() {
            summary.files += 1;
            summary.bytes += metadata.len();

            if metadata.len() >= options.min_size {
                visit(entry.path(), &metadata, entry.depth());
            }
        }
        // Symlinks and other special files (fifos, sockets, devices) are
        // skipped: a symlink's own size is meaningless, and following it
        // would risk double-counting or traversal cycles.

        let seen = summary.files + summary.directories;
        if seen % PROGRESS_INTERVAL == 0 {
            on_progress(entry.path(), seen, summary.bytes);
        }
    }

    summary
}

/// The `limit` largest files below `root`.
///
/// Keeps a bounded min-heap so memory stays proportional to `limit`, not to the
/// number of files walked. Returned biggest-first.
pub fn largest_files<P>(
    root: &Path,
    limit: usize,
    options: &ScanOptions,
    cancel: &CancelToken,
    on_progress: P,
) -> Vec<ScanEntry>
where
    P: FnMut(&Path, u64, u64),
{
    if limit == 0 {
        return Vec::new();
    }

    // `Reverse` turns the max-heap into a min-heap, so the smallest kept file
    // is always the one we pop when a bigger one shows up.
    let mut heap: BinaryHeap<Reverse<(u64, PathBuf, Option<u64>)>> = BinaryHeap::new();

    for_each_file(
        root,
        options,
        cancel,
        |path, metadata, _depth| {
            let size = metadata.len();

            if heap.len() < limit {
                heap.push(Reverse((
                    size,
                    path.to_path_buf(),
                    metadata.modified().ok().and_then(format::epoch_millis),
                )));
                return;
            }

            if heap.peek().is_some_and(|Reverse((smallest, ..))| size > *smallest) {
                heap.pop();
                heap.push(Reverse((
                    size,
                    path.to_path_buf(),
                    metadata.modified().ok().and_then(format::epoch_millis),
                )));
            }
        },
        on_progress,
    );

    let mut files: Vec<ScanEntry> = heap
        .into_iter()
        .map(|Reverse((size, path, modified_at))| ScanEntry {
            path,
            size,
            modified_at,
            is_directory: false,
        })
        .collect();

    files.sort_by(|a, b| b.size.cmp(&a.size));
    files
}

/// Collect entries under the configured roots, invoking `on_progress` as the
/// walk advances. Returns early when `cancel` is tripped.
pub fn collect<F>(
    options: &ScanOptions,
    cancel: &CancelToken,
    mut on_progress: F,
) -> Vec<ScanEntry>
where
    F: FnMut(&Path, u64, u64),
{
    let mut found = Vec::new();
    let mut items = 0u64;
    let mut bytes = 0u64;

    for root in &options.roots {
        for entry in walker(root, options)
            .into_iter()
            .filter_entry(|entry| !should_skip(entry, options))
        {
            if cancel.is_cancelled() {
                return found;
            }

            let Ok(entry) = entry else { continue };
            let Ok(metadata) = entry.metadata() else {
                continue;
            };

            // Symlinks are skipped so their own (meaningless) size is never
            // reported and the target is never double-counted.
            if metadata.is_symlink() {
                continue;
            }

            let is_directory = metadata.is_dir();
            let size = if is_directory { 0 } else { metadata.len() };

            items += 1;
            bytes += size;

            // Report roughly once per 512 entries to keep the IPC quiet.
            if items % PROGRESS_INTERVAL == 0 {
                on_progress(entry.path(), items, bytes);
            }

            if !is_directory && size < options.min_size {
                continue;
            }

            found.push(ScanEntry {
                path: entry.path().to_path_buf(),
                size,
                modified_at: metadata.modified().ok().and_then(format::epoch_millis),
                is_directory,
            });
        }
    }

    found
}

/// Find directories whose name matches one of `names`, without descending into
/// a match (no point walking `node_modules` twice).
pub fn find_dirs_named(root: &Path, names: &[&str], max_depth: usize) -> Vec<PathBuf> {
    let mut matches = Vec::new();

    let mut iter = WalkDir::new(root)
        .max_depth(max_depth)
        .follow_links(false)
        .into_iter();

    while let Some(entry) = iter.next() {
        let Ok(entry) = entry else { continue };

        if !entry.file_type().is_dir() {
            continue;
        }

        if is_hidden(&entry) && entry.depth() > 0 {
            let name = entry.file_name().to_string_lossy().into_owned();
            if !names.contains(&name.as_str()) {
                iter.skip_current_dir();
                continue;
            }
        }

        let name = entry.file_name().to_string_lossy().into_owned();
        if names.contains(&name.as_str()) {
            matches.push(entry.path().to_path_buf());
            iter.skip_current_dir();
        }
    }

    matches
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build `root/{a.bin: 3000, b.bin: 10, nested/c.bin: 500}`.
    fn fixture(name: &str) -> PathBuf {
        let root = crate::utils::test_support::scratch(&format!("walker-{name}"));
        std::fs::create_dir_all(root.join("nested")).expect("create fixture");
        std::fs::write(root.join("a.bin"), vec![0u8; 3_000]).unwrap();
        std::fs::write(root.join("b.bin"), vec![0u8; 10]).unwrap();
        std::fs::write(root.join("nested/c.bin"), vec![0u8; 500]).unwrap();
        root
    }

    #[test]
    fn sums_sizes_recursively() {
        let root = fixture("size");

        assert_eq!(directory_size(&root), 3_510);
        assert_eq!(directory_size_parallel(&root), 3_510);
        assert_eq!(directory_stats(&root), (3_510, 3));

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn largest_files_is_bounded_and_sorted() {
        let root = fixture("largest");
        let options = ScanOptions::default();

        let top = largest_files(&root, 2, &options, &CancelToken::default(), |_, _, _| {});

        assert_eq!(top.len(), 2);
        assert_eq!(top[0].size, 3_000);
        assert_eq!(top[1].size, 500);

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_cancelled_walk_stops_early() {
        let root = fixture("cancel");
        let cancel = CancelToken::default();
        cancel.cancel();

        let summary = for_each_file(
            &root,
            &ScanOptions::default(),
            &cancel,
            |_, _, _| panic!("no file should be visited after cancelling"),
            |_, _, _| {},
        );

        assert!(summary.cancelled);
        assert_eq!(summary.files, 0);

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn finds_artifact_directories_without_descending_into_them() {
        let root = fixture("named");
        std::fs::create_dir_all(root.join("nested/node_modules/inner/node_modules")).unwrap();

        let found = find_dirs_named(&root, &["node_modules"], 6);

        assert_eq!(found, vec![root.join("nested/node_modules")]);

        std::fs::remove_dir_all(&root).ok();
    }

    /// Build a fixture that contains symlinks to both a file and a directory,
    /// so we can verify they are never counted as files or followed. Returns
    /// `None` when the platform will not let us create symlinks (Windows
    /// without the privilege), and the caller steps aside.
    fn symlink_fixture(name: &str) -> Option<PathBuf> {
        let root = crate::utils::test_support::scratch(&format!("walker-symlink-{name}"));
        std::fs::create_dir_all(root.join("real_dir")).expect("create fixture");
        std::fs::write(root.join("real_dir/inner.bin"), vec![0u8; 2_000]).unwrap();
        std::fs::write(root.join("real_file.bin"), vec![0u8; 4_000]).unwrap();

        // Symlink to a file, then to a directory.
        let file_link = crate::utils::test_support::symlink_file(
            &root.join("real_file.bin"),
            &root.join("link_to_file"),
        );
        let dir_link = crate::utils::test_support::symlink_dir(
            &root.join("real_dir"),
            &root.join("link_to_dir"),
        );

        if !file_link || !dir_link {
            std::fs::remove_dir_all(&root).ok();
            return None;
        }

        Some(root)
    }

    #[test]
    fn for_each_file_skips_symlinks() {
        let Some(root) = symlink_fixture("foreach") else {
            return;
        };

        let mut visited = Vec::new();
        let summary = for_each_file(
            &root,
            &ScanOptions::default(),
            &CancelToken::default(),
            |path, _, _| {
                visited.push(path.to_path_buf());
            },
            |_, _, _| {},
        );

        // Only two real files exist: real_file.bin and real_dir/inner.bin.
        assert_eq!(summary.files, 2);
        assert_eq!(summary.bytes, 6_000);

        // No symlink path should appear in the visited list.
        for path in &visited {
            let name = path.file_name().unwrap().to_string_lossy();
            assert!(
                name != "link_to_file" && name != "link_to_dir",
                "symlink {path:?} should have been skipped"
            );
        }

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn directory_size_ignores_symlinks() {
        let Some(root) = symlink_fixture("dirsize") else {
            return;
        };

        // The real files total 6_000 bytes. Symlinks must not add anything,
        // and the directory symlink must not cause double-counting.
        assert_eq!(directory_size(&root), 6_000);
        assert_eq!(directory_size_parallel(&root), 6_000);
        assert_eq!(directory_stats(&root), (6_000, 2));

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn largest_files_excludes_symlinks() {
        let Some(root) = symlink_fixture("largest-symlink") else {
            return;
        };
        let options = ScanOptions::default();

        let top = largest_files(&root, 10, &options, &CancelToken::default(), |_, _, _| {});

        // Only real files appear — symlinks are never offered.
        assert_eq!(top.len(), 2);
        for entry in &top {
            let name = entry.path.file_name().unwrap().to_string_lossy();
            assert!(
                name != "link_to_file" && name != "link_to_dir",
                "symlink {name} must not appear in largest files"
            );
        }

        std::fs::remove_dir_all(&root).ok();
    }
}
