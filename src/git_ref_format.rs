use std::process::{Command, Output};

use thiserror::Error;

#[derive(Debug, Error)]
pub(crate) enum CheckRefFormatError {
    #[error("git check-ref-format rejected {ref_name:?}: {details}")]
    Rejected { ref_name: String, details: String },
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

pub(crate) fn check_ref_format(ref_name: &str) -> Result<(), CheckRefFormatError> {
    let output = Command::new("git")
        .arg("check-ref-format")
        .arg(ref_name)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_PREFIX")
        .output()?;

    if output.status.success() {
        Ok(())
    } else {
        Err(CheckRefFormatError::Rejected {
            ref_name: ref_name.to_owned(),
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
