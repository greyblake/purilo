# Structured, sized, colored cleanup report

Issue: none (requested in conversation)
Branch: `serhii/structured-cleanup-report` (off local `master` at `600cd11`; `origin/master` lacks
the last three unpushed commits this builds on)

purilo prints one flat `Cleaned <absolute path>` line per item, with no sizes and no structure.
Replace it with a per-section tree (Rust projects, kache, Claude Code tmp, node_modules), each
item with the disk space it freed, section subtotals, and a final summary with the total
reclaimed and free space before/after, colored and bold where it helps. Cleanup rules do not
change; only what is measured and how it is reported.

## Why the current output is bad

Current print sites (all `println!`, no shared format):

| Site | Prints |
|---|---|
| `src/main.rs:34` | `<path> does not exist` (stale `locate` entries) |
| `src/main.rs:40,42` | `Cleaned <path>` / `Failed to clean <path>` - failure reason discarded |
| `src/main.rs:71,72` | kache `Cleaned` / `Failed to clean <path>: <err>` |
| `src/claude.rs:35` | `Skipped <dir>: cannot tell which Claude sessions are running: <err>` |
| `src/claude.rs:58,59,64` | `Cleaned <120-char session path>`, `Kept N Claude session directories...` |
| `src/node_modules.rs:47,64,65,70` | same shapes for node_modules |

Problems, from the user's real run:
- No sizes anywhere, so the run says nothing about what it achieved (it deleted ~29 GiB).
- Claude session paths are ~120 chars of `/home/serhii/.cache/claude-tmp/claude-1000/-home-...`.
- `Failed to clean /usr/share/doc/3cpio` hides why. `cargo clean` there exits non-zero with
  `error: could not find `Cargo.toml` in `/usr/share/doc/3cpio` or any parent directory` on stderr.
- Items skipped for being recently changed (Claude, node_modules) are silent; running sessions are
  a bare count, so the user can't see the 16 GiB held by open Claude sessions.
- `cargo clean`'s own `Removed 15 files, 8.6MiB total` can't be reused: for a target dir that
  `du` measures at 4.6 MB it says 8.6 MiB, because it counts hardlinked files once per link.

## Non-goals

- No change to what gets cleaned or the safety rules (in-use / idle / registry checks).
- No switch of Rust discovery from `locate` to a `$HOME` walk (separate open question). On this
  machine `locate` has no entries under `~` (`updatedb` runs as `nobody`, `/home/serhii` is
  `drwxr-x---`), so the Rust section will be empty; the report says why instead of "nothing to
  clean" (step 5).
- No CLI flags (`--dry-run`, `--json`, `--quiet`); purilo still takes no arguments.
- Free space is shown for the filesystem holding `$HOME` only, not every filesystem touched.
- Rust projects with a custom `target-dir` / `build.build-dir` / `CARGO_TARGET_DIR`, or a
  workspace member with a stale `Cargo.lock`: still `cargo clean`ed, but measured from
  `<project>/target` only, so they report what that dir held.

## Target output

TTY, 90 columns (colors described in brackets):

```
Rust projects                                          2 cleaned · 1 failed · 1.2 GiB  [bold title; size bold green]
├─ ✓ ~/dev/rust/sonicode                                                   1.1 GiB    [✓ green]
├─ ✓ ~/dev/aoc/2024                                                      120.4 MiB
└─ ✗ ~/dev/broken                                                           failed    [red]
     could not find `Cargo.toml` in `/home/serhii/dev/broken` or any parent directory [dim red]
   3 already clean · 1 no longer exists                                               [dim]

kache cache                                                         1 cleaned · 25.1 GiB
└─ ✓ ~/.cache/kache                                                       25.1 GiB

Claude Code tmp                                    12 cleaned · 11 kept · 3.7 GiB
├─ ✓ work-uw-serhii-projects-uw-3d-ads · eb75ce08                          3.5 GiB
├─ ✓ dev-sineborn-music-track-2026-10-13-c4-polish · 54ce425c            182.0 MiB
├─ ● dev-sineborn-music-track-2026-10-13-c4-ffmpeg · 17dd135b   running    4.6 GiB    [● yellow, row dim]
├─ ● work-uw-repos-spider-crab-v2 · 24368707              changed < 24h    2.2 GiB
└─ …

node_modules                                                         nothing to clean  [dim]

━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
Reclaimed 30.0 GiB  ·  15 cleaned · 11 kept · 1 failed  ·  took 41.2s   [size bold green]
Free space on ~  98.1 GiB → 128.1 GiB
```

