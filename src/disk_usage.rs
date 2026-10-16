// Measures the disk space of directory trees and how much removing them frees.

use std::collections::HashMap;
use std::fs::{self, Metadata};
use std::io;
use std::os::unix::fs::MetadataExt;
use std::path::Path;

// (device, inode)
type Inode = (u64, u64);

// Bytes allocated on disk (st_blocks * 512, like du) in a tree, not following symlinks.
#[derive(Debug, Default)]
pub struct Measured {
    // Directories and files with a single link.
    single: u64,
    // Files with more links, and tracked ones, by inode so that hardlinks count once.
    files: HashMap<Inode, File>,
}

#[derive(Debug, Clone, Copy)]
struct File {
    nlink: u64,
    // How many of the links are in the tree.
    links: u64,
    bytes: u64,
}

impl Measured {
    pub fn total(&self) -> u64 {
        self.files
            .values()
            .fold(self.single, |total, file| total.saturating_add(file.bytes))
    }

    fn add(&mut self, metadata: &Metadata, track: &impl Fn(&Inode) -> bool) {
        let bytes = metadata.blocks().saturating_mul(512);
        let inode = (metadata.dev(), metadata.ino());
        if metadata.is_dir() || (metadata.nlink() <= 1 && !track(&inode)) {
            self.single = self.single.saturating_add(bytes);
            return;
        }
        self.files
            .entry(inode)
            .or_insert(File {
                nlink: metadata.nlink(),
                links: 0,
                bytes,
            })
            .links += 1;
    }
}

// `track` picks single-link files to record by inode too. Unreadable entries count as 0:
// measuring must never get in the way of cleaning. Memory grows with the depth and the
// number of hardlinked files, not with the size of the tree.
fn measure_tracking(path: &Path, track: &impl Fn(&Inode) -> bool) -> Measured {
    let mut measured = Measured::default();
    let Ok(metadata) = fs::symlink_metadata(path) else {
        return measured;
    };
    measured.add(&metadata, track);
    if !metadata.is_dir() {
        return measured;
    }

    let mut dirs: Vec<_> = fs::read_dir(path).into_iter().collect();
    while let Some(entries) = dirs.last_mut() {
        let Some(entry) = entries.next() else {
            dirs.pop();
            continue;
        };
        let Ok(entry) = entry else {
            continue;
        };
        // DirEntry::metadata doesn't follow symlinks.
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        measured.add(&metadata, track);
        if metadata.is_dir() {
            dirs.extend(fs::read_dir(entry.path()));
        }
    }
    measured
}

pub struct Removal {
    // Whether there was anything at the path.
    pub existed: bool,
    // What the path held before removing it.
    pub size: u64,
    pub freed: u64,
    pub error: Option<String>,
}

// Space freed over the run. A file frees its blocks only when its last link is removed;
// the link count read before each removal already reflects earlier removals.
#[derive(Debug, Default)]
pub struct DiskUsage {
    // Bytes of removed files that still had links elsewhere.
    still_linked: HashMap<Inode, u64>,
}

impl DiskUsage {
    // Like `measure`, but also tracks files whose other links were removed earlier in the run.
    pub fn measure(&self, path: &Path) -> Measured {
        measure_tracking(path, &|inode| self.still_linked.contains_key(inode))
    }

    // Runs `remove` on `path`, measured as `before` just beforehand, and records what it
    // freed, also when it failed half way.
    pub fn remove(
        &mut self,
        path: &Path,
        before: Measured,
        remove: impl FnOnce(&Path) -> Result<(), String>,
    ) -> Removal {
        // Gone since it was measured: someone else removed it, nothing to credit.
        let existed = fs::symlink_metadata(path).is_ok();
        let size = if existed { before.total() } else { 0 };
        let error = remove(path).err();
        let after = fs::symlink_metadata(path)
            .is_ok()
            .then(|| measure_tracking(path, &|inode| before.files.contains_key(inode)));
        let freed = if existed {
            self.record_removal(before, after)
        } else {
            0
        };
        Removal {
            existed,
            size,
            freed,
            error,
        }
    }

    // `before` is the path measured before removing it, `after` what is left of it.
    fn record_removal(&mut self, before: Measured, after: Option<Measured>) -> u64 {
        let after = after.unwrap_or_default();
        let mut freed = before.single.saturating_sub(after.single);

        for (inode, file) in before.files {
            let left = after.files.get(&inode).map_or(0, |left| left.links);
            let removed = file.links.saturating_sub(left);
            if removed == 0 {
                continue;
            }
            if removed >= file.nlink {
                freed = freed.saturating_add(file.bytes);
                self.still_linked.remove(&inode);
            } else {
                self.still_linked.insert(inode, file.bytes);
            }
        }
        freed
    }

    // Bytes of removed files that are still on disk because other links to them remain.
    pub fn still_linked(&self) -> u64 {
        self.still_linked
            .values()
            .fold(0, |total, bytes| total.saturating_add(*bytes))
    }
}

// Like fs::remove_dir_all, but a path that is already gone (e.g. removed by another purilo
// running at the same time) is not an error.
pub fn remove_dir_all(path: &Path) -> Result<(), String> {
    match fs::remove_dir_all(path) {
        Err(err) if err.kind() != io::ErrorKind::NotFound => Err(err.to_string()),
        _ => Ok(()),
    }
}

