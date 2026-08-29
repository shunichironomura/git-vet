use std::env;
use std::ffi::OsStr;
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

    pub(crate) fn from_process_args() -> Option<Self> {
        let mut args = env::args_os().skip(1);
        let mut explicit = None;

        while let Some(argument) = args.next() {
            if argument == OsStr::new("--") {
                break;
            }
            match argument
                .to_str()
                .and_then(|value| value.strip_prefix("--color="))
            {
                Some(value) => explicit = Self::from_argument(value),
                None if argument == OsStr::new("--color") => {
                    explicit = args
                        .next()
                        .and_then(|value| Self::from_argument(&value.to_string_lossy()));
                }
                None => {}
            }
        }

        explicit
    }

    fn from_argument(value: &str) -> Option<Self> {
        match value {
            "auto" => Some(Self::Auto),
            "always" => Some(Self::Always),
            "never" => Some(Self::Never),
            _ => None,
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
