// Cleans up the temporary directory of Claude Code.
//
// Claude Code keeps per-session files (scratchpad, background task output, pasted images)
// in <tmp>/claude-<uid>/<project>/<session-id>/. A session directory is removed only when
// its session is not running and nothing in it changed for MIN_IDLE.
// Anything else in the temporary directory is left alone.

use std::collections::HashSet;
use std::fs;
use std::io;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde::Deserialize;

use crate::disk_usage::{self, DiskUsage};
use crate::keep::{INTERRUPTED, MIN_IDLE, NOT_IDLE, keep_reason};
use crate::progress::Progress;
use crate::report::{Item, Section};
use crate::{interrupted, non_empty_env};

const TITLE: &str = "Claude Code tmp";

const RUNNING: &str = "running";

pub fn clean(usage: &mut DiskUsage, progress: &Progress) -> Section {
    let mut section = Section::new(TITLE);
    let Some(config_dir) = config_dir() else {
        return section;
    };
    let Some(user_tmp_dir) = user_tmp_dir(&config_dir) else {
        return section;
    };
    if !user_tmp_dir.exists() {
        return section;
    }

    let sessions_dir = config_dir.join("sessions");
    let running_at_start = match running_session_ids(&sessions_dir) {
        Ok(ids) => ids,
        Err(err) => {
            let reason = format!("cannot tell which Claude sessions are running: {err}");
            return Section::skipped(TITLE, reason);
        }
    };

    let home_slug = non_empty_env("HOME").map(|home| home_slug(&home));
    for session_dir in session_dirs(&user_tmp_dir) {
        if interrupted() {
            section.interrupted = true;
            break;
        }
        let label = session_label(&session_dir, home_slug.as_deref());
        progress.set(TITLE, &label);
        let session_id = session_dir
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();

        // Measured before the checks, so they run right before the removal. The registry is
        // read again in case the session was resumed since the section started.
        let before = usage.measure(&session_dir);
        let keep = keep_reason(
            RUNNING,
            running_at_start.contains(&session_id),
            || is_idle(&session_dir).map(|idle| (!idle).then_some(NOT_IDLE)),
            || running_session_ids(&sessions_dir).map(|ids| ids.contains(&session_id)),
        );
        if keep.is_none() && interrupted() {
            section
                .items
                .push(Item::kept(label, INTERRUPTED, before.total()));
            section.interrupted = true;
            break;
        }

        let item = match keep {
            Some(reason) => Item::kept(label, reason, before.total()),
            None => {
                let removal = usage.remove(&session_dir, before, disk_usage::remove_dir_all);
                Item::removed(label, removal)
            }
        };
        section.items.push(item);
    }
    section
}

fn home_slug(home: &Path) -> String {
    slug(home.to_string_lossy().trim_end_matches('/'))
}

// "<project> · <session id prefix>". The project directory is named after the session's
// working directory, with every non-alphanumeric character replaced by '-'. That is lossy,
// so this is only a label; the $HOME part of it is dropped.
fn session_label(session_dir: &Path, home_slug: Option<&str>) -> String {
    let name = |path: Option<&Path>| {
        path.and_then(Path::file_name)
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default()
    };
    let session_id = name(Some(session_dir));
    let project_slug = name(session_dir.parent());
    let project = match home_slug {
        Some(home_slug) if project_slug == home_slug => "~",
        Some(home_slug) => project_slug
            .strip_prefix(home_slug)
            .and_then(|rest| rest.strip_prefix('-'))
            .unwrap_or(&project_slug),
        None => &project_slug,
    };
    let short_id: String = session_id.chars().take(8).collect();
    format!("{project} · {short_id}")
}

