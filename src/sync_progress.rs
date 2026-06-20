use std::env;
use std::io::{self, IsTerminal, Write};
use std::time::Duration;

use indicatif::{ProgressBar, ProgressStyle};

use crate::channel::{NotesRef, ReviewChannel};
use crate::error::AppError;
use crate::remote::RemoteName;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SyncStep {
    CheckRemote,
    CheckLocal,
    Fetch,
    Merge,
    Push,
    Cleanup,
}

impl SyncStep {
    const fn message(self) -> &'static str {
        match self {
            Self::CheckRemote => "Checking remote review notes",
            Self::CheckLocal => "Checking local review notes",
            Self::Fetch => "Fetching review notes",
            Self::Merge => "Merging review notes",
            Self::Push => "Pushing review notes",
            Self::Cleanup => "Cleaning up temporary notes ref",
        }
    }

    const fn gerund(self) -> &'static str {
        match self {
            Self::CheckRemote => "checking remote review notes",
            Self::CheckLocal => "checking local review notes",
            Self::Fetch => "fetching review notes",
            Self::Merge => "merging review notes",
            Self::Push => "pushing review notes",
            Self::Cleanup => "cleaning up temporary notes ref",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SyncOutcome {
    FetchedMergedPushed,
    PushedLocalOnly,
    NothingToSync,
}

impl SyncOutcome {
    const fn details(self) -> &'static str {
        match self {
            Self::FetchedMergedPushed => "fetched, merged, and pushed",
            Self::PushedLocalOnly => "pushed local notes; remote ref did not exist",
            Self::NothingToSync => "nothing to sync",
        }
    }

    const fn summary_verb(self) -> &'static str {
        match self {
            Self::FetchedMergedPushed => "Synced",
            Self::PushedLocalOnly => "Pushed",
            Self::NothingToSync => "No",
        }
    }
}

#[derive(Debug)]
pub(crate) struct SyncContext<'a> {
    pub channel: &'a ReviewChannel,
    pub remote: &'a RemoteName,
    pub notes_ref: &'a NotesRef,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SyncReport {
    pub channel: ReviewChannel,
    pub remote: RemoteName,
    pub notes_ref: NotesRef,
    pub outcome: SyncOutcome,
}

pub(crate) trait SyncProgress {
    fn started(&mut self, context: &SyncContext<'_>) -> Result<(), AppError>;
    fn step_started(&mut self, step: SyncStep) -> Result<(), AppError>;
    fn step_finished(&mut self, step: SyncStep) -> Result<(), AppError>;
    fn step_failed(&mut self, step: SyncStep) -> Result<(), AppError>;
    fn finished(&mut self, report: &SyncReport) -> Result<(), AppError>;
}

pub(crate) enum SyncProgressReporter {
    Spinner(SpinnerSyncProgress),
    Plain(PlainSyncProgress),
}

impl SyncProgressReporter {
    pub(crate) fn from_environment() -> Self {
        if should_use_spinner() {
            Self::Spinner(SpinnerSyncProgress::new())
        } else {
            Self::Plain(PlainSyncProgress::new())
        }
    }
}

impl SyncProgress for SyncProgressReporter {
    fn started(&mut self, context: &SyncContext<'_>) -> Result<(), AppError> {
        match self {
            Self::Spinner(progress) => progress.started(context),
            Self::Plain(progress) => progress.started(context),
        }
    }

    fn step_started(&mut self, step: SyncStep) -> Result<(), AppError> {
        match self {
            Self::Spinner(progress) => progress.step_started(step),
            Self::Plain(progress) => progress.step_started(step),
        }
    }

    fn step_finished(&mut self, step: SyncStep) -> Result<(), AppError> {
        match self {
            Self::Spinner(progress) => progress.step_finished(step),
            Self::Plain(progress) => progress.step_finished(step),
        }
    }

    fn step_failed(&mut self, step: SyncStep) -> Result<(), AppError> {
        match self {
            Self::Spinner(progress) => progress.step_failed(step),
            Self::Plain(progress) => progress.step_failed(step),
        }
    }

