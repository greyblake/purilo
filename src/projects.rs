// Cleans build outputs (target/, node_modules/) of projects under the home directory.
// Rust and JavaScript projects follow the same rules. An output directory is removed only when:
// - it is a real directory next to the project's manifest and lockfile, so it can be rebuilt;
// - no running process uses files in the project;
// - the project didn't change for MIN_IDLE, or for LARGE_MIN_IDLE when the output is LARGE.

use std::collections::HashSet;
use std::ffi::OsStr;
use std::fs;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::process;
use std::time::{Duration, SystemTime};

use crate::disk_usage::DiskUsage;
use crate::keep::{INTERRUPTED, keep_reason, recently_changed};
use crate::progress::Progress;
use crate::report::{Item, Section, display_path};
use crate::{interrupted, non_empty_env};

const IN_USE: &str = "in use";

// Directories with these names are not searched for projects or for changes.
const SKIPPED_DIR_NAMES: [&str; 2] = ["node_modules", "target"];

pub struct Kind {
    // The build output directory, next to the manifest.
    pub output: &'static str,
    pub manifest: &'static str,
    // At least one of these.
    pub lockfiles: &'static [&'static str],
}

// `remove` gets the project and its output directory.
pub fn clean(
    title: &'static str,
    kind: &Kind,
    usage: &mut DiskUsage,
    progress: &Progress,
    remove: impl Fn(&Path, &Path) -> Result<(), String>,
) -> Section {
    let mut section = Section::new(title);
    let Some(home) = non_empty_env("HOME") else {
        return section;
    };
    progress.set(title, "searching ~");
    let projects = find(&home, kind);
    if interrupted() {
        section.interrupted = true;
        return section;
    }
    if projects.is_empty() {
        return section;
    }

    progress.set(title, "checking running processes");
    let in_use_at_start = match paths_in_use() {
        Ok(paths) => paths,
        Err(err) => {
            let reason = format!("cannot tell which files are in use: {err}");
            return Section::skipped(title, reason);
        }
    };

    for project in projects {
        if interrupted() {
            section.interrupted = true;
            break;
        }
        let label = display_path(&project);
        progress.set(title, &label);
        let output = project.join(kind.output);

        // Measured before the checks, so they run right before the removal. Processes are
        // scanned again in case a dev server or a build started since the section started.
        let before = usage.measure(&output);
        let size = before.total();
        let keep = keep_reason(
            IN_USE,
            uses(&in_use_at_start, &project),
            || last_change(&project, &output).map(|changed| recently_changed(age(changed), size)),
            || paths_in_use().map(|paths| uses(&paths, &project)),
        );
        if keep.is_none() && interrupted() {
            section.items.push(Item::kept(label, INTERRUPTED, size));
            section.interrupted = true;
            break;
        }

        let item = match keep {
            Some(reason) => Item::kept(label, reason, size),
            None => {
                let removal = usage.remove(&output, before, |output| remove(&project, output));
                Item::removed(label, removal)
            }
        };
        section.items.push(item);
    }
    section
}

fn uses(paths_in_use: &HashSet<PathBuf>, project: &Path) -> bool {
    paths_in_use.iter().any(|path| path.starts_with(project))
}

// A time in the future (clock changes) counts as just now.
fn age(time: SystemTime) -> Duration {
    SystemTime::now()
        .duration_since(time)
        .unwrap_or(Duration::ZERO)
}

// Directories with the manifest, a lockfile and the output directory (not a symlink).
// Hidden directories, ~/snap and SKIPPED_DIR_NAMES are not searched. Symlinks are not followed.
fn find(home: &Path, kind: &Kind) -> Vec<PathBuf> {
    let home_snap = home.join("snap");
    let mut projects = Vec::new();
    let mut dirs = vec![home.to_path_buf()];

    while let Some(dir) = dirs.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        let mut has_manifest = false;
        let mut has_lockfile = false;
        let mut has_output = false;

        for entry in entries.filter_map(Result::ok) {
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            let name = entry.file_name();
            let name = name.to_string_lossy();

            if file_type.is_dir() {
                has_output |= name == kind.output;
                let path = entry.path();
                if !name.starts_with('.')
                    && !SKIPPED_DIR_NAMES.contains(&name.as_ref())
                    && path != home_snap
                {
                    dirs.push(path);
                }
            } else if file_type.is_file() {
                has_manifest |= name == kind.manifest;
                has_lockfile |= kind.lockfiles.contains(&name.as_ref());
            }
        }

        if has_manifest && has_lockfile && has_output {
            projects.push(dir);
        }
    }
    projects
}

// When the project last changed: anything in it outside build outputs and .git, plus the
// output directory and its direct entries, which builds and installs touch. Walking the whole
// output would be slow. Entries that vanish or can't be read (e.g. a root-owned Docker volume)
// are skipped.
fn last_change(project: &Path, output: &Path) -> io::Result<SystemTime> {
    let mut newest = fs::symlink_metadata(output)?.modified()?;
    for entry in fs::read_dir(output)? {
        newest = newest.max(entry?.metadata()?.modified()?);
    }

    newest = newest.max(fs::symlink_metadata(project)?.modified()?);
    let mut dirs = vec![fs::read_dir(project)?];
    while let Some(entries) = dirs.last_mut() {
        let Some(entry) = entries.next() else {
            dirs.pop();
            continue;
        };
        // DirEntry::metadata doesn't follow symlinks.
        let metadata = match entry.and_then(|entry| Ok((entry.metadata()?, entry))) {
            Ok((metadata, entry)) => {
                let name = entry.file_name();
                let descend = metadata.is_dir()
                    && name != ".git"
                    && !SKIPPED_DIR_NAMES.iter().any(|skipped| name == *skipped);
                if descend {
                    match fs::read_dir(entry.path()) {
                        Ok(entries) => dirs.push(entries),
                        Err(err) if skippable(&err) => {}
                        Err(err) => return Err(err),
                    }
                }
                metadata
            }
            Err(err) if skippable(&err) => continue,
            Err(err) => return Err(err),
        };
        newest = newest.max(metadata.modified()?);
    }
    Ok(newest)
}