Rules: within a section, cleaned items sort by freed bytes descending, then kept items by size
descending, then failures. Cleaned items whose path held nothing (Rust projects without
`target/`) are not listed; they are counted as "already clean". A section that could not run
shows `skipped` in its header and the reason under it. Labels longer than the space left are
truncated in the middle with `…` when stdout is a TTY; piped output is aligned to 80 columns and
never truncated. Below a minimum label width (16) rows are not right-aligned at all. Colors
follow `console`'s detection for stdout (off when not a TTY or `NO_COLOR`/`CLICOLOR=0`, forced
by `CLICOLOR_FORCE`).

## 1. Dependencies: `Cargo.toml`

```toml
console = "0.16"        # styles + color detection (NO_COLOR, CLICOLOR, CLICOLOR_FORCE), stdout-only
                        # size (Term::stdout().size_checked), measure_text_width, strip_ansi_codes
indicatif = "0.18"      # spinner on stderr while a section runs; already depends on console 0.16
rustix = { version = "1.1", features = ["fs"] }   # safe statvfs for free space
```

`console` replaces the anstream + anstyle + terminal_size + unicode-width set first planned
(see review log). Output goes through `console::Term::stdout().write_str`, ignoring write
errors: with `purilo | head -3` cleanup keeps going and the rest of the report is dropped,
instead of today's `println!` panic halfway through the cleanup.

## 2. `src/disk_usage.rs` (new): measuring

```rust
// Bytes allocated on disk (st_blocks * 512, like du) in a tree, not following symlinks.
pub struct Measured {
    unlinked: u64,                          // directories and files with nlink == 1
    linked: HashMap<(u64, u64), Linked>,    // files with nlink > 1, by (dev, ino)
}
struct Linked { nlink: u64, links: u64, bytes: u64 }   // links: how many of them are in this tree

impl Measured {
    pub fn total(&self) -> u64;             // unlinked + each linked inode once
}
pub fn measure(path: &Path) -> Measured;

// Space freed over the run. A hardlinked file frees its blocks only when its last link goes,
// which may be in a later item (kache blobs are hardlinked into target/ dirs).
#[derive(Default)]
pub struct DiskUsage { removed_links: HashMap<(u64, u64), Linked>, freed: u64 }

impl DiskUsage {
    // `before` measured before removing, `after` what is left of the path (None if gone).
    // Returns the bytes this removal freed; works for partial removals too.
    pub fn record_removal(&mut self, before: Measured, after: Option<Measured>) -> u64;
    pub fn freed(&self) -> u64;
    // Bytes of removed links whose inode is still linked from outside the removed paths.
    pub fn still_linked(&self) -> u64;
}

// Space available to unprivileged users on the filesystem holding `path`.
pub fn free_space(path: &Path) -> Option<u64>;  // rustix::fs::statvfs: f_bavail * f_frsize
```

Directories never go into `linked` (on ext4 every directory has nlink >= 2). Unreadable entries
contribute 0: measuring must never block cleanup. Every removal - `cargo clean`,
`remove_dir_all` - follows the same shape: `before = measure(p)`, remove, `after =
p.exists().then(|| measure(p))`, `freed = usage.record_removal(before, after)`, so a
`remove_dir_all` that fails half way still credits what it deleted.

## 3. `src/report.rs` (new): model and rendering

