use std::fmt::Write as _;

use serde::Serialize;

use crate::channel::ReviewChannel;
use crate::error::AppError;
use crate::git_types::BlobOid;
use crate::path::RepoPath;
use crate::review::{ClassifiedFile, ReviewState};

#[derive(Serialize)]
struct JsonStatus<'a> {
    channel: &'a ReviewChannel,
    files: Vec<JsonStatusRecord<'a>>,
}

#[derive(Serialize)]
struct JsonStatusRecord<'a> {
    path: &'a RepoPath,
    state: &'static str,
    blob: &'a BlobOid,
    baseline: Option<&'a BlobOid>,
    last_reviewed_at: Option<&'a str>,
    reviewer: Option<&'a str>,
}

pub fn json_status(
    channel: &ReviewChannel,
    classified: &[ClassifiedFile],
) -> Result<String, AppError> {
    let files = classified
        .iter()
        .map(|file| JsonStatusRecord {
            path: &file.path,
            state: file.state.as_str(),
            blob: &file.blob,
            baseline: file.state.baseline(),
            last_reviewed_at: file
                .metadata
                .as_ref()
                .map(|metadata| metadata.last_reviewed_at.as_str()),
            reviewer: file
                .metadata
                .as_ref()
                .map(|metadata| metadata.reviewer.as_str()),
        })
        .collect::<Vec<_>>();
    serde_json::to_string_pretty(&JsonStatus { channel, files })
        .map(|json| format!("{json}\n"))
        .map_err(AppError::from)
}

pub fn human_status(channel: &ReviewChannel, classified: &[ClassifiedFile]) -> String {
    let mut output = format!("channel: {channel}\n");
    push_group(&mut output, "vetted", classified, |state| {
        matches!(state, ReviewState::Vetted)
    });
    push_group(&mut output, "stale", classified, |state| {
        matches!(state, ReviewState::Stale { .. })
    });
    push_group(&mut output, "new", classified, |state| {
        matches!(state, ReviewState::New)
    });
    output
}

fn push_group(
    output: &mut String,
    label: &str,
    classified: &[ClassifiedFile],
    include: impl Fn(&ReviewState) -> bool,
) {
    let _ = writeln!(output, "{label}:");
    let files = classified
        .iter()
        .filter(|file| include(&file.state))
        .collect::<Vec<_>>();
    if files.is_empty() {
        output.push_str("  (none)\n");
    } else {
        for file in files {
            let _ = writeln!(output, "  {}", human_status_line(file));
        }
    }
}

fn human_status_line(file: &ClassifiedFile) -> String {
    let baseline = file
        .state
        .baseline()
        .map(|oid| format!(" baseline={oid}"))
        .unwrap_or_default();
    let metadata = file
        .metadata
        .as_ref()
        .map(|metadata| {
            format!(
                " reviewed-at={} reviewer={}",
                metadata.last_reviewed_at, metadata.reviewer
            )
        })
        .unwrap_or_default();
    format!("{} blob={}{}{}", file.path, file.blob, baseline, metadata)
}
