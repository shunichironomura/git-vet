use std::io::{self, Write};
use std::path::{Path, PathBuf};

use chrono::{SecondsFormat, Utc};

use crate::channel::ReviewChannel;
use crate::error::AppError;
use crate::git::Git;
use crate::git_types::{HistoricalBlob, TrackedFile};
use crate::notes::NotesStore;
use crate::review::{ClassifiedFile, ReviewRecord, ReviewState, ReviewedSet, append_record};
use crate::status_output::{human_status, json_status};
use crate::vetignore::Vetignore;

#[derive(Clone, Copy, Debug)]
pub struct StatusMode {
    pub(crate) json: bool,
    pub(crate) check: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Gate {
    Open,
    Closed,
}

pub fn mark_paths(git: &Git, notes: &impl NotesStore, paths: &[PathBuf]) -> Result<(), AppError> {
    let paths = paths
        .iter()
        .map(|path| git.normalize_user_path(path))
        .collect::<Result<Vec<_>, _>>()?;
    let targets = paths
        .iter()
        .map(|path| git.blob_at_head(path))
        .collect::<Result<Vec<_>, _>>()?;
    let vetter = git.vetter()?;
    let commit = git.head_commit()?;
    let vetted_at = Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true);

    targets.iter().try_for_each(|file| {
        let record = ReviewRecord {
            vetted_at: vetted_at.clone(),
            vetted_by: vetter.clone(),
            commit,
            path: file.path.clone(),
        };
        let body = append_record(notes.note_body(&file.blob)?.as_deref(), &record)?;
        notes.write_note_body(&file.blob, &body)?;
        stdout_line(format_args!("marked {}", file.path))
    })
}

pub fn status(
    git: &Git,
    notes: &impl NotesStore,
    channel: &ReviewChannel,
    mode: StatusMode,
) -> Result<Gate, AppError> {
    let vetignore = Vetignore::load(&git.root)?;
    let tracked = git
        .tracked_files_at_head()?
        .into_iter()
        .filter(|file| !vetignore.is_ignored(&file.path))
        .collect::<Vec<_>>();
    let reviewed = notes.list_reviewed()?;

    if mode.check {
        check_status(&tracked, &reviewed)
    } else {
        let mut classified = tracked
            .iter()
            .map(|file| classify_path(git, file, &reviewed))
            .collect::<Result<Vec<_>, _>>()?;
        classified.sort_by(|left, right| left.path.cmp(&right.path));

        let output = if mode.json {
            json_status(channel, &classified)?
        } else {
            human_status(channel, &classified)
        };
        stdout_str(&output)?;
        Ok(Gate::Open)
    }
}

fn check_status(tracked: &[TrackedFile], reviewed: &ReviewedSet) -> Result<Gate, AppError> {
    let unreviewed = tracked
        .iter()
        .filter(|file| !reviewed.contains(&file.blob))
        .collect::<Vec<_>>();

    if unreviewed.is_empty() {
        Ok(Gate::Open)
    } else {
        for file in unreviewed {
            stdout_line(format_args!("{}", file.path))?;
        }
        Ok(Gate::Closed)
    }
}

pub fn diff_path(git: &Git, notes: &impl NotesStore, path: &Path) -> Result<(), AppError> {
    let path = git.normalize_user_path(path)?;
    let file = git.blob_at_head(&path)?;
    let reviewed = notes.list_reviewed()?;
    let classified = classify_path(git, &file, &reviewed)?;

    match classified.state {
        ReviewState::Vetted => stdout_line(format_args!("{path} is up to date")),
        ReviewState::New => stdout_str(&git.diff_empty_to_head(&file)?),
        ReviewState::Stale {
            baseline,
            baseline_mode,
        } => {
            let baseline = HistoricalBlob {
                blob: baseline,
                mode: baseline_mode,
            };
            stdout_str(&git.diff_blobs_with_path_label(&baseline, &file)?)
        }
    }
}

fn stdout_line(args: std::fmt::Arguments<'_>) -> Result<(), AppError> {
    let mut stdout = io::stdout().lock();
    stdout.write_fmt(args)?;
    stdout.write_all(b"\n")?;
    Ok(())
}

fn stdout_str(output: &str) -> Result<(), AppError> {
    io::stdout().lock().write_all(output.as_bytes())?;
    Ok(())
}

fn classify_path(
    git: &Git,
    file: &TrackedFile,
    reviewed: &ReviewedSet,
) -> Result<ClassifiedFile, AppError> {
    if reviewed.contains(&file.blob) {
        Ok(ClassifiedFile {
            path: file.path.clone(),
            state: ReviewState::Vetted,
            blob: file.blob,
            metadata: reviewed.metadata(&file.blob),
        })
    } else {
        let baseline = git
            .historical_blobs(&file.path, &file.blob)?
            .into_iter()
            .find(|entry| reviewed.contains(&entry.blob));
        let metadata = baseline
            .as_ref()
            .and_then(|entry| reviewed.metadata(&entry.blob));
        let state = baseline.map_or(ReviewState::New, |baseline| ReviewState::Stale {
            baseline: baseline.blob,
            baseline_mode: baseline.mode,
        });
        Ok(ClassifiedFile {
            path: file.path.clone(),
            state,
            blob: file.blob,
            metadata,
        })
    }
}
