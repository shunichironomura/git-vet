use std::fmt;

use thiserror::Error;

use crate::channel::ChannelTransferError;
use crate::path::{PathError, RepoPath, RepoPathScope};
use crate::remote::RemoteError;

#[derive(Debug, Error)]
pub enum AppError {
    #[error("git operation failed while {operation}: {details}")]
    Git {
        operation: &'static str,
        details: String,
    },
    #[error("repository has no worktree")]
    MissingWorktree,
    #[error("path is not valid UTF-8")]
    NonUtf8Path,
    #[error("path escapes the repository root")]
    PathOutsideRepo,
    #[error("path must be absolute")]
    NonAbsolutePath,
    #[error("empty paths are not valid tracked files")]
    EmptyPath,
    #[error("repo path is invalid: {details}")]
    InvalidRepoPath { details: &'static str },
    #[error("path is not tracked at HEAD: {0}")]
    PathNotTracked(RepoPath),
    #[error("pathspec did not match any tracked files at HEAD: {0}")]
    PathspecNotMatched(RepoPathScope),
    #[error("path is a submodule/gitlink and is out of scope: {0}")]
    PathIsSubmodule(RepoPath),
    #[error("failed to read vetignore file: {0}")]
    Vetignore(String),
    #[error("missing git config user.name")]
    MissingUserName,
    #[error("missing git config user.email")]
    MissingUserEmail,
    #[error("git config {key} is not valid UTF-8: {details}")]
    NonUtf8GitConfig { key: &'static str, details: String },
    #[error(
        "target paths have uncommitted working-tree changes; rerun with --allow-dirty to proceed with committed HEAD contents"
    )]
    DirtyPathsRequireAllowDirty,
    #[error("aborted because target paths have uncommitted working-tree changes")]
    DirtyPathsDeclined,
    #[error("invalid review channel {channel:?}: {details}")]
    InvalidChannel { channel: String, details: String },
    #[error("source and destination channels must differ")]
    SameChannelTransfer,
    #[error("source channel {channel:?} has no local review notes")]
    MissingSourceChannelNotes { channel: String },
    #[error("review channel {channel:?} does not exist locally")]
    MissingChannelNotes { channel: String },
    #[error("destination channel {channel:?} already has local review notes")]
    ExistingDestinationChannelNotes { channel: String },
    #[error("source channel {channel:?} has a symbolic notes ref; expected a direct ref")]
    SymbolicChannelNotesRef { channel: String },
    #[error("--channel cannot be used with `{command}`; pass SOURCE and DESTINATION explicitly")]
    ChannelOptionNotAllowed { command: &'static str },
    #[error("--channel cannot be used with `channel list`; listing is not channel-scoped")]
    ChannelOptionNotAllowedForList,
    #[error("--channel cannot be used with `channel remove`; pass CHANNEL explicitly")]
    ChannelOptionNotAllowedForRemove,
    #[error("aborted removing review channel")]
    ChannelRemovalDeclined,
    #[error("non-interactive channel removal requires --force")]
    ChannelRemovalRequiresForce,
    #[error("sync remote error: {0}")]
    Remote(#[from] RemoteError),
    #[error("terminal interaction failed: {0}")]
    Dialog(#[from] dialoguer::Error),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
}

impl From<ChannelTransferError> for AppError {
    fn from(error: ChannelTransferError) -> Self {
        match error {
            ChannelTransferError::SameChannel => Self::SameChannelTransfer,
        }
    }
}

impl From<PathError> for AppError {
    fn from(error: PathError) -> Self {
        match error {
            PathError::NonUtf8 => Self::NonUtf8Path,
            PathError::OutsideRepo => Self::PathOutsideRepo,
            PathError::NonAbsolute => Self::NonAbsolutePath,
            PathError::EmptyPath => Self::EmptyPath,
            PathError::EmptyComponent => Self::InvalidRepoPath {
                details: "path contains an empty component",
            },
            PathError::CurrentDirComponent => Self::InvalidRepoPath {
                details: "path contains a current-directory component",
            },
            PathError::NulByte => Self::InvalidRepoPath {
                details: "path contains a NUL byte",
            },
        }
    }
}

pub(crate) fn git_error(operation: &'static str, source: impl fmt::Display) -> AppError {
    AppError::Git {
        operation,
        details: source.to_string(),
    }
}
