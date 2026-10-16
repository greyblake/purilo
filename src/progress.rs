// A spinner on stderr showing what is being cleaned. Drawn only when stdout and stderr are
// both terminals, so spinner frames never end up in a piped report.

use std::time::Duration;

use console::{Term, style};
use indicatif::{ProgressBar, ProgressStyle};

use crate::report::{self, escape_control};

pub struct Progress {
    bar: ProgressBar,
}

impl Progress {
    pub fn new() -> Self {
        if !(Term::stdout().is_term() && Term::stderr().is_term()) {
            return Self {
                bar: ProgressBar::hidden(),
            };
        }
        let bar = ProgressBar::new_spinner();
        bar.set_style(
            ProgressStyle::with_template("{spinner:.cyan} {wide_msg}")
                .unwrap_or_else(|_| ProgressStyle::default_spinner()),
        );
        bar.enable_steady_tick(Duration::from_millis(100));
        Self { bar }
    }

    // One message, so the section and the item never show up mismatched.
    pub fn set(&self, section: &'static str, item: &str) {
        let item = escape_control(item);
        self.bar
            .set_message(format!("{} {}", style(section).bold(), style(item).dim()));
    }

    // Prints above the spinner; it can't redraw until the text is written.
    pub fn print(&self, text: &str) {
        self.bar.suspend(|| report::print(text));
    }

    pub fn finish(&self) {
        self.bar.finish_and_clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_control_characters_in_the_spinner() {
        let progress = Progress {
            bar: ProgressBar::hidden(),
        };
        progress.set("node_modules", "~/ev\u{1b}]0;x\u{7}il");
        let message = console::strip_ansi_codes(&progress.bar.message()).into_owned();
        assert_eq!(message, "node_modules ~/ev\\u{1b}]0;x\\u{7}il");
    }
}
