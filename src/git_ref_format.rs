use std::process::{Command, Output};

use thiserror::Error;

use crate::channel::{ReviewChannelCandidate, ValidatedReviewChannelCandidate};

#[derive(Debug, Error)]
pub(crate) enum CheckRefFormatError {
    #[error("git check-ref-format rejected {ref_name:?}: {details}")]
    Rejected { ref_name: String, details: String },
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// Proof that `value`'s concrete Git ref name was accepted by strict
/// `git check-ref-format` validation, without `--normalize`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct StrictGitRefFormatValidated<T> {
    value: T,
}

impl ValidatedReviewChannelCandidate for StrictGitRefFormatValidated<ReviewChannelCandidate> {
    fn into_candidate(self) -> ReviewChannelCandidate {
        self.value
    }
}

pub(crate) fn check_ref_format(
    candidate: ReviewChannelCandidate,
) -> Result<StrictGitRefFormatValidated<ReviewChannelCandidate>, CheckRefFormatError> {
    let ref_name = candidate.notes_ref_name().to_owned();
    let output = Command::new("git")
        .arg("check-ref-format")
        .arg(&ref_name)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_PREFIX")
        .output()?;

    if output.status.success() {
        Ok(StrictGitRefFormatValidated { value: candidate })
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
