use std::fmt;

use thiserror::Error;

use crate::channel::ChannelError;
use crate::path::{PathError, RepoPath};
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
    #[error("sync remote error: {0}")]
    Remote(#[from] RemoteError),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
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

impl From<ChannelError> for AppError {
    fn from(error: ChannelError) -> Self {
        Self::InvalidChannel {
            channel: error.channel,
            details: error.details,
        }
    }
}

pub(crate) fn git_error(operation: &'static str, source: impl fmt::Display) -> AppError {
    AppError::Git {
        operation,
        details: source.to_string(),
    }
}
