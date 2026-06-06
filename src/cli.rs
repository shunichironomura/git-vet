use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};

use crate::channel::{DEFAULT_REVIEW_CHANNEL, ReviewChannel, ReviewChannelCandidate};
use crate::commands::{Gate, StatusMode, diff_path, mark_paths, status, unmark_paths};
use crate::error::AppError;
use crate::git::Git;
use crate::git_ref_format::{CheckRefFormatError, check_ref_format};
use crate::notes::{GitNotesStore, NotesStore};

#[derive(Parser, Debug)]
#[command(
    name = "git-vet",
    version,
    about = "Track human review state for Git-tracked file contents"
)]
pub struct Cli {
    /// Review channel/pipeline to read or write.
    #[arg(long, global = true, default_value = DEFAULT_REVIEW_CHANNEL)]
    channel: String,
    #[command(subcommand)]
    command: CommandKind,
}

#[derive(Subcommand, Debug)]
enum CommandKind {
    /// Sign off the current HEAD content of tracked files.
    Mark {
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
        /// Exit 1 when any in-scope tracked file is unreviewed.
        #[arg(long)]
        check: bool,
    },
    /// Show the diff that still needs review for a tracked file.
    Diff { path: PathBuf },
    /// Prune notes for objects that are no longer present.
    Prune,
}

pub fn run_cli() -> Result<ExitCode, AppError> {
    let cli = Cli::parse();
    let channel = review_channel_from_cli(&cli.channel)?;
    let git = Git::discover()?;
    let notes = GitNotesStore::new(&git, channel.notes_ref().clone());

    match cli.command {
        CommandKind::Mark { paths } => {
            mark_paths(&git, &notes, &paths)?;
            Ok(ExitCode::SUCCESS)
        }
        CommandKind::Unmark { paths } => {
            unmark_paths(&git, &notes, &paths)?;
            Ok(ExitCode::SUCCESS)
        }
        CommandKind::Status { json, check } => {
            match status(&git, &notes, &channel, StatusMode { json, check })? {
                Gate::Open => Ok(ExitCode::SUCCESS),
                Gate::Closed => Ok(ExitCode::from(1)),
            }
        }
        CommandKind::Diff { path } => {
            diff_path(&git, &notes, &path)?;
            Ok(ExitCode::SUCCESS)
        }
        CommandKind::Prune => {
            notes.prune()?;
            Ok(ExitCode::SUCCESS)
        }
    }
}

fn review_channel_from_cli(input: &str) -> Result<ReviewChannel, AppError> {
    let candidate = ReviewChannelCandidate::new(input)?;

    // Channel validity is exact Git refname validity for the concrete notes ref
    // git-vet will use: refs/notes/vet/<channel>. Keep the Git subprocess at
    // the CLI boundary instead of making ReviewChannel construction impure.
    match check_ref_format(candidate.notes_ref_name()) {
        Ok(()) => Ok(candidate.into_channel_after_git_check_ref_format()),
        Err(CheckRefFormatError::Rejected { ref_name, details }) => Err(candidate
            .channel_error(format!(
                "`git check-ref-format` rejected {ref_name:?}: {details}"
            ))
            .into()),
        Err(CheckRefFormatError::Io(error)) => Err(AppError::Io(error)),
    }
}