fn slug(path: &str) -> String {
    path.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

fn config_dir() -> Option<PathBuf> {
    non_empty_env("CLAUDE_CONFIG_DIR")
        .or_else(|| non_empty_env("HOME").map(|home| home.join(".claude")))
}

// Resolves <tmp>/claude-<uid> the same way Claude Code does:
// $CLAUDE_CODE_TMPDIR, then `env.CLAUDE_CODE_TMPDIR` in settings.json, then $TMPDIR, then /tmp.
fn user_tmp_dir(config_dir: &Path) -> Option<PathBuf> {
    let tmp_dir = non_empty_env("CLAUDE_CODE_TMPDIR")
        .or_else(|| tmp_dir_from_settings(&config_dir.join("settings.json")))
        .or_else(|| non_empty_env("TMPDIR"))
        .unwrap_or_else(|| PathBuf::from("/tmp"));
    let uid = fs::metadata("/proc/self").ok()?.uid();
    Some(tmp_dir.join(format!("claude-{uid}")))
}

fn tmp_dir_from_settings(settings_path: &Path) -> Option<PathBuf> {
    let content = fs::read_to_string(settings_path).ok()?;
    let settings: serde_json::Value = serde_json::from_str(&content).ok()?;
    let tmp_dir = settings["env"]["CLAUDE_CODE_TMPDIR"].as_str()?;
    (!tmp_dir.is_empty()).then(|| PathBuf::from(tmp_dir))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SessionRecord {
    session_id: String,
    pid: u32,
    proc_start: Option<String>,
    pid_domain: Option<String>,
}

// Claude Code registers every running session in <config>/sessions/<pid>.json.
// Fails if any record can't be read, so a session is never mistaken for a finished one.
fn running_session_ids(sessions_dir: &Path) -> Result<HashSet<String>, String> {
    let entries =
        fs::read_dir(sessions_dir).map_err(|err| format!("{}: {err}", sessions_dir.display()))?;

    let mut ids = HashSet::new();
    for entry in entries {
        let path = entry.map_err(|err| err.to_string())?.path();
        if path.extension().is_none_or(|ext| ext != "json") {
            continue;
        }
        let content =
            fs::read_to_string(&path).map_err(|err| format!("{}: {err}", path.display()))?;
        let record: SessionRecord =
            serde_json::from_str(&content).map_err(|err| format!("{}: {err}", path.display()))?;
        if is_running(&record) {
            ids.insert(record.session_id);
        }
    }
    Ok(ids)
}

// A record is left behind when a session crashes, and its PID may be reused later,
// so the process start time must match too.
// A session in another PID namespace can't be checked, so it is assumed to be running.
fn is_running(record: &SessionRecord) -> bool {
    if let (Some(domain), Ok(own_namespace)) =
        (&record.pid_domain, fs::read_link("/proc/self/ns/pid"))
        && !domain.ends_with(own_namespace.to_string_lossy().as_ref())
    {
        return true;
    }

    let stat = match fs::read_to_string(format!("/proc/{}/stat", record.pid)) {
        Ok(stat) => stat,
        Err(err) => return err.kind() != io::ErrorKind::NotFound,
    };
    let Some(proc_start) = &record.proc_start else {
        return true;
    };
    // Fields after the command name, which is in parentheses and may contain spaces.
    // The start time is field 22 of the whole line.
    let start_time = stat
        .rsplit_once(')')
        .and_then(|(_, fields)| fields.split_whitespace().nth(19));
    start_time == Some(proc_start.as_str())
}

fn session_dirs(user_tmp_dir: &Path) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    for project_dir in real_subdirs(user_tmp_dir) {
        for dir in real_subdirs(&project_dir) {
            if dir
                .file_name()
                .is_some_and(|name| is_uuid(&name.to_string_lossy()))
            {
                dirs.push(dir);
            }
        }
    }
    dirs
}

// Subdirectories, not following symlinks.
fn real_subdirs(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_ok_and(|file_type| file_type.is_dir()))
        .map(|entry| entry.path())
        .collect()
}

fn is_uuid(name: &str) -> bool {
    name.len() == 36
        && name.char_indices().all(|(i, c)| match i {
            8 | 13 | 18 | 23 => c == '-',
            _ => c.is_ascii_hexdigit(),
        })
}

// Whether nothing in the directory changed for MIN_IDLE; an error when something in it
// can't be checked.
fn is_idle(dir: &Path) -> io::Result<bool> {
    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_err(io::Error::other)?;
    Ok(newest_mtime(dir)? < now.saturating_sub(MIN_IDLE))
}

// Newest modification time in the tree, as a duration since the Unix epoch.
fn newest_mtime(path: &Path) -> io::Result<Duration> {
    let metadata = fs::symlink_metadata(path)?;
    let mut newest = metadata
        .modified()?
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_err(io::Error::other)?;
    if metadata.is_dir() {
        for entry in fs::read_dir(path)? {
            newest = newest.max(newest_mtime(&entry?.path())?);
        }
    }
    Ok(newest)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SESSION: &str = "54ce425c-e0ca-4a9a-8a66-ed89089141a5";

    fn label(project_slug: &str, home_slug: Option<&str>) -> String {
        let dir = Path::new("/tmp/claude-1000")
            .join(project_slug)
            .join(SESSION);
        session_label(&dir, home_slug)
    }

    #[test]
    fn labels_sessions_by_project_without_home() {
        let home = slug("/home/me");
        assert_eq!(home, "-home-me");
        assert_eq!(label("-home-me-dev-x", Some(&home)), "dev-x · 54ce425c");
        assert_eq!(label("-home-me", Some(&home)), "~ · 54ce425c");
        assert_eq!(
            label("-home-meow-x", Some(&home)),
            "-home-meow-x · 54ce425c"
        );
        assert_eq!(label("-srv-x", None), "-srv-x · 54ce425c");
    }

    #[test]
    fn ignores_a_trailing_slash_in_home() {
        assert_eq!(home_slug(Path::new("/home/me/")), "-home-me");
        assert_eq!(home_slug(Path::new("/home/me")), "-home-me");
    }
}
