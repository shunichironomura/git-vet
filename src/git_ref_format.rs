use std::process::{Command, Output};

use thiserror::Error;

/// Proof that a concrete Git ref name was accepted by `git check-ref-format`
/// without normalization.
///
/// The inner string is private so callers cannot fabricate this proof without
/// going through [`check_ref_format`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct StrictGitRefName {
    name: String,
}

impl StrictGitRefName {
    /// Consume the proof and return the exact ref name that Git accepted.
    pub(crate) fn into_string(self) -> String {
        self.name
    }
}

/// Error returned while checking a ref name with `git check-ref-format`.
#[derive(Debug, Error)]
pub(crate) enum CheckRefFormatError {
    /// Git ran successfully but rejected the supplied ref name.
    #[error("git check-ref-format rejected {ref_name:?}: {details}")]
    Rejected { ref_name: String, details: String },
    /// The `git check-ref-format` process could not be executed.
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// Run strict `git check-ref-format` validation for `ref_name`.
///
/// This intentionally does not pass `--normalize`; success proves the exact
/// input string is a valid Git ref name as-is.
pub(crate) fn check_ref_format(ref_name: &str) -> Result<StrictGitRefName, CheckRefFormatError> {
    let ref_name = ref_name.to_owned();
    let output = Command::new("git")
        .arg("check-ref-format")
        .arg(&ref_name)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_PREFIX")
        .output()?;

    if output.status.success() {
        Ok(StrictGitRefName { name: ref_name })
    } else {
        Err(CheckRefFormatError::Rejected {
            ref_name,
            details: command_failure_details(&output),
        })
    }
}

fn command_failure_details(output: &Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stderr = stderr.trim();
    if stderr.is_empty() {
        output.status.to_string()
    } else {
        format!("{}: {stderr}", output.status)
    }
}