```rust
pub struct Section {
    pub title: &'static str,
    pub items: Vec<Item>,
    pub notes: Vec<String>,          // dim lines under the tree ("1 no longer exists")
    pub skipped: Option<String>,     // the section could not run; header says "skipped"
}
pub struct Item { pub label: String, pub status: Status, pub size: u64, pub freed: u64 }
pub enum Status {
    Cleaned,                  // size == 0 -> "already clean", counted, not listed
    Kept(&'static str),       // "running", "in use", "changed < 24h"; size shown, freed 0
    Failed(String),           // reason on a continuation line; freed shown if > 0
}

pub fn render_section(section: &Section, width: Width) -> String;   // styled
pub fn render_summary(summary: &Summary, width: Width) -> String;
pub fn format_size(bytes: u64) -> String;   // "0 B", "1023 B", "1.0 KiB", "25.1 GiB"
pub fn display_path(path: &Path) -> String; // $HOME prefix -> "~", control chars escaped
pub enum Width { Terminal(usize), Piped }   // Terminal: truncate, cap 120; Piped: 80, no truncation
```

Rendering is pure (returns a `String`) so it is unit-tested with `console::strip_ansi_codes`.
Layout math runs on unstyled text with `console::measure_text_width`; styles wrap the already
padded segments. All width arithmetic uses `saturating_sub`; when the label column would be
under 16 columns, the right column follows the label after two spaces instead of aligning.
Labels go through one helper that escapes control characters (a `\n` in a directory name must
not break the tree).

## 4. `src/progress.rs` (new): spinner

Thin wrapper over `indicatif::ProgressBar::new_spinner()` drawing to stderr with a 100 ms steady
tick: `Progress::new()`, `progress.set(section_title, &item_label)`,
`progress.print(&str)` and `progress.finish()`. `print` writes a rendered section inside
`bar.suspend(..)`, which holds the bar's state lock so the ticker can't draw between the clear
and the write; the spinner then resumes for the next section. `finish` calls
`finish_and_clear()` once, before the summary. The spinner is enabled only when stdout and
stderr are both terminals (`console::Term::{stdout,stderr}().is_term()`); otherwise
`ProgressBar::hidden()`, so `purilo | tee log` and `| less` never mix spinner frames into the
report.

## 5. Cleaners return a `Section` instead of printing

Signature for all four: `pub fn clean(usage: &mut DiskUsage, progress: &Progress) -> Section`.

- **`src/cargo_projects.rs`** (moved out of `main.rs:13-58`): same `locate` + filters, and no
  more `expect`s, which today panic and skip every later section:
  - `locate` can't start -> `skipped: "cannot run locate: <err>"`. Non-zero exit with non-empty
    stderr -> `skipped: <stderr>` (exit 1 with empty stderr just means no matches). Output read
    with `String::from_utf8_lossy`.
  - No projects left after filtering -> run `locate -l 1 <HOME>/`; if that finds nothing, note
    "locate's database has no entries under ~" so an empty section is explained.
  - Per project: `progress.set`, measure `project/target`, run
    `cargo clean --color never`. `cargo` can't start -> `Failed("cannot run cargo: <err>")`.
    Non-zero exit -> `Failed(reason)`: the first stderr line starting with `error: ` (prefix
    stripped), else the last non-empty line. The last line alone is a caret marker for a
    malformed `Cargo.toml`.
  - Missing project dirs -> note "N no longer exist(s)".
- **`src/kache.rs`** (moved out of `main.rs:60-85`): one item, label `display_path(dir)`.
- **`src/claude.rs`**: running session -> `Kept("running")` with `measure(dir).total()`; not
  idle -> `Kept("changed < 24h")` (today silent); removed -> `Cleaned`; registry error ->
  `skipped` instead of `println!`. Label: project slug with the `$HOME` slug plus `-` stripped
  (a session in `$HOME` itself -> `~`), then ` · ` and the first 8 chars of the session id. The
  `$HOME` slug replaces every non-`is_ascii_alphanumeric` char with `-`, which matches all 73
  directories under `~/.claude/projects` here. Slugs are lossy (`-` is ambiguous); the label is
  for recognition, not a path.
- **`src/node_modules.rs`**: in use -> `Kept("in use")`; not idle -> `Kept("changed < 24h")`
  (today silent); "cannot tell which files are in use" -> `skipped`; label =
  `display_path(project)`. Kept items are measured too (a cold walk of next-web's 1.7 GiB
  node_modules took 3.8 s here) - showing what is held is the point of listing them.

## 6. `src/main.rs`: orchestration

