use std::path::PathBuf;
use std::process::ExitCode;
use std::str::FromStr;

use clap::{Parser, Subcommand};

use crate::channel::{DEFAULT_REVIEW_CHANNEL, ReviewChannel};
use crate::commands::{Gate, StatusMode, diff_path, mark_paths, status};
use crate::error::AppError;
use crate::git::Git;
use crate::notes::{GixNotesStore, NotesStore};

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
    let channel = ReviewChannel::from_str(&cli.channel)?;
    let git = Git::discover()?;
    let notes = GixNotesStore::new(&git, channel.notes_ref().clone());

    match cli.command {
        CommandKind::Mark { paths } => {
            mark_paths(&git, &notes, paths)?;
            Ok(ExitCode::SUCCESS)
        }
        CommandKind::Status { json, check } => {
            match status(&git, &notes, &channel, StatusMode { json, check })? {
                Gate::Open => Ok(ExitCode::SUCCESS),
                Gate::Closed => Ok(ExitCode::from(1)),
            }
        }
        CommandKind::Diff { path } => {
            diff_path(&git, &notes, path)?;
            Ok(ExitCode::SUCCESS)
        }
        CommandKind::Prune => {
            notes.prune()?;
            Ok(ExitCode::SUCCESS)
        }
    }
}
