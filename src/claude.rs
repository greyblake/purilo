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

use crate::non_empty_env;

const MIN_IDLE: Duration = Duration::from_secs(24 * 60 * 60);

pub fn clean() {
    let Some(config_dir) = config_dir() else {
        return;
    };
    let Some(user_tmp_dir) = user_tmp_dir(&config_dir) else {
        return;
    };
    if !user_tmp_dir.exists() {
        return;
    }

    let running_sessions = match running_session_ids(&config_dir.join("sessions")) {
        Ok(ids) => ids,
        Err(err) => {
            println!(
                "Skipped {}: cannot tell which Claude sessions are running: {err}",
                user_tmp_dir.display()
            );
            return;
        }
    };

    let mut kept_running = 0;
    for session_dir in session_dirs(&user_tmp_dir) {
        let session_id = session_dir
            .file_name()
            .unwrap_or_default()
            .to_string_lossy();
        if running_sessions.contains(session_id.as_ref()) {
            kept_running += 1;
            continue;
        }
        if !is_idle(&session_dir) {
            continue;
        }

        match fs::remove_dir_all(&session_dir) {
            Ok(()) => println!("Cleaned {}", session_dir.display()),
            Err(err) => println!("Failed to clean {}: {err}", session_dir.display()),
        }
    }

    if kept_running > 0 {
        println!("Kept {kept_running} Claude session directories in use by running sessions");
    }
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

// Whether nothing in the directory changed for MIN_IDLE.
// Anything that can't be checked counts as recently changed.
fn is_idle(dir: &Path) -> bool {
    let Ok(cutoff) = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH) else {
        return false;
    };
    let cutoff = cutoff.saturating_sub(MIN_IDLE);
    newest_mtime(dir).is_ok_and(|mtime| mtime < cutoff)
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
