use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};

use crate::channel::{ChannelError, DEFAULT_REVIEW_CHANNEL, ReviewChannel, ReviewChannelCandidate};
use crate::commands::{
    DirtyPathHandling, Gate, MarkOptions, StatusMode, diff_path, mark_paths, status, sync_notes,
    unmark_paths,
};
use crate::error::AppError;
use crate::git::Git;
use crate::git_ref_format::{CheckRefFormatError, check_ref_format};
use crate::notes::{GitNotesStore, NotesStore};
use crate::sync_progress::SyncProgressReporter;

#[derive(Parser, Debug)]
#[command(
    name = "git-vet",
    version,
    about = "Track human review state for Git-tracked file contents"
)]
pub struct Cli {
    /// Review channel/pipeline to read or write.
    #[arg(long, global = true)]
    channel: Option<String>,
    #[command(subcommand)]
    command: CommandKind,
}

#[derive(Subcommand, Debug)]
enum CommandKind {
    /// Sign off the current HEAD content of tracked files.
    Mark {
        /// Proceed even if target paths have uncommitted working-tree changes.
        #[arg(long)]
        allow_dirty: bool,
        #[arg(required = true)]
        paths: Vec<PathBuf>,
    },
    /// Remove sign-off for the current HEAD content of tracked files.
    Unmark {
        #[arg(required = true)]
        paths: Vec<PathBuf>,
    },
    /// Show review state for tracked files.
    Status {
        /// Emit stable machine-readable JSON.
        #[arg(long)]
        json: bool,
        /// Include files that are already vetted in human-readable output.
        #[arg(long)]
        all: bool,
        /// Exit 1 when any in-scope tracked file is unreviewed.
        #[arg(long)]
        check: bool,
    },
    /// Show the diff that still needs review for a tracked file.
    Diff { path: PathBuf },
    /// Fetch, merge, and push review notes for the selected channel.
    Sync {
        /// Remote to sync with. Overrides vet.syncRemote and origin fallback.
        #[arg(long)]
        remote: Option<String>,
    },
    /// Prune notes for objects that are no longer present.
    Prune,
}

pub fn run_cli() -> Result<ExitCode, AppError> {
    let cli = Cli::parse();
    let git = Git::discover()?;
    let channel = review_channel_from_selection(cli.channel.as_deref(), &git)?;
    let notes = GitNotesStore::new(&git, channel.notes_ref().clone());

    match cli.command {
        CommandKind::Mark { paths, allow_dirty } => {
            let dirty_paths = if allow_dirty {
                DirtyPathHandling::Allow
            } else {
                DirtyPathHandling::Prompt
            };
            mark_paths(&git, &notes, &paths, MarkOptions { dirty_paths })?;
            Ok(ExitCode::SUCCESS)
        }
        CommandKind::Unmark { paths } => {
            unmark_paths(&git, &notes, &paths)?;
            Ok(ExitCode::SUCCESS)
        }
        CommandKind::Status { json, all, check } => {
            match status(&git, &notes, &channel, StatusMode { json, all, check })? {
                Gate::Open => Ok(ExitCode::SUCCESS),
                Gate::Closed => Ok(ExitCode::from(1)),
            }
        }
        CommandKind::Diff { path } => {
            diff_path(&git, &notes, &path)?;
            Ok(ExitCode::SUCCESS)
        }
        CommandKind::Sync { remote } => {
            let remote = git.select_sync_remote(remote.as_deref())?;
            let mut progress = SyncProgressReporter::from_environment();
            sync_notes(&notes, &channel, &remote, &mut progress)?;
            Ok(ExitCode::SUCCESS)
        }
        CommandKind::Prune => {
            notes.prune()?;
            Ok(ExitCode::SUCCESS)
        }
    }
}

fn review_channel_from_selection(
    explicit: Option<&str>,
    git: &Git,
) -> Result<ReviewChannel, AppError> {
    match explicit {
        Some(input) => review_channel_from_input(input, ReviewChannelSource::Cli),
        None => git.configured_review_channel()?.map_or_else(
            || {
                review_channel_from_input(
                    DEFAULT_REVIEW_CHANNEL,
                    ReviewChannelSource::BuiltInDefault,
                )
            },
            |input| review_channel_from_input(&input, ReviewChannelSource::Config),
        ),
    }
}

#[derive(Clone, Copy, Debug)]
enum ReviewChannelSource {
    Cli,
    Config,
    BuiltInDefault,
}

fn review_channel_from_input(
    input: &str,
    source: ReviewChannelSource,
) -> Result<ReviewChannel, AppError> {
    let candidate = ReviewChannelCandidate::new(input)
        .map_err(|error| channel_error_from_source(error, source))?;

    // Channel validity is exact Git refname validity for the concrete notes ref
    // git-vet will use: refs/notes/vet/<channel>. Keep the Git subprocess at
    // the CLI/config boundary instead of making ReviewChannel construction
    // impure.
    match check_ref_format(candidate) {
        Ok(validated) => Ok(ReviewChannel::from_validated_candidate(validated)),
        Err(CheckRefFormatError::Rejected { ref_name, details }) => Err(ChannelError {
            channel: input.to_owned(),
            details: details_from_source(
                format!("`git check-ref-format` rejected {ref_name:?}: {details}"),
                source,
            ),
        }
        .into()),
        Err(CheckRefFormatError::Io(error)) => Err(AppError::Io(error)),
    }
}

fn channel_error_from_source(error: ChannelError, source: ReviewChannelSource) -> AppError {
    ChannelError {
        channel: error.channel,
        details: details_from_source(error.details, source),
    }
    .into()
}

fn details_from_source(details: String, source: ReviewChannelSource) -> String {
    match source {
        ReviewChannelSource::Config => format!("from git config vet.channel: {details}"),
        ReviewChannelSource::Cli | ReviewChannelSource::BuiltInDefault => details,
    }
}
