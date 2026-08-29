use std::env;
use std::io::{self, IsTerminal};
use std::time::Duration;

use indicatif::{ProgressBar, ProgressStyle};

pub(crate) struct Activity {
    spinner: ProgressBar,
}

impl Activity {
    pub(crate) fn start(message: impl Into<String>) -> Self {
        let spinner = if interactive_stderr() {
            ProgressBar::new_spinner()
        } else {
            ProgressBar::hidden()
        };
        spinner.set_style(
            ProgressStyle::default_spinner()
                .tick_strings(&["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"]),
        );
        spinner.set_message(message.into());
        spinner.enable_steady_tick(Duration::from_millis(80));
        Self { spinner }
    }
}

impl Drop for Activity {
    fn drop(&mut self) {
        self.spinner.finish_and_clear();
    }
}

pub(crate) fn interactive_stderr() -> bool {
    io::stderr().is_terminal() && env::var_os("CI").is_none()
}
