use std::path::PathBuf;
use std::process::ExitCode;

use clap::builder::styling::{AnsiColor, Effects, Styles};
use clap::{ColorChoice, CommandFactory, FromArgMatches, Parser, Subcommand, ValueEnum};

use crate::channel::{
    ChannelError, ChannelTransfer, ChannelTransferKind, DEFAULT_REVIEW_CHANNEL, ReviewChannel,
};
use crate::commands::{
    DiffTarget, DirtyPathHandling, Gate, MarkOptions, StatusMode, StatusTarget, diff_path,
    list_channels, mark_paths, remove_channel, status, sync_notes, transfer_channel_notes,
    unmark_paths,
};
use crate::error::AppError;
use crate::git::Git;
use crate::notes::{GitNotesChannelStore, GitNotesStore, NotesStore};
use crate::sync_progress::SyncProgressReporter;
use crate::ui::{ColorMode, ColorPolicy};

const CLI_STYLES: Styles = Styles::styled()
    .header(AnsiColor::BrightCyan.on_default().effects(Effects::BOLD))
    .usage(AnsiColor::BrightCyan.on_default().effects(Effects::BOLD))
    .literal(AnsiColor::BrightGreen.on_default().effects(Effects::BOLD))
    .placeholder(AnsiColor::Cyan.on_default())
    .error(AnsiColor::BrightRed.on_default().effects(Effects::BOLD))
    .valid(AnsiColor::BrightGreen.on_default())
    .invalid(AnsiColor::BrightYellow.on_default());

#[derive(Parser, Debug)]
#[command(
    name = "git-vet",
    version,
    about = "Track human review state for Git-tracked file contents",
    styles = CLI_STYLES,
    after_help = "Typical workflow:\n  git vet status\n  git vet diff <path>\n  git vet mark <path>\n  git vet status --check"
)]
pub struct Cli {
    /// Review channel for commands that operate on one selected channel.
    #[arg(long, global = true)]
    channel: Option<String>,
    /// Control ANSI color output.
    #[arg(long, value_enum, global = true)]
    color: Option<CliColorMode>,
    #[command(subcommand)]
    command: CommandKind,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
enum CliColorMode {
    Auto,
    Always,
    Never,
}

impl From<CliColorMode> for ColorMode {
    fn from(mode: CliColorMode) -> Self {
        match mode {
            CliColorMode::Auto => Self::Auto,
            CliColorMode::Always => Self::Always,
            CliColorMode::Never => Self::Never,
        }
    }
}

#[derive(Subcommand, Debug)]
enum CommandKind {
    /// Manage local review notes across channels.
    Channel {
        #[command(subcommand)]
        command: ChannelCommand,
    },
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
        /// Classify local working-tree contents instead of committed HEAD contents.
        #[arg(long)]
        workspace: bool,
        /// Limit status to tracked files matching these file or directory pathspecs.
        #[arg(value_name = "PATHSPEC")]
        paths: Vec<PathBuf>,
    },
    /// Show the diff that still needs review for a tracked file.
    Diff {
        /// Compare the latest vetted content with the workspace instead of HEAD.
        #[arg(long)]
        workspace: bool,
        path: PathBuf,
    },
    /// Fetch, merge, and push review notes for the selected channel.
    Sync {
        /// Remote to sync with. Overrides vet.syncRemote and origin fallback.
        #[arg(long)]
        remote: Option<String>,
    },
    /// Prune notes for objects that are no longer present.
    Prune,
}

#[derive(Subcommand, Debug)]
enum ChannelCommand {
    /// List local review-note channels.
    List {
        /// Emit stable machine-readable JSON.
        #[arg(long)]
        json: bool,
    },
    /// Copy all local review notes into a new channel.
    Copy {
        /// Channel whose local review notes are copied.
        #[arg(value_name = "SOURCE")]
        source: String,
        /// New channel that receives the copied review notes.
        #[arg(value_name = "DESTINATION")]
        destination: String,
    },
    /// Move all local review notes into a new channel.
    Move {
        /// Channel whose local review notes are moved.
        #[arg(value_name = "SOURCE")]
        source: String,
        /// New channel that receives the moved review notes.
        #[arg(value_name = "DESTINATION")]
        destination: String,
    },
    /// Remove a local review-note channel.
    Remove {
        /// Channel whose local review notes are removed.
        #[arg(value_name = "CHANNEL")]
        target: String,
        /// Skip confirmation.
        #[arg(long)]
        force: bool,
    },
}

pub struct CliError {
    error: AppError,
    color_error: bool,
}

impl CliError {
    #[must_use]
    pub const fn error(&self) -> &AppError {
        &self.error
    }

    #[must_use]
    pub const fn color_error(&self) -> bool {
        self.color_error
    }
}

pub fn run_cli() -> Result<ExitCode, CliError> {
    let environment_color = ColorPolicy::resolve(None);
    let Cli {
        channel,
        color,
        command,
    } = parse_cli(environment_color);
    let color = ColorPolicy::resolve(color.map(Into::into));
    let git = Git::discover().map_err(|error| CliError {
        error,
        color_error: color.stderr(),
    })?;
    run_cli_inner(&git, channel.as_deref(), command, color).map_err(|error| CliError {
        error,
        color_error: color.stderr(),
    })
}

fn parse_cli(color: ColorPolicy) -> Cli {
    let command = Cli::command().color(if color.stderr() {
        ColorChoice::Always
    } else {
        ColorChoice::Never
    });
    let matches = command.get_matches();
    Cli::from_arg_matches(&matches).unwrap_or_else(|error| error.exit())
}

