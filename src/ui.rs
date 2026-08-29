use std::env;
use std::io::{self, IsTerminal};
use std::time::Duration;

use indicatif::{ProgressBar, ProgressStyle};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ColorMode {
    Auto,
    Always,
    Never,
}

impl ColorMode {
    pub(crate) fn from_environment() -> Self {
        if environment_variable_is_non_empty("NO_COLOR") {
            Self::Never
        } else if environment_variable_is_non_empty("FORCE_COLOR") {
            Self::Always
        } else {
            Self::Auto
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ColorPolicy {
    stdout: bool,
    stderr: bool,
}

impl ColorPolicy {
    pub(crate) fn resolve(explicit: Option<ColorMode>) -> Self {
        let mode = explicit.unwrap_or_else(ColorMode::from_environment);
        let terminal = || io::stdout().is_terminal();
        let stderr_terminal = || io::stderr().is_terminal();
        match mode {
            ColorMode::Auto => Self {
                stdout: terminal(),
                stderr: stderr_terminal(),
            },
            ColorMode::Always => Self {
                stdout: true,
                stderr: true,
            },
            ColorMode::Never => Self {
                stdout: false,
                stderr: false,
            },
        }
    }

    pub(crate) const fn stdout(self) -> bool {
        self.stdout
    }

    pub(crate) const fn stderr(self) -> bool {
        self.stderr
    }
}

fn environment_variable_is_non_empty(name: &str) -> bool {
    env::var_os(name).is_some_and(|value| !value.is_empty())
}

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
        spinner.set_style(ProgressStyle::default_spinner());
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
