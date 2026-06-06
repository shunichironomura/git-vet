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

pub(crate) fn print_json_status(
    channel: &ReviewChannel,
    classified: &[ClassifiedFile],
) -> Result<(), AppError> {
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
    println!(
        "{}",
        serde_json::to_string_pretty(&JsonStatus { channel, files })?
    );
    Ok(())
}

pub(crate) fn print_human_status(channel: &ReviewChannel, classified: &[ClassifiedFile]) {
    println!("channel: {channel}");
    print_group("vetted", classified, |state| {
        matches!(state, ReviewState::Vetted)
    });
    print_group("stale", classified, |state| {
        matches!(state, ReviewState::Stale { .. })
    });
    print_group("new", classified, |state| matches!(state, ReviewState::New));
}

fn print_group(label: &str, classified: &[ClassifiedFile], include: impl Fn(&ReviewState) -> bool) {
    println!("{label}:");
    let files = classified
        .iter()
        .filter(|file| include(&file.state))
        .collect::<Vec<_>>();
    match files.is_empty() {
        true => println!("  (none)"),
        false => files
            .iter()
            .for_each(|file| println!("  {}", human_status_line(file))),
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
