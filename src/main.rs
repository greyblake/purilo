use std::env;
use std::path::PathBuf;
use std::process;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

mod cargo_projects;
mod claude;
mod disk_usage;
mod kache;
mod keep;
mod node_modules;
mod progress;
mod projects;
mod report;

use disk_usage::DiskUsage;
use progress::Progress;
use report::{Summary, Width};

static INTERRUPTED: AtomicBool = AtomicBool::new(false);

// Set by the first Ctrl-C (or SIGTERM/SIGHUP): cleaning stops before the next item and the
// report of what was done is still printed. A second one exits right away.
fn interrupted() -> bool {
    INTERRUPTED.load(Ordering::Relaxed)
}

fn main() {
    // try_: signals a parent set to be ignored (e.g. SIGHUP under nohup) stay ignored.
    let _ = ctrlc::try_set_handler(|| {
        if INTERRUPTED.swap(true, Ordering::Relaxed) {
            process::exit(130);
        }
    });

    let started = Instant::now();
    let home = non_empty_env("HOME");
    let free_before = home.as_deref().and_then(disk_usage::free_space);
    let progress = Progress::new();
    let mut usage = DiskUsage::default();

    let mut sections = Vec::new();
    let mut stopped_early = false;
    for clean in [
        cargo_projects::clean,
        kache::clean,
        claude::clean,
        node_modules::clean,
    ] {
        if interrupted() {
            stopped_early = true;
            break;
        }
        let section = clean(&mut usage, &progress);
        progress.print(&report::render_section(&section, Width::detect()));
        sections.push(section);
    }
    progress.finish();

    let free_after = home.as_deref().and_then(disk_usage::free_space);
    let summary = Summary {
        still_linked: usage.still_linked(),
        interrupted: stopped_early || sections.iter().any(|section| section.interrupted),
        elapsed: started.elapsed(),
        free_space: free_before.zip(free_after),
    };
    report::print(&report::render_summary(
        &sections,
        &summary,
        Width::detect(),
    ));
    if summary.interrupted {
        process::exit(130);
    }
}

fn non_empty_env(name: &str) -> Option<PathBuf> {
    env::var_os(name)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}