```rust
fn main() {
    let started = Instant::now();
    let home = non_empty_env("HOME");
    let free_before = home.as_deref().and_then(disk_usage::free_space);
    let width = report::Width::detect();   // Term::stdout().size_checked(), stdout only
    let mut usage = DiskUsage::default();
    let progress = Progress::new();
    let mut sections = Vec::new();
    for clean in [cargo_projects::clean, kache::clean, claude::clean, node_modules::clean] {
        let section = clean(&mut usage, &progress);
        progress.print(&report::render_section(&section, width));
        sections.push(section);
    }
    progress.finish();
    let free_after = home.as_deref().and_then(disk_usage::free_space);
    report::print(&report::render_summary(&Summary { ... }, width));
}
```

`non_empty_env` stays in `main.rs` (modules already use `crate::non_empty_env`).

## Verification

```bash
cargo fmt -- --check
cargo clippy --all-targets
cargo test
```

Unit tests: `format_size` boundaries; middle truncation with wide chars; `render_section`
(ANSI stripped with `console::strip_ansi_codes`) for a mixed section, an empty section, a
skipped section, notes, `Width::Piped` vs `Terminal(60)` vs `Terminal(20)` (no panic, no
alignment); control characters in a label; `DiskUsage`: a file hardlinked inside one tree counts
once, a file hardlinked from outside every removed tree frees 0 and shows in `still_linked`, the
second of two removed trees completes the inode and gets the credit, a partial removal credits
only what is gone; failure-reason extraction from real cargo stderr samples (malformed
`Cargo.toml`, missing workspace member, missing `Cargo.toml`); Claude label from slug, including
`$HOME` itself.

End to end, never touching real data: stub `locate` in the scratchpad printing `Cargo.lock`
paths of scratch Rust projects (one built, one without `target/`, one without `Cargo.toml`),
`KACHE_CACHE_DIR`, `CLAUDE_CODE_TMPDIR`, `CLAUDE_CONFIG_DIR` and `HOME` all pointing at a
scratch tree with a live `sleep` session record and node_modules projects. Run three ways:
piped (no ANSI, no truncation), `CLICOLOR_FORCE=1` (ANSI present), and under
`script -qc 'stty cols 90 rows 40; …' /dev/null` (a pty with a real size: truncation and
spinner; without `stty` the pty reports 0x0). Compare reported sizes with `du -s
--block-size=1` taken before, and the summary's freed total with the `statvfs` delta.

## Risks and notes

- Measuring adds a full walk of everything removed and kept (Claude session dirs are walked
  twice before removal: the idle check and the measurement). Cold-cache cost measured: 3.8 s
  for next-web's 1.7 GiB node_modules. Merging the idle walk with the measurement is possible
  later if it matters.
- `st_blocks * 512` is what deleting frees on ext4 (checked: +300,007,424 B free after
  deleting a 300 MB file). On a copy-on-write filesystem, reflinked extents (kache logs
  `reflinked_bytes`) are shared and freed bytes are overstated; the free-space line is the
  ground truth.
- The glyphs (`✓ ✗ ● ├ └ … → ━`) assume a UTF-8 terminal; piped logs carry them too.
- Walking kept Claude sessions reads directories other running sessions write to; read-only,
  errors ignored.
- Free space before/after includes writes by other processes during the run; it is labelled as
  free space, not as reclaimed.
- The installed `~/.cargo/bin/purilo` dates from 2026-05-11 and has none of this branch's
  sections, so the user's pasted run came from another build; `cargo install --path .` after
  merging either way.

## Review log

- **`terminal_size()` falls back to stderr/stdin, so `purilo > file` would truncate** - accepted.
  Confirmed in terminal_size-0.4.4/src/unix.rs:6-19. Width now comes from
  `console::Term::stdout().size_checked()` (stdout only).
- **Clearing the spinner between sections either keeps the ticker drawing or kills it** -
  accepted. Sections are written inside `bar.suspend(..)`; `finish_and_clear()` once at the end.
