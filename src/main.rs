use std::process::{Command, Output};
use std::path::Path;

fn main() {
    let output = Command::new("locate")
        .arg("Cargo.lock")
        .output()
        .expect("Failed to run `locate Cargo.lock`");

    // TODO: consider using OsString
    let stdout = String::from_utf8(output.stdout).expect("Invalid UTF-8");

    let project_paths: Vec<&Path> =  stdout
        .lines()
        .filter(|line| !line.is_empty() && !line.contains(".cargo") && !line.contains(".rustup"))
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
}


fn cargo_clean(rust_project_path: &Path) -> Output {
    Command::new("cargo")
        .arg("clean")
        .current_dir(rust_project_path)
        .output()
        .expect("Failed to run `cargo clean`")
}
