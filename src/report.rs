// The cleanup report: per section, a tree of what was cleaned, kept or failed and the space
// it took, then a summary of the whole run.

use std::cmp::Reverse;
use std::io::{self, Write};
use std::path::Path;
use std::time::Duration;

use console::{Style, Term, measure_text_width, style};

use crate::disk_usage::Removal;
use crate::non_empty_env;

const MAX_WIDTH: usize = 120;
const PIPED_WIDTH: usize = 80;
// Below this the right column is not aligned, so a narrow terminal wraps instead of breaking.
const MIN_LABEL_WIDTH: usize = 16;
const GAP: usize = 2;
const GIB: u64 = 1 << 30;

pub struct Section {
    pub title: &'static str,
    pub items: Vec<Item>,
    // Shown dimmed under the tree.
    pub notes: Vec<String>,
    // Why the section could not run at all.
    pub skipped: Option<String>,
    // Stopped early on Ctrl-C.
    pub interrupted: bool,
}

impl Section {
    pub fn new(title: &'static str) -> Self {
        Self {
            title,
            items: Vec::new(),
            notes: Vec::new(),
            skipped: None,
            interrupted: false,
        }
    }

    pub fn skipped(title: &'static str, reason: String) -> Self {
        Self {
            skipped: Some(reason),
            ..Self::new(title)
        }
    }
}

pub struct Item {
    pub label: String,
    pub status: Status,
    // What the path held before cleaning.
    pub size: u64,
    pub freed: u64,
}

pub enum Status {
    Cleaned,
    // There was nothing to remove; counted, not listed.
    AlreadyClean,
    Kept(&'static str),
    // `freed` is what was removed before the failure.
    Failed(String),
}

impl Item {
    pub fn removed(label: String, removal: Removal) -> Self {
        let status = match (removal.error, removal.existed) {
            (Some(reason), _) => Status::Failed(reason),
            (None, true) => Status::Cleaned,
            (None, false) => Status::AlreadyClean,
        };
        Self {
            label,
            status,
            size: removal.size,
            freed: removal.freed,
        }
    }

    pub fn kept(label: String, reason: &'static str, size: u64) -> Self {
        Self {
            label,
            status: Status::Kept(reason),
            size,
            freed: 0,
        }
    }
}

pub struct Summary {
    // Removed files whose space is not freed because other links to them remain.
    pub still_linked: u64,
    // Something was left undone because of Ctrl-C.
    pub interrupted: bool,
    pub elapsed: Duration,
    // Free space on the filesystem holding $HOME, before and after the run.
    pub free_space: Option<(u64, u64)>,
}

#[derive(Clone, Copy)]
pub enum Width {
    // Columns of the terminal stdout is attached to; long labels are truncated.
    Terminal(usize),
    // stdout is a file or a pipe; nothing is truncated.
    Piped,
}

impl Width {
    pub fn detect() -> Self {
        match Term::stdout().size_checked() {
            Some((_rows, columns)) => Self::Terminal(usize::from(columns)),
            None => Self::Piped,
        }
    }