- **Stale-binary evidence was wrong** - accepted. Reworded; the advice to reinstall stands.
- **`nlink > 1` also catches every directory** - accepted. Directories never go into `linked`.
- **Reclaimed credited a hardlinked inode on its first removed link, not its last** - accepted
  (high). kache logs hardlinked bytes, so `rm -rf ~/.cache/kache` can leave blobs alive via
  `target/` dirs. `DiskUsage` now counts removed links per inode, credits on the last one, and
  reports the remainder as `still_linked`.
- **Last stderr line is the wrong failure reason** - accepted. First `error: ` line, else the
  last non-empty line; `cargo clean --color never`.
- **Section-level errors and `expect`s not mapped** - accepted. `Section::skipped`, no
  `expect`s in the Rust section, `locate` exit status checked (exit 1 + empty stderr = no matches).
- **A partial `remove_dir_all` failure would report 0 bytes** - accepted. Measure what is left
  and credit the difference, for every removal.
- **Width arithmetic underflows on narrow terminals** - accepted. `saturating_sub`, minimum
  label width 16, no alignment below it.
- **Claude label edge cases** - partly accepted: `-` boundary, `$HOME` itself -> `~`,
  `is_ascii_alphanumeric`. Rejected reading `cwd` from the session transcript: more I/O and
  parsing for a display label; the slug is recognisable.
- **Control characters in labels break the tree** - accepted. Escaped in `display_path`.
- **`locate` finds nothing under `~` here, so the Rust section is always empty** - accepted as a
  reporting fix (a note explains it). Changing discovery stays out of scope.
- **`script -qc` without `stty` reports 0x0** - accepted. Verification sets `stty cols 90`.
- **Use `console` instead of anstream/anstyle/terminal_size/unicode-width** - accepted. Verified
  `NO_COLOR` (console-0.16.6/src/unix_term.rs:30), `CLICOLOR`/`CLICOLOR_FORCE` (utils.rs:15-19),
  `measure_text_width`, `strip_ansi_codes`, `Term::size_checked`; indicatif 0.18 already
  depends on console 0.16. Kept `rustix` for a safe `statvfs`.
- **Drop the spinner** - rejected. A real run takes tens of seconds with no output otherwise;
  enabling it only when stdout and stderr are both terminals removes the tee/less problem.
- **`purilo | tee` alignment at 100 columns wraps on 80-column terminals** - accepted: piped
  output aligns to 80.
- **Broken pipe behaviour** - accepted as documented behaviour: write errors are ignored and
  cleanup continues (today `println!` panics halfway through cleaning).
- **Kept node_modules sizing adds a full walk** - rejected the skip: measured 3.8 s cold for the
  largest one here, and the size is the reason to list it.

## Implementation notes

All steps are implemented (`src/{disk_usage,report,progress,cargo_projects,kache}.rs` new,
`src/{main,claude,node_modules}.rs` changed). Deviations from the steps above:

- **`DiskUsage` keys every file by inode, and credits by the link count read before each
  removal.** As planned it accumulated removed links per inode against the `nlink` first seen.
  The unit test `credits_the_removal_of_the_last_link` failed: once the first of two links is
  removed, the second tree sees the file with `nlink == 1`, so it was credited as an ordinary
  file while the stale entry kept reporting it in `still_linked`. Now `Measured` has `dirs` plus
  `files: HashMap<(dev, ino), File>` for every non-directory, a removal frees a file when it
  removes as many links as the file had right before, and `still_linked` holds only inodes
  whose latest removal left links behind. The kernel's live `nlink` does the cross-item
  bookkeeping. `DiskUsage::freed()` was dropped; the summary sums the items.
- **`DiskUsage::remove(path, remove_fn) -> Removal`** wraps measure / remove / re-measure /
  record, so `cargo_projects`, `kache`, `claude` and `node_modules` share one shape;
  `Item::removed(label, Removal)` turns it into a row.
- **Control characters are escaped when rendering** (labels, reasons, notes), not in
  `display_path`, so labels from any source are covered.
- **`render_summary(sections, summary, width)`** takes the sections and derives the counts and
  the total itself.

Verification run (fake environment in the scratchpad, real `cargo`):