fn skippable(err: &io::Error) -> bool {
    matches!(
        err.kind(),
        io::ErrorKind::NotFound | io::ErrorKind::PermissionDenied
    )
}

// Working directories, executables, open files, memory-mapped files and absolute
// command line arguments of running processes, except this one.
// Processes of other users can't be inspected and are left out.
fn paths_in_use() -> io::Result<HashSet<PathBuf>> {
    let own_pid = process::id().to_string();
    let mut paths = HashSet::new();

    for entry in fs::read_dir("/proc")? {
        let entry = entry?;
        let pid = entry.file_name();
        let pid = pid.to_string_lossy();
        if !pid.bytes().all(|b| b.is_ascii_digit()) || pid == own_pid {
            continue;
        }
        let proc_dir = entry.path();

        paths.extend(fs::read_link(proc_dir.join("cwd")));
        paths.extend(fs::read_link(proc_dir.join("exe")));
        if let Ok(fds) = fs::read_dir(proc_dir.join("fd")) {
            paths.extend(
                fds.filter_map(Result::ok)
                    .filter_map(|fd| fs::read_link(fd.path()).ok()),
            );
        }
        if let Ok(cmdline) = fs::read(proc_dir.join("cmdline")) {
            paths.extend(
                cmdline
                    .split(|&b| b == 0)
                    .filter(|arg| arg.starts_with(b"/"))
                    .map(|arg| PathBuf::from(OsStr::from_bytes(arg))),
            );
        }
        // The path is the last column, and the only one that contains '/'.
        if let Ok(maps) = fs::read_to_string(proc_dir.join("maps")) {
            paths.extend(
                maps.lines()
                    .filter_map(|line| line.find('/').map(|start| PathBuf::from(&line[start..]))),
            );
        }
    }
    Ok(paths)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use std::os::unix::fs::symlink;
    use tempfile::TempDir;

    const RUST: Kind = Kind {
        output: "target",
        manifest: "Cargo.toml",
        lockfiles: &["Cargo.lock"],
    };
    const JS: Kind = Kind {
        output: "node_modules",
        manifest: "package.json",
        lockfiles: &["package-lock.json", "yarn.lock"],
    };
    const DAY: Duration = Duration::from_secs(24 * 60 * 60);

    fn scratch() -> TempDir {
        tempfile::tempdir_in(concat!(env!("CARGO_MANIFEST_DIR"), "/target")).unwrap()
    }

    fn touch(path: &Path) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, "").unwrap();
    }

    fn project(dir: &Path, files: &[&str], dirs: &[&str]) {
        for file in files {
            touch(&dir.join(file));
        }
        for sub in dirs {
            fs::create_dir_all(dir.join(sub)).unwrap();
        }
    }

    fn set_mtime(path: &Path, ago: Duration) {
        let time = SystemTime::now() - ago;
        File::open(path).unwrap().set_modified(time).unwrap();
    }

    // Sets every mtime in the tree to `ago`, children before parents.
    fn age_tree(path: &Path, ago: Duration) {
        if fs::symlink_metadata(path).unwrap().is_dir() {
            for entry in fs::read_dir(path).unwrap() {
                age_tree(&entry.unwrap().path(), ago);
            }
        }
        set_mtime(path, ago);
    }

    #[test]
    fn finds_projects_with_manifest_lockfile_and_output() {
        let tmp = scratch();
        let home = tmp.path();
        let full = &["Cargo.toml", "Cargo.lock"];
        project(&home.join("dev/rust"), full, &["target"]);
        project(&home.join("dev/no-target"), full, &[]);
        project(&home.join("dev/no-manifest"), &["Cargo.lock"], &["target"]);
        project(&home.join(".hidden/rust"), full, &["target"]);
        project(&home.join("snap/app/rust"), full, &["target"]);
        project(&home.join("dev/rust/target/inner"), full, &["target"]);
        project(&home.join("dev/linked"), full, &[]);
        symlink(home.join("dev/rust/target"), home.join("dev/linked/target")).unwrap();
        project(
            &home.join("dev/web"),
            &["package.json", "yarn.lock"],
            &["node_modules"],
        );
        project(
            &home.join("dev/web-no-lock"),
            &["package.json"],
            &["node_modules"],
        );

        assert_eq!(find(home, &RUST), [home.join("dev/rust")]);
        assert_eq!(find(home, &JS), [home.join("dev/web")]);
    }

    #[test]
    fn sees_changes_to_sources_and_to_the_top_of_the_output() {
        let tmp = scratch();
        let root = tmp.path().join("project");
        project(
            &root,
            &[
                "Cargo.toml",
                "src/lib.rs",
                ".git/index",
                "target/debug/deps/libx.rlib",
                "crates/a/target/debug/x",
            ],
            &[],
        );
        let output = root.join("target");
        let changed_recently = |path: &str| {
            age_tree(&root, 3 * DAY);
            if !path.is_empty() {
                set_mtime(&root.join(path), Duration::ZERO);
            }
            age(last_change(&root, &output).unwrap()) < DAY
        };

        assert!(!changed_recently(""));
        assert!(changed_recently("src/lib.rs"));
        assert!(changed_recently("src"));
        assert!(changed_recently("target/debug"));
        assert!(!changed_recently("target/debug/deps/libx.rlib"));
        assert!(!changed_recently(".git/index"));
        assert!(!changed_recently("crates/a/target/debug/x"));
    }
}