    fn columns(self) -> usize {
        match self {
            Self::Terminal(columns) => columns.min(MAX_WIDTH),
            Self::Piped => PIPED_WIDTH,
        }
    }
}

// Write errors (e.g. a closed pipe) are ignored, so cleaning carries on.
pub fn print(text: &str) {
    let mut stdout = io::stdout().lock();
    let _ = stdout
        .write_all(text.as_bytes())
        .and_then(|()| stdout.flush());
}

pub fn render_section(section: &Section, width: Width) -> String {
    let counts = Counts::of(&section.items);
    let header_right = if section.skipped.is_some() {
        style("skipped").yellow().to_string()
    } else if section.interrupted && section.items.is_empty() {
        style("interrupted").yellow().to_string()
    } else {
        counts.describe()
    };
    let mut out = row(
        "",
        section.title,
        &Style::new().bold().cyan(),
        &header_right,
        width,
    );

    if let Some(reason) = &section.skipped {
        let reason = fit_end(&escape_control(reason), 5, width);
        out += &format!(
            "   {} {}\n\n",
            style("!").yellow().bold(),
            style(reason).yellow()
        );
        return out;
    }

    let mut items: Vec<&Item> = section
        .items
        .iter()
        .filter(|item| !matches!(item.status, Status::AlreadyClean))
        .collect();
    items.sort_by_key(|item| match item.status {
        Status::Cleaned | Status::AlreadyClean => (0, Reverse(item.freed)),
        Status::Kept(_) => (1, Reverse(item.size)),
        Status::Failed(_) => (2, Reverse(0)),
    });

    for (index, item) in items.iter().enumerate() {
        let last = index + 1 == items.len();
        let branch = style(if last { "└─ " } else { "├─ " }).dim();
        let (marker, label_style, right) = match &item.status {
            Status::Cleaned | Status::AlreadyClean => {
                (style("✓").green(), Style::new(), size(item.freed))
            }
            Status::Kept(reason) => (
                style("●").yellow(),
                Style::new().dim(),
                format!(
                    "{}  {}",
                    style(reason).yellow().dim(),
                    style(format_size(item.size)).dim()
                ),
            ),
            Status::Failed(_) if item.freed > 0 => (
                style("✗").red().bold(),
                Style::new(),
                format!("{} · freed {}", style("failed").red(), size(item.freed)),
            ),
            Status::Failed(_) => (
                style("✗").red().bold(),
                Style::new(),
                style("failed").red().to_string(),
            ),
        };
        out += &row(
            &format!("{branch}{marker} "),
            &item.label,
            &label_style,
            &right,
            width,
        );

        if let Status::Failed(reason) = &item.status {
            let continuation = style(if last { "     " } else { "│    " }).dim();
            let reason = fit_end(&escape_control(reason), 5, width);
            out += &format!("{continuation}{}\n", style(reason).red().dim());
        }
    }

    let mut notes = Vec::new();
    if counts.already_clean > 0 {
        notes.push(format!("{} already clean", counts.already_clean));
    }
    notes.extend(section.notes.iter().map(|note| escape_control(note)));
    if !notes.is_empty() {
        out += &format!("   {}\n", style(notes.join(" · ")).dim());
    }
    if section.interrupted && !section.items.is_empty() {
        out += &format!("   {}\n", style("interrupted").yellow());
    }
    out += "\n";
    out
}

pub fn render_summary(sections: &[Section], summary: &Summary, width: Width) -> String {
    let counts = Counts::of(sections.iter().flat_map(|section| &section.items));
    let mut out = format!("{}\n", style("━".repeat(width.columns())).dim());

    let mut parts = vec![format!(
        "{} {}",
        style("Reclaimed").bold(),
        style(format_size(counts.freed)).green().bold()
    )];
    let tallies = counts.tallies();
    if !tallies.is_empty() {
        parts.push(tallies.join(" · "));
    }
    parts.push(format!("took {}", format_duration(summary.elapsed)));
    out += &format!("{}\n", parts.join("  ·  "));

    if let Some((before, after)) = summary.free_space {
        out += &format!(
            "Free space on ~  {} → {}\n",
            format_size(before),
            style(format_size(after)).bold()
        );
    }
    if summary.still_linked > 0 {
        let note = format!(
            "{} of removed files is still on disk: other hardlinks to them remain",
            format_size(summary.still_linked)
        );
        out += &format!("{}\n", style(fit_end(&note, 0, width)).yellow());
    }
    if summary.interrupted {
        let note = "Interrupted: stopped before cleaning everything";
        out += &format!("{}\n", style(fit_end(note, 0, width)).yellow().bold());
    }
    out
}

#[derive(Default)]
struct Counts {
    cleaned: usize,
    already_clean: usize,
    kept: usize,
    failed: usize,
    freed: u64,
}

impl Counts {
    fn of<'a>(items: impl IntoIterator<Item = &'a Item>) -> Self {
        let mut counts = Self::default();
        for item in items {
            counts.freed = counts.freed.saturating_add(item.freed);
            match item.status {
                Status::Cleaned => counts.cleaned += 1,
                Status::AlreadyClean => counts.already_clean += 1,
                Status::Kept(_) => counts.kept += 1,
                Status::Failed(_) => counts.failed += 1,
            }
        }
        counts
    }

    fn tallies(&self) -> Vec<String> {
        let mut tallies = Vec::new();
        if self.cleaned > 0 {
            tallies.push(format!("{} cleaned", self.cleaned));
        }
        if self.kept > 0 {
            tallies.push(format!("{} kept", self.kept));
        }
        if self.failed > 0 {
            tallies.push(style(format!("{} failed", self.failed)).red().to_string());
        }
        tallies
    }