    fn finished(&mut self, report: &SyncReport) -> Result<(), AppError> {
        match self {
            Self::Spinner(progress) => progress.finished(report),
            Self::Plain(progress) => progress.finished(report),
        }
    }
}

fn should_use_spinner() -> bool {
    io::stderr().is_terminal() && env::var_os("NO_COLOR").is_none() && env::var_os("CI").is_none()
}

pub(crate) struct SpinnerSyncProgress {
    spinner: ProgressBar,
    color: bool,
}

impl SpinnerSyncProgress {
    fn new() -> Self {
        let spinner = ProgressBar::new_spinner().with_style(
            ProgressStyle::default_spinner()
                .tick_strings(&["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏", " "]),
        );
        spinner.enable_steady_tick(Duration::from_millis(80));
        Self {
            spinner,
            color: true,
        }
    }
}

impl SyncProgress for SpinnerSyncProgress {
    fn started(&mut self, context: &SyncContext<'_>) -> Result<(), AppError> {
        self.spinner.set_message(format!(
            "Syncing {} for channel {} via {}",
            context.notes_ref, context.channel, context.remote
        ));
        Ok(())
    }

    fn step_started(&mut self, step: SyncStep) -> Result<(), AppError> {
        self.spinner
            .set_message(paint(step.message(), Color::Yellow, self.color));
        Ok(())
    }

    fn step_finished(&mut self, _step: SyncStep) -> Result<(), AppError> {
        Ok(())
    }

    fn step_failed(&mut self, step: SyncStep) -> Result<(), AppError> {
        self.spinner.finish_with_message(format!(
            "{} Failed while {}",
            paint("✗", Color::Red, self.color),
            step.gerund()
        ));
        Ok(())
    }

    fn finished(&mut self, report: &SyncReport) -> Result<(), AppError> {
        self.spinner
            .finish_with_message(final_summary(report, self.color));
        Ok(())
    }
}

pub(crate) struct PlainSyncProgress;

impl PlainSyncProgress {
    const fn new() -> Self {
        Self
    }
}

impl SyncProgress for PlainSyncProgress {
    fn started(&mut self, _context: &SyncContext<'_>) -> Result<(), AppError> {
        Ok(())
    }

    fn step_started(&mut self, _step: SyncStep) -> Result<(), AppError> {
        Ok(())
    }

    fn step_finished(&mut self, _step: SyncStep) -> Result<(), AppError> {
        Ok(())
    }

    fn step_failed(&mut self, step: SyncStep) -> Result<(), AppError> {
        let mut stderr = io::stderr().lock();
        writeln!(stderr, "✗ Failed while {}", step.gerund())?;
        Ok(())
    }

    fn finished(&mut self, report: &SyncReport) -> Result<(), AppError> {
        let mut stderr = io::stderr().lock();
        writeln!(stderr, "{}", final_summary(report, false))?;
        writeln!(stderr, "  ref: {}", report.notes_ref)?;
        writeln!(stderr, "  result: {}", report.outcome.details())?;
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Color {
    Green,
    Yellow,
    Red,
}

impl Color {
    const fn ansi_code(self) -> &'static str {
        match self {
            Self::Green => "32",
            Self::Yellow => "33",
            Self::Red => "31",
        }
    }
}

fn final_summary(report: &SyncReport, color_enabled: bool) -> String {
    match report.outcome {
        SyncOutcome::NothingToSync => format!(
            "{} No review notes to sync for channel {} via {}",
            paint("✓", Color::Green, color_enabled),
            report.channel,
            report.remote
        ),
        SyncOutcome::FetchedMergedPushed | SyncOutcome::PushedLocalOnly => format!(
            "{} {} review notes for channel {} via {}",
            paint("✓", Color::Green, color_enabled),
            report.outcome.summary_verb(),
            report.channel,
            report.remote
        ),
    }
}

fn paint(text: &str, color: Color, enabled: bool) -> String {
    if enabled {
        format!("\u{1b}[{}m{text}\u{1b}[0m", color.ansi_code())
    } else {
        text.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channel::{ReviewChannel, ReviewChannelCandidate};
    use crate::git_ref_format::check_ref_format;
    use crate::remote::RemoteNameSource;

    fn report(outcome: SyncOutcome) -> SyncReport {
        let channel = ReviewChannel::from_validated_candidate(
            check_ref_format(ReviewChannelCandidate::new("default").expect("valid candidate"))
                .expect("valid ref"),
        );
        SyncReport {
            notes_ref: channel.notes_ref().clone(),
            channel,
            remote: RemoteName::new("origin", RemoteNameSource::Cli).expect("valid remote"),
            outcome,
        }
    }

    #[test]
    fn final_summary_describes_completed_sync() {
        assert_eq!(
            final_summary(&report(SyncOutcome::FetchedMergedPushed), false),
            "✓ Synced review notes for channel default via origin"
        );
    }

    #[test]
    fn final_summary_describes_noop_sync() {
        assert_eq!(
            final_summary(&report(SyncOutcome::NothingToSync), false),
            "✓ No review notes to sync for channel default via origin"
        );
    }
}