fn run_cli_inner(
    git: &Git,
    channel: Option<&str>,
    command: CommandKind,
    color: ColorPolicy,
) -> Result<ExitCode, AppError> {
    match command {
        CommandKind::Channel { command } => run_channel_command(git, channel, command),
        CommandKind::Mark { paths, allow_dirty } => {
            with_selected_channel(git, channel, |_, notes| {
                let dirty_paths = if allow_dirty {
                    DirtyPathHandling::Allow
                } else {
                    DirtyPathHandling::Prompt
                };
                mark_paths(git, notes, &paths, MarkOptions { dirty_paths })?;
                Ok(ExitCode::SUCCESS)
            })
        }
        CommandKind::Unmark { paths } => with_selected_channel(git, channel, |_, notes| {
            unmark_paths(git, notes, &paths)?;
            Ok(ExitCode::SUCCESS)
        }),
        CommandKind::Status {
            json,
            all,
            check,
            workspace,
            paths,
        } => with_selected_channel(git, channel, |channel, notes| {
            let target = if workspace {
                StatusTarget::Workspace
            } else {
                StatusTarget::Head
            };
            match status(
                git,
                notes,
                channel,
                StatusMode {
                    json,
                    all: all || !paths.is_empty(),
                    check,
                    target,
                    color,
                },
                &paths,
            )? {
                Gate::Open => Ok(ExitCode::SUCCESS),
                Gate::Closed => Ok(ExitCode::from(1)),
            }
        }),
        CommandKind::Diff { path, workspace } => with_selected_channel(git, channel, |_, notes| {
            let target = if workspace {
                DiffTarget::Workspace
            } else {
                DiffTarget::Head
            };
            diff_path(git, notes, &path, target, color)?;
            Ok(ExitCode::SUCCESS)
        }),
        CommandKind::Sync { remote } => with_selected_channel(git, channel, |channel, notes| {
            let remote = git.select_sync_remote(remote.as_deref())?;
            let mut progress = SyncProgressReporter::from_environment(color.stderr());
            sync_notes(notes, channel, &remote, &mut progress)?;
            Ok(ExitCode::SUCCESS)
        }),
        CommandKind::Prune => with_selected_channel(git, channel, |_, notes| {
            notes.prune()?;
            Ok(ExitCode::SUCCESS)
        }),
    }
}

fn run_channel_command(
    git: &Git,
    explicit_channel: Option<&str>,
    command: ChannelCommand,
) -> Result<ExitCode, AppError> {
    let (command_name, kind, source, destination) = match command {
        ChannelCommand::List { json } => {
            if explicit_channel.is_some() {
                return Err(AppError::ChannelOptionNotAllowedForList);
            }

            let channels = GitNotesChannelStore::new(git);
            list_channels(&channels, json)?;
            return Ok(ExitCode::SUCCESS);
        }
        ChannelCommand::Copy {
            source,
            destination,
        } => (
            "channel copy",
            ChannelTransferKind::Copy,
            source,
            destination,
        ),
        ChannelCommand::Move {
            source,
            destination,
        } => (
            "channel move",
            ChannelTransferKind::Move,
            source,
            destination,
        ),
        ChannelCommand::Remove { target, force } => {
            if explicit_channel.is_some() {
                return Err(AppError::ChannelOptionNotAllowedForRemove);
            }

            let channel = review_channel_from_input(&target, ReviewChannelSource::RemoveTarget)?;
            let channels = GitNotesChannelStore::new(git);
            remove_channel(&channels, &channel, force)?;
            return Ok(ExitCode::SUCCESS);
        }
    };

    if explicit_channel.is_some() {
        return Err(AppError::ChannelOptionNotAllowed {
            command: command_name,
        });
    }

    let source = review_channel_from_input(&source, ReviewChannelSource::TransferSource)?;
    let destination =
        review_channel_from_input(&destination, ReviewChannelSource::TransferDestination)?;
    let transfer = ChannelTransfer::new(kind, source, destination)?;
    let channels = GitNotesChannelStore::new(git);
    transfer_channel_notes(git, &channels, &transfer)?;
    Ok(ExitCode::SUCCESS)
}

fn with_selected_channel<T>(
    git: &Git,
    explicit_channel: Option<&str>,
    operation: impl FnOnce(&ReviewChannel, &GitNotesStore<'_>) -> Result<T, AppError>,
) -> Result<T, AppError> {
    let channel = review_channel_from_selection(explicit_channel, git)?;
    let notes = GitNotesStore::new(git, channel.notes_ref().clone());
    operation(&channel, &notes)
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
    TransferSource,
    TransferDestination,
    RemoveTarget,
}

fn review_channel_from_input(
    input: &str,
    source: ReviewChannelSource,
) -> Result<ReviewChannel, AppError> {
    ReviewChannel::new(input).map_err(|error| channel_error_from_source(input, &error, source))
}

fn channel_error_from_source(
    input: &str,
    error: &ChannelError,
    source: ReviewChannelSource,
) -> AppError {
    AppError::InvalidChannel {
        channel: input.to_owned(),
        details: details_from_source(error.to_string(), source),
    }
}

fn details_from_source(details: String, source: ReviewChannelSource) -> String {
    match source {
        ReviewChannelSource::Config => format!("from git config vet.channel: {details}"),
        ReviewChannelSource::TransferSource => format!("from SOURCE argument: {details}"),
        ReviewChannelSource::TransferDestination => {
            format!("from DESTINATION argument: {details}")
        }
        ReviewChannelSource::RemoveTarget => format!("from CHANNEL argument: {details}"),
        ReviewChannelSource::Cli | ReviewChannelSource::BuiltInDefault => details,
    }
}