    // "2 cleaned · 1 kept · 1 failed · 1.2 GiB"
    fn describe(&self) -> String {
        let mut parts = self.tallies();
        if self.cleaned > 0 || self.freed > 0 {
            parts.push(size(self.freed));
        }
        if parts.is_empty() {
            return style("nothing to clean").dim().to_string();
        }
        parts.join(" · ")
    }
}

// One line: `lead` (already styled), the label, and `right` (already styled) aligned to the
// last column. On a terminal a long label is truncated in the middle.
fn row(lead: &str, label: &str, label_style: &Style, right: &str, width: Width) -> String {
    let label = escape_control(label);
    let used = measure_text_width(lead) + GAP + measure_text_width(right);
    let space = width.columns().saturating_sub(used);
    if space < MIN_LABEL_WIDTH {
        let gap = " ".repeat(GAP);
        return format!("{lead}{}{gap}{right}\n", label_style.apply_to(label));
    }

    let label = match width {
        Width::Terminal(_) => truncate_middle(&label, space),
        Width::Piped => label,
    };
    let padding = " ".repeat(
        (space + GAP)
            .saturating_sub(measure_text_width(&label))
            .max(GAP),
    );
    format!("{lead}{}{padding}{right}\n", label_style.apply_to(label))
}

// Cuts the end of a text that starts `indent` columns in, on a terminal only.
fn fit_end(text: &str, indent: usize, width: Width) -> String {
    let space = width.columns().saturating_sub(indent);
    match width {
        Width::Terminal(_) if space >= MIN_LABEL_WIDTH && measure_text_width(text) > space => {
            format!("{}…", take_width(text.chars(), space - 1))
        }
        _ => text.to_string(),
    }
}

fn size(bytes: u64) -> String {
    let text = style(format_size(bytes)).green();
    if bytes >= GIB {
        text.bold().to_string()
    } else {
        text.to_string()
    }
}