| Check | Result |
|---|---|
| `cargo fmt -- --check` | clean |
| `cargo clippy --all-targets` | no warnings |
| `cargo test` | 18 passed |
| Piped run | no ANSI, no truncation, sizes equal `du` before (4,726,784 B target = 4.5 MiB; kache 50,016,256 B minus the 8 MB blob hardlinked from a kept dir = 40.1 MiB freed, 7.6 MiB reported still linked) |
| `CLICOLOR_FORCE=1` piped | 19 lines with ANSI |
| pty `stty cols 90` | every row 90 columns, middle truncation, spinner animates |
| pty + `NO_COLOR=1` | no color codes |
| `run | head -3` | exit 0, last section still cleaned |

## Code review log

Two fresh reviewers (line-level; failure modes and data safety). Both confirmed the keep/remove
predicates and their order are unchanged versus `600cd11`, and that the cross-item hardlink
accounting is right.

- **Check-to-delete gap widened: a full measuring walk ran between the in-use / running / idle
  checks and the removal, and kept items were measured inside the loop** (both reviewers) -
  fixed. `DiskUsage::remove(path, before, remove)` now takes the measurement, so cleaners
  measure first and check last: Claude re-reads the session registry and node_modules re-scans
  `/proc` right before each removal (the start-of-run snapshot only short-circuits known
  running / in-use items and decides "skipped"). A `/proc` scan is ~0.5 s here, paid only for
  removal candidates.
- **A removed directory that held 0 blocks (tmpfs dirs, empty files) was shown as "already
  clean"** - fixed. `Removal::existed`; `Status::AlreadyClean` is set only when nothing was at
  the path. Test `lists_a_removed_path_that_held_no_blocks`, `tells_a_missing_path_from_an_empty_one`.
- **A failing idle check reported "changed < 24h"** - fixed. Both `is_idle` return
  `io::Result<bool>`; errors give `Kept("cannot check")` (`report::CANNOT_CHECK`).
- **Non-UTF-8 paths from `locate` reported as "no longer exist" and not cleaned** - fixed.
  `project_paths(&Output)` splits bytes and uses `OsStr::from_bytes`; tests cover a `\xe9` path,
  the filters, and exit 1 with and without stderr.
- **Spinner passed raw directory names (escape sequences) to the terminal** (both) - fixed.
  `Progress::set` escapes; verified in a pty with an OSC title sequence in a project name.
- **A killed run left no record of deletions done so far** - fixed for Ctrl-C/SIGTERM/SIGHUP
  (`ctrlc` with `termination`): the first signal stops before the next item, the report and
  summary still print ("interrupted" note, "Interrupted: ..." line), exit code 130; a second
  signal exits at once. Verified with a slow `cargo` wrapper: the running `cargo clean`
  finished, later sections untouched. SIGKILL/OOM still lose the report (not streamed: the tree
  is sorted per section).
- **Memory: a map entry per file** - fixed. Only files with `nlink > 1`, plus files tracked from
  an earlier partial removal (`DiskUsage::measure`) or from `before` (the after-walk), are keyed
  by inode; the rest is a running sum. The walk keeps a stack of `ReadDir`s and uses
  `DirEntry::metadata` (no symlink following), so memory grows with depth.
- **Two disk tests passed only because `/tmp` is tmpfs** - fixed. Tests use a temp dir under
  `target/` and assert directories take blocks there. Added a test for a hardlinked pair
  half-removed by a failed removal.
- **Reasons/notes truncation and escaping untested; fake "file lock" stderr sample** - fixed.
  Test `cuts_long_reasons_and_escapes_notes`; real `could not find Cargo.toml` sample.
- **"still in use ... files that were kept" wording** - fixed: "still on disk: other hardlinks
  to them remain"; summary lines fit the width.
- **`HOME` with a trailing slash broke Claude labels** - fixed (trimmed before slugging).
- **Spinner drew section and item in two steps** - fixed: one message.
- **Terminal width read once** - fixed: detected per render.
- **Overflow on garbage sizes** - fixed: saturating sums and `blocks * 512`, `checked_mul` in
  `free_space`.
- **A second purilo at the same time made removed roots "Failed: No such file"** - fixed:
  `disk_usage::remove_dir_all` treats NotFound as done (also dedupes the three closures).
