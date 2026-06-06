#![expect(
    clippy::expect_used,
    clippy::format_push_string,
    clippy::map_unwrap_or,
    clippy::match_bool,
    clippy::missing_const_for_fn,
    clippy::needless_for_each,
    clippy::needless_pass_by_value,
    clippy::option_if_let_else,
    clippy::print_stdout,
    clippy::redundant_pub_crate,
    clippy::trivially_copy_pass_by_ref,
    clippy::unnecessary_wraps,
    clippy::unreachable,
    clippy::unused_self,
    reason = "Existing code does not yet satisfy the newly imported lint policy"
)]

mod channel;
mod cli;
mod commands;
mod error;
mod git;
mod git_types;
mod notes;
mod path;
mod review;
mod status_output;
mod vetignore;

pub use cli::{Cli, run_cli};
pub use error::AppError;
pub use git_types::BlobOid;
pub use path::RepoPath;