// "0 B", "1023 B", "1.0 KiB", "25.1 GiB"
pub fn format_size(bytes: u64) -> String {
    const UNITS: [&str; 6] = ["KiB", "MiB", "GiB", "TiB", "PiB", "EiB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64 / 1024.0;
    let mut unit = 0;
    // 1023.95 and up would print as "1024.0" of the smaller unit.
    while value >= 1023.95 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    format!("{value:.1} {}", UNITS[unit])
}

fn format_duration(duration: Duration) -> String {
    let seconds = duration.as_secs();
    if seconds < 60 {
        format!("{:.1}s", duration.as_secs_f64())
    } else {
        format!("{}m {:02}s", seconds / 60, seconds % 60)
    }
}

// The path with $HOME shown as "~".
pub fn display_path(path: &Path) -> String {
    match non_empty_env("HOME") {
        Some(home) => display_path_in(path, &home),
        None => path.display().to_string(),
    }
}

fn display_path_in(path: &Path, home: &Path) -> String {
    match path.strip_prefix(home) {
        Ok(rest) if rest.as_os_str().is_empty() => "~".to_string(),
        Ok(rest) => format!("~/{}", rest.display()),
        Err(_) => path.display().to_string(),
    }
}

// A newline or escape sequence in a directory name must not break the report.
pub fn escape_control(text: &str) -> String {
    text.chars()
        .map(|c| {
            if c.is_control() {
                c.escape_default().to_string()
            } else {
                c.to_string()
            }
        })
        .collect()
}

// Shortens `text` to at most `max` columns by replacing its middle with "…".
fn truncate_middle(text: &str, max: usize) -> String {
    if measure_text_width(text) <= max {
        return text.to_string();
    }
    let Some(keep) = max.checked_sub(1) else {
        return String::new();
    };
    let head_width = keep.div_ceil(2);
    let head = take_width(text.chars(), head_width);
    let tail: String = take_width(text.chars().rev(), keep - head_width)
        .chars()
        .rev()
        .collect();
    format!("{head}…{tail}")
}

fn take_width(chars: impl Iterator<Item = char>, max: usize) -> String {
    let mut width = 0;
    let mut taken = String::new();
    for c in chars {
        width += measure_text_width(c.encode_utf8(&mut [0; 4]));
        if width > max {
            break;
        }
        taken.push(c);
    }
    taken
}

#[cfg(test)]
mod tests {
    use super::*;
    use console::strip_ansi_codes;

    const MIB: u64 = 1 << 20;

    fn plain(text: &str) -> Vec<String> {
        strip_ansi_codes(text)
            .lines()
            .map(|line| line.trim_end().to_string())
            .collect()
    }

    fn cleaned(label: &str, freed: u64) -> Item {
        Item {
            label: label.to_string(),
            status: Status::Cleaned,
            size: freed,
            freed,
        }
    }

    fn already_clean(label: &str) -> Item {
        Item {
            label: label.to_string(),
            status: Status::AlreadyClean,
            size: 0,
            freed: 0,
        }
    }

    fn failed(label: &str, reason: &str, freed: u64) -> Item {
        Item {
            label: label.to_string(),
            status: Status::Failed(reason.to_string()),
            size: 10 * MIB,
            freed,
        }
    }

    fn mixed_section() -> Section {
        Section {
            title: "Rust projects",
            items: vec![
                failed(
                    "~/dev/broken",
                    "could not find `Cargo.toml` in `~/dev/broken`",
                    0,
                ),
                cleaned("~/dev/small", 120 * MIB),
                Item::kept("~/dev/busy".to_string(), "in use", 3 * GIB),
                already_clean("~/dev/empty"),
                cleaned("~/dev/big", 2 * GIB),
            ],
            notes: vec!["1 no longer exists".to_string()],
            skipped: None,
            interrupted: false,
        }
    }

    #[test]
    fn formats_sizes() {
        assert_eq!(format_size(0), "0 B");
        assert_eq!(format_size(1023), "1023 B");
        assert_eq!(format_size(1024), "1.0 KiB");
        assert_eq!(format_size(1536), "1.5 KiB");
        assert_eq!(format_size(MIB - 1), "1.0 MiB");
        assert_eq!(format_size(25 * GIB + GIB / 10), "25.1 GiB");
        assert_eq!(format_size(u64::MAX), "16.0 EiB");
    }

    #[test]
    fn formats_durations() {
        assert_eq!(format_duration(Duration::from_millis(41_230)), "41.2s");
        assert_eq!(format_duration(Duration::from_secs(185)), "3m 05s");
    }

    #[test]
    fn renders_a_section_as_a_sorted_tree() {
        let lines = plain(&render_section(&mixed_section(), Width::Terminal(60)));
        assert_eq!(
            lines,
            [
                "Rust projects        2 cleaned · 1 kept · 1 failed · 2.1 GiB",
                "├─ ✓ ~/dev/big                                       2.0 GiB",
                "├─ ✓ ~/dev/small                                   120.0 MiB",
                "├─ ● ~/dev/busy                              in use  3.0 GiB",
                "└─ ✗ ~/dev/broken                                     failed",
                "     could not find `Cargo.toml` in `~/dev/broken`",
                "   1 already clean · 1 no longer exists",
                "",
            ]
        );
        for line in &lines[..5] {
            assert_eq!(measure_text_width(line), 60, "{line}");
        }
    }

    #[test]
    fn shows_what_a_failed_removal_freed() {
        let section = Section {
            items: vec![failed("~/x", "Permission denied (os error 13)", 3 * MIB)],
            ..Section::new("kache cache")
        };
        let lines = plain(&render_section(&section, Width::Piped));
        assert!(lines[0].ends_with("1 failed · 3.0 MiB"), "{lines:?}");
        assert!(lines[1].ends_with("failed · freed 3.0 MiB"), "{lines:?}");
        assert_eq!(lines[2], "     Permission denied (os error 13)");
    }

    #[test]
    fn truncates_long_labels_on_a_terminal_only() {
        let long = format!("~/{}", "a".repeat(100));
        let section = Section {
            items: vec![cleaned(&long, MIB)],
            ..Section::new("node_modules")
        };

        let terminal = plain(&render_section(&section, Width::Terminal(50)));
        assert_eq!(measure_text_width(&terminal[1]), 50);
        assert!(terminal[1].contains('…'), "{terminal:?}");

        let piped = plain(&render_section(&section, Width::Piped));
        assert!(piped[1].contains(&long), "{piped:?}");
    }

    #[test]
    fn does_not_align_on_a_very_narrow_terminal() {
        let lines = plain(&render_section(&mixed_section(), Width::Terminal(20)));
        assert_eq!(lines[1], "├─ ✓ ~/dev/big  2.0 GiB");
    }

    #[test]
    fn renders_empty_and_skipped_sections() {
        let empty = plain(&render_section(&Section::new("kache cache"), Width::Piped));
        assert_eq!(
            empty[0],
            format!("kache cache{}nothing to clean", " ".repeat(53))
        );
        assert_eq!(empty.len(), 2);

        let reason = "cannot tell which Claude sessions are running: no registry";
        let skipped = Section::skipped("Claude Code tmp", reason.to_string());
        let lines = plain(&render_section(&skipped, Width::Piped));
        assert!(lines[0].ends_with("  skipped"), "{lines:?}");
        assert_eq!(lines[1], format!("   ! {reason}"));
    }

    #[test]
    fn escapes_control_characters_in_labels() {
        let section = Section {
            items: vec![cleaned("~/evil\nname", MIB)],
            ..Section::new("node_modules")
        };
        let lines = plain(&render_section(&section, Width::Piped));
        assert!(lines[1].starts_with("└─ ✓ ~/evil\\nname "), "{lines:?}");
    }

    #[test]
    fn renders_the_summary() {
        let summary = Summary {
            still_linked: 5 * MIB,
            interrupted: true,
            elapsed: Duration::from_millis(41_230),
            free_space: Some((98 * GIB, 100 * GIB)),
        };
        let lines = plain(&render_summary(&[mixed_section()], &summary, Width::Piped));
        assert_eq!(lines[0], "━".repeat(80));
        assert_eq!(
            lines[1],
            "Reclaimed 2.1 GiB  ·  2 cleaned · 1 kept · 1 failed  ·  took 41.2s"
        );
        assert_eq!(lines[2], "Free space on ~  98.0 GiB → 100.0 GiB");
        assert_eq!(
            lines[3],
            "5.0 MiB of removed files is still on disk: other hardlinks to them remain"
        );
        assert_eq!(lines[4], "Interrupted: stopped before cleaning everything");
    }

    #[test]
    fn tells_cleaned_from_already_clean_by_existence_not_size() {
        let removal = |existed| Removal {
            existed,
            size: 0,
            freed: 0,
            error: None,
        };
        let item = Item::removed("~/x".to_string(), removal(true));
        assert!(matches!(item.status, Status::Cleaned));
        let item = Item::removed("~/x".to_string(), removal(false));
        assert!(matches!(item.status, Status::AlreadyClean));
    }

    #[test]
    fn marks_interrupted_sections() {
        let empty = Section {
            interrupted: true,
            ..Section::new("node_modules")
        };
        let lines = plain(&render_section(&empty, Width::Piped));
        assert!(lines[0].ends_with("  interrupted"), "{lines:?}");

        let partial = Section {
            interrupted: true,
            ..mixed_section()
        };
        let lines = plain(&render_section(&partial, Width::Piped));
        assert_eq!(lines[lines.len() - 2], "   interrupted");
    }

    #[test]
    fn lists_a_removed_path_that_held_no_blocks() {
        // On tmpfs a directory takes no blocks, yet removing it is still cleaning.
        let section = Section {
            items: vec![cleaned("~/session", 0)],
            ..Section::new("Claude Code tmp")
        };
        let lines = plain(&render_section(&section, Width::Piped));
        assert!(lines[0].ends_with("1 cleaned · 0 B"), "{lines:?}");
        assert!(lines[1].starts_with("└─ ✓ ~/session"), "{lines:?}");
    }

    #[test]
    fn cuts_long_reasons_and_escapes_notes() {
        let reason = format!("could not find `Cargo.toml` in `{}`", "x".repeat(100));
        let section = Section {
            items: vec![failed("~/dev/broken", &reason, 0)],
            notes: vec!["evil\nnote".to_string()],
            ..Section::new("Rust projects")
        };
        let lines = plain(&render_section(&section, Width::Terminal(60)));
        assert_eq!(measure_text_width(&lines[2]), 60, "{lines:?}");
        assert!(lines[2].ends_with('…'), "{lines:?}");
        assert_eq!(lines[3], "   evil\\nnote");
    }

    #[test]
    fn truncates_in_the_middle_by_display_width() {
        assert_eq!(truncate_middle("abcdefghij", 5), "ab…ij");
        assert_eq!(truncate_middle("abc", 5), "abc");
        let wide = truncate_middle("日本語のテキスト", 7);
        assert!(measure_text_width(&wide) <= 7, "{wide}");
        assert!(wide.starts_with('日') && wide.ends_with('ト'), "{wide}");
    }

    #[test]
    fn shows_home_as_tilde() {
        let home = Path::new("/home/me");
        assert_eq!(
            display_path_in(Path::new("/home/me/dev/x"), home),
            "~/dev/x"
        );
        assert_eq!(display_path_in(home, home), "~");
        assert_eq!(display_path_in(Path::new("/home/meow"), home), "/home/meow");
    }
}