// Space available to unprivileged users on the filesystem holding `path`.
pub fn free_space(path: &Path) -> Option<u64> {
    let stat = rustix::fs::statvfs(path).ok()?;
    stat.f_bavail.checked_mul(stat.f_frsize)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use tempfile::TempDir;

    // Under target/ rather than /tmp: on tmpfs directories take no blocks, which would hide
    // mistakes in counting them.
    fn scratch() -> TempDir {
        let dir = tempfile::tempdir_in(concat!(env!("CARGO_MANIFEST_DIR"), "/target")).unwrap();
        assert!(
            allocated(dir.path()) > 0,
            "directories must take blocks here"
        );
        dir
    }

    fn file(dir: &Path, name: &str, len: usize) -> PathBuf {
        fs::create_dir_all(dir).unwrap();
        let path = dir.join(name);
        fs::write(&path, vec![1u8; len]).unwrap();
        path
    }

    fn allocated(path: &Path) -> u64 {
        fs::symlink_metadata(path).unwrap().blocks() * 512
    }

    fn remove_all(usage: &mut DiskUsage, path: &Path) -> Removal {
        let before = usage.measure(path);
        usage.remove(path, before, remove_dir_all)
    }

    #[test]
    fn counts_a_file_hardlinked_within_the_tree_once() {
        let tmp = scratch();
        let tree = tmp.path().join("tree");
        let original = file(&tree, "a", 100_000);
        fs::hard_link(&original, tree.join("b")).unwrap();
        let expected = allocated(&tree) + allocated(&original);

        assert_eq!(DiskUsage::default().measure(&tree).total(), expected);

        let mut usage = DiskUsage::default();
        let removal = remove_all(&mut usage, &tree);
        assert_eq!((removal.size, removal.freed), (expected, expected));
        assert_eq!(usage.still_linked(), 0);
    }

    #[test]
    fn does_not_credit_a_file_still_linked_from_outside() {
        let tmp = scratch();
        let tree = tmp.path().join("tree");
        let original = file(&tree, "a", 100_000);
        let file_bytes = allocated(&original);
        fs::hard_link(&original, tmp.path().join("kept")).unwrap();
        let dir_bytes = allocated(&tree);

        let mut usage = DiskUsage::default();
        let removal = remove_all(&mut usage, &tree);

        assert_eq!(removal.size, dir_bytes + file_bytes);
        assert_eq!(removal.freed, dir_bytes);
        assert_eq!(usage.still_linked(), file_bytes);
    }

    #[test]
    fn credits_the_removal_of_the_last_link() {
        let tmp = scratch();
        let first = tmp.path().join("first");
        let second = tmp.path().join("second");
        let original = file(&first, "a", 100_000);
        let file_bytes = allocated(&original);
        fs::create_dir(&second).unwrap();
        fs::hard_link(&original, second.join("a")).unwrap();
        let (first_dir, second_dir) = (allocated(&first), allocated(&second));

        let mut usage = DiskUsage::default();
        assert_eq!(remove_all(&mut usage, &first).freed, first_dir);
        assert_eq!(usage.still_linked(), file_bytes);
        assert_eq!(
            remove_all(&mut usage, &second).freed,
            second_dir + file_bytes
        );
        assert_eq!(usage.still_linked(), 0);
    }

    #[test]
    fn credits_only_what_a_failed_removal_deleted() {
        let tmp = scratch();
        let tree = tmp.path().join("tree");
        let removed = file(&tree, "removed", 100_000);
        file(&tree, "kept", 50_000);
        let removed_bytes = allocated(&removed);

        let mut usage = DiskUsage::default();
        let before = usage.measure(&tree);
        let removal = usage.remove(&tree, before, |tree| {
            fs::remove_file(tree.join("removed")).unwrap();
            Err("permission denied".to_string())
        });

        assert_eq!(removal.freed, removed_bytes);
        assert_eq!(removal.error.as_deref(), Some("permission denied"));
    }

    #[test]
    fn does_not_credit_a_hardlinked_file_left_by_a_failed_removal() {
        let tmp = scratch();
        let tree = tmp.path().join("tree");
        let original = file(&tree, "a", 100_000);
        fs::hard_link(&original, tree.join("b")).unwrap();

        let mut usage = DiskUsage::default();
        let before = usage.measure(&tree);
        let removal = usage.remove(&tree, before, |tree| {
            fs::remove_file(tree.join("b")).unwrap();
            Err("permission denied".to_string())
        });

        assert_eq!(removal.freed, 0);
    }

    #[test]
    fn tells_a_missing_path_from_an_empty_one() {
        let tmp = scratch();
        let mut usage = DiskUsage::default();

        let missing = tmp.path().join("missing");
        assert_eq!(usage.measure(&missing).total(), 0);
        let removal = remove_all(&mut usage, &missing);
        assert!(!removal.existed && removal.error.is_none());

        // An empty file takes no blocks, even here.
        let empty = file(tmp.path(), "empty", 0);
        let before = usage.measure(&empty);
        let removal = usage.remove(&empty, before, |path| {
            fs::remove_file(path).map_err(|err| err.to_string())
        });
        assert!(removal.existed && removal.error.is_none());
        assert_eq!(removal.size, 0);
    }

    #[test]
    fn does_not_credit_a_path_removed_by_someone_else() {
        let tmp = scratch();
        let tree = tmp.path().join("tree");
        file(&tree, "a", 100_000);

        let mut usage = DiskUsage::default();
        let before = usage.measure(&tree);
        fs::remove_dir_all(&tree).unwrap();
        let removal = usage.remove(&tree, before, remove_dir_all);

        assert!(!removal.existed);
        assert_eq!((removal.size, removal.freed), (0, 0));
    }

    #[test]
    fn keys_only_hardlinked_files_by_inode() {
        let tmp = scratch();
        let tree = tmp.path().join("tree");
        for name in ["a", "b", "c"] {
            file(&tree, name, 10_000);
        }
        let measured = DiskUsage::default().measure(&tree);
        assert!(measured.files.is_empty());
        assert!(measured.single > 0);
    }
}
