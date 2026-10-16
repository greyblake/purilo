// Runs `cargo clean` in Rust projects under the home directory, by the rules in `projects`.

use std::os::unix::process::ExitStatusExt;
use std::path::Path;
use std::process::Command;

use crate::disk_usage::DiskUsage;
use crate::interrupted;
use crate::keep::INTERRUPTED;
use crate::progress::Progress;
use crate::projects::{self, Kind};
use crate::report::Section;

const TITLE: &str = "Rust projects";

const RUST: Kind = Kind {
    output: "target",
    manifest: "Cargo.toml",
    lockfiles: &["Cargo.lock"],
};

pub fn clean(usage: &mut DiskUsage, progress: &Progress) -> Section {
    projects::clean(TITLE, &RUST, usage, progress, |project, _| {
        cargo_clean(project)
    })
}

fn cargo_clean(rust_project_path: &Path) -> Result<(), String> {
    let output = Command::new("cargo")
        .args(["clean", "--color", "never"])
        .current_dir(rust_project_path)
        .output()
        .map_err(|err| format!("cannot run cargo: {err}"))?;
    match output.status.signal() {
        _ if output.status.success() => Ok(()),
        // A Ctrl-C in the terminal reaches cargo too.
        _ if interrupted() => Err(INTERRUPTED.to_string()),
        Some(signal) => Err(format!("cargo was killed by signal {signal}")),
        None => Err(failure_reason(&String::from_utf8_lossy(&output.stderr))),
    }
}

// The first `error: ` line; the last line can be a source snippet or a cause without the path.
fn failure_reason(stderr: &str) -> String {
    stderr
        .lines()
        .find_map(|line| line.strip_prefix("error: "))
        .or_else(|| stderr.lines().rev().find(|line| !line.trim().is_empty()))
        .map_or_else(
            || "cargo clean failed".to_string(),
            |line| line.trim().to_string(),
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn takes_the_error_line_as_the_failure_reason() {
        let malformed_manifest = "error: string values must be quoted, expected literal string
 --> Cargo.toml:3:11
  |
3 | version =
  |           ^
";
        assert_eq!(
            failure_reason(malformed_manifest),
            "string values must be quoted, expected literal string"
        );

        let missing_member = "error: failed to load manifest for workspace member `/w/gone`
referenced by workspace at `/w/Cargo.toml`

Caused by:
  failed to read `/w/gone/Cargo.toml`

Caused by:
  No such file or directory (os error 2)
";
        assert_eq!(
            failure_reason(missing_member),
            "failed to load manifest for workspace member `/w/gone`"
        );

        let missing_manifest =
            "error: could not find `Cargo.toml` in `/w/x` or any parent directory\n";
        assert_eq!(
            failure_reason(missing_manifest),
            "could not find `Cargo.toml` in `/w/x` or any parent directory"
        );

        assert_eq!(failure_reason("warning: something\nKilled\n\n"), "Killed");
        assert_eq!(failure_reason(""), "cargo clean failed");
    }
}
