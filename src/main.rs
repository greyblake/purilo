use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

mod claude;
mod node_modules;

// System and package-manager directories with read-only or vendored Rust projects.
const IGNORED_DIRS: [&str; 3] = ["/nix", "/snap", "/usr"];

fn main() {
    let output = Command::new("locate")
        .arg("Cargo.lock")
        .output()
        .expect("Failed to run `locate Cargo.lock`");

    // TODO: consider using OsString
    let stdout = String::from_utf8(output.stdout).expect("Invalid UTF-8");

    let project_paths: Vec<&Path> =  stdout
        .lines()
        .filter(|line| {
            !line.is_empty()
                && !IGNORED_DIRS.iter().any(|dir| Path::new(line).starts_with(dir))
                && !line.contains(".cargo")
                && !line.contains(".rustup")
        })
        .map(|line| Path::new(line).parent().expect("No parent"))
        .collect();

    for project_path in project_paths.iter() {
        if !project_path.exists() {
            println!("{} does not exist", project_path.display());
            continue;
        }

        let output = cargo_clean(project_path);
        if output.status.success() {
            println!("Cleaned {}", project_path.display());
        } else {
            println!("Failed to clean {}", project_path.display());
        }
    }

    clean_kache();
    claude::clean();
    node_modules::clean();
}


fn cargo_clean(rust_project_path: &Path) -> Output {
    Command::new("cargo")
        .arg("clean")
        .current_dir(rust_project_path)
        .output()
        .expect("Failed to run `cargo clean`")
}

// Removes the local cache of kache (https://github.com/kunobi-ninja/kache).
// kache recreates it on the next build.
fn clean_kache() {
    let Some(kache_dir) = kache_dir() else {
        return;
    };
    if !kache_dir.exists() {
        return;
    }

    match fs::remove_dir_all(&kache_dir) {
        Ok(()) => println!("Cleaned {}", kache_dir.display()),
        Err(err) => println!("Failed to clean {}: {err}", kache_dir.display()),
    }
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

fn non_empty_env(name: &str) -> Option<PathBuf> {
    env::var_os(name)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}