- **Loose version requirements; `Term::write_str` is `#[doc(hidden)]`; repeated literals** -
  fixed: pinned `0.16.6` / `0.18.6` / `1.1.5`, `io::stdout().write_all` + flush, `NOT_IDLE`
  next to `MIN_IDLE`.
- **Don't count files on other mounts (`du -x`)** - rejected: removing a tmpfs tree does free
  that space (memory); the free-space line shows the `~` filesystem separately.
- **Measure `cargo metadata`'s `target_directory`** - deferred: listed as a non-goal; one extra
  cargo call per project.
- **Files still held open by a process count as freed; two runs both credit the same bytes** -
  deferred: rare, and the free-space line is the ground truth.
- **Closed stdout: cleanup continues silently** - kept as a decision (the run was asked for);
  previously it panicked mid-run.

Re-verification after the fixes: `cargo fmt -- --check` clean, `cargo clippy --all-targets` no
warnings, `cargo test` 23 passed; e2e piped (same numbers as before, no ANSI), pty at 100
columns (aligned, escaped), Ctrl-C (exit 130, partial report).

## Code review log, round 2

One fresh reviewer on the fix round (the fixes changed the check order, the measuring
structure and added signal handling). Keep/remove predicates confirmed unchanged except the
intended extra re-checks.

- **A Ctrl-C during an item's measure/idle walk still deleted that item** - fixed. Every
  cleaner checks `interrupted()` again right before removing; the item is then listed as
  `Kept("interrupted")` and the section stops (`Section::interrupted`).
- **`ctrlc::set_handler` overrode signals the parent ignored (`nohup purilo &` stopped early on
  SIGHUP)** - fixed with `try_set_handler`, which installs nothing and restores the old
  handlers unless INT/TERM/HUP are all at their defaults. Verified: with SIGHUP ignored, a
  `kill -HUP` leaves the run to completion.
- **A terminal Ctrl-C also kills the running `cargo clean`, reported as "cargo clean failed"** -
  fixed: a failed `cargo clean` after an interrupt reports "interrupted", another signal "cargo
  was killed by signal N". The round-1 log line "the running `cargo clean` finished" holds only
  for a signal sent to purilo alone; verified both ways.
- **`locate` killed by a signal or exiting 2 with empty stderr was taken as "no matches" /
  truncated output was used** - fixed: only exit 0 is parsed and exit 1 with empty stderr means
  no matches; anything else skips the section. Test with `ExitStatus::from_raw(2)`.
- **A path removed by someone else between measuring and removing was credited in full** -
  fixed: `Removal` reports size and freed 0 when nothing was at the path. Test
  `does_not_credit_a_path_removed_by_someone_else`.
- **"Interrupted" shown when nothing was skipped; a signal during discovery showed "nothing to
  clean"** - fixed: the summary line appears only when a section or the section loop actually
  stopped early; a section stopped before any item shows "interrupted" in its header.
- **Tests that would not catch reverts** - fixed: the keep decision is now the pure
  `keep::keep_reason` (busy snapshot, idle, fresh busy check last; any error keeps) with tests;
  `Item::removed` tested by `existed` not size; an empty file (0 blocks) removed through
  `DiskUsage::remove`; NotFound root gives no error; single-link trees keep the inode map empty;
  spinner escaping; `$HOME` trailing slash. `MIN_IDLE`, `NOT_IDLE`, `CANNOT_CHECK` and
  `INTERRUPTED` now live in `src/keep.rs`.
- **Comments: "memory grows with depth, not size" and a misplaced symlink comment** - fixed.
- **A failed removal that deletes one of two in-tree links adds the inode to `still_linked`** -
  rejected: the file is still on disk through its other link, which is what the line says.
- **Exit code 130 for SIGTERM/SIGHUP too** - rejected: `ctrlc` does not say which signal arrived.

Final verification: `cargo fmt -- --check` clean, `cargo clippy --all-targets` no warnings,
`cargo test` 32 passed. Fake-environment runs with a freshly built binary: piped (same numbers
as before, no ANSI); SIGINT to purilo alone (current `cargo clean` finishes, later sections
untouched, exit 130); SIGINT to the process group (row "interrupted"); SIGHUP while ignored by
the parent (ignored, run completes).
