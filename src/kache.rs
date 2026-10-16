// Removes the local cache of kache (https://github.com/kunobi-ninja/kache).
// kache recreates it on the next build.

use std::path::PathBuf;

use crate::disk_usage::{self, DiskUsage};
use crate::keep::INTERRUPTED;
use crate::progress::Progress;
use crate::report::{Item, Section, display_path};
use crate::{interrupted, non_empty_env};

const TITLE: &str = "kache cache";

pub fn clean(usage: &mut DiskUsage, progress: &Progress) -> Section {
    let mut section = Section::new(TITLE);
    let Some(kache_dir) = kache_dir() else {
        return section;
    };
    if !kache_dir.exists() {
        return section;
    }

    let label = display_path(&kache_dir);
    progress.set(TITLE, &label);
    let before = usage.measure(&kache_dir);
    if interrupted() {
        section
            .items
            .push(Item::kept(label, INTERRUPTED, before.total()));
        section.interrupted = true;
        return section;
    }
    let removal = usage.remove(&kache_dir, before, disk_usage::remove_dir_all);
    section.items.push(Item::removed(label, removal));
    section
}

// Resolves the cache directory the same way kache does:
// $KACHE_CACHE_DIR, then $XDG_CACHE_HOME/kache, then ~/.cache/kache.
fn kache_dir() -> Option<PathBuf> {
    if let Some(dir) = non_empty_env("KACHE_CACHE_DIR") {
        return Some(dir);
    }
    let cache_dir = non_empty_env("XDG_CACHE_HOME")
        .or_else(|| non_empty_env("HOME").map(|home| home.join(".cache")))?;
    Some(cache_dir.join("kache"))
}
