// Keep restricted visibility for internal modules without expanding the crate API.
#![expect(
    clippy::redundant_pub_crate,
    reason = "Internal modules use restricted visibility to avoid unreachable public API."
)]

mod channel;
mod cli;
mod commands;
mod error;
mod git;
mod git_types;
mod manpage;
mod notes;
mod path;
mod remote;
mod review;
mod status_output;
mod sync_progress;
mod ui;
mod vetignore;

pub use cli::{Cli, CliError, run_cli};
pub use error::AppError;
pub use git_types::BlobOid;
pub use path::RepoPath;
