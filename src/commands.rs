use std::path::PathBuf;

use chrono::{SecondsFormat, Utc};

use crate::channel::ReviewChannel;
use crate::error::AppError;
use crate::git::Git;
use crate::git_types::{HistoricalBlob, TrackedFile};
use crate::notes::NotesStore;
use crate::review::{ClassifiedFile, ReviewRecord, ReviewState, ReviewedSet, append_record};
use crate::status_output::{print_human_status, print_json_status};
use crate::vetignore::Vetignore;

#[derive(Clone, Copy, Debug)]
pub(crate) struct StatusMode {
    pub(crate) json: bool,
    pub(crate) check: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Gate {
    Open,
    Closed,
}

pub(crate) fn mark_paths(
    git: &Git,
    notes: &impl NotesStore,
    paths: Vec<PathBuf>,
) -> Result<(), AppError> {
    let paths = paths
        .iter()
        .map(|path| git.normalize_user_path(path))
        .collect::<Result<Vec<_>, _>>()?;
    let targets = paths
        .iter()
        .map(|path| git.blob_at_head(path))
        .collect::<Result<Vec<_>, _>>()?;
    let reviewer = git.reviewer()?;
    let commit = git.head_commit()?;
    let reviewed_at = Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true);

    targets.iter().try_for_each(|file| {
        let record = ReviewRecord {
            reviewed_at: reviewed_at.clone(),
            reviewer: reviewer.clone(),
            commit,
            path: file.path.clone(),
        };
        let body = append_record(notes.note_body(&file.blob)?.as_deref(), &record);
        notes.write_note_body(&file.blob, &body)?;
        println!("marked {}", file.path);
        Ok(())
    })
}

pub(crate) fn status(
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

    match mode.check {
        true => check_status(&tracked, &reviewed),
        false => {
            let mut classified = tracked
                .iter()
                .map(|file| classify_path(git, file, &reviewed))
                .collect::<Result<Vec<_>, _>>()?;
            classified.sort_by(|left, right| left.path.cmp(&right.path));

            match mode.json {
                true => print_json_status(channel, &classified)?,
                false => print_human_status(channel, &classified),
            }
            Ok(Gate::Open)
        }
    }
}

fn check_status(tracked: &[TrackedFile], reviewed: &ReviewedSet) -> Result<Gate, AppError> {
    let unreviewed = tracked
        .iter()
        .filter(|file| !reviewed.contains(&file.blob))
        .collect::<Vec<_>>();

    match unreviewed.is_empty() {
        true => Ok(Gate::Open),
        false => {
            unreviewed.iter().for_each(|file| println!("{}", file.path));
            Ok(Gate::Closed)
        }
    }
}

pub(crate) fn diff_path(git: &Git, notes: &impl NotesStore, path: PathBuf) -> Result<(), AppError> {
    let path = git.normalize_user_path(&path)?;
    let file = git.blob_at_head(&path)?;
    let reviewed = notes.list_reviewed()?;
    let classified = classify_path(git, &file, &reviewed)?;

    match classified.state {
        ReviewState::Vetted => {
            println!("{path} is up to date");
            Ok(())
        }
        ReviewState::New => {
            print!("{}", git.diff_empty_to_head(&file)?);
            Ok(())
        }
        ReviewState::Stale {
            baseline,
            baseline_mode,
        } => {
            let baseline = HistoricalBlob {
                blob: baseline,
                mode: baseline_mode,
            };
            print!("{}", git.diff_blobs_with_path_label(&baseline, &file)?);
            Ok(())
        }
    }
}

fn classify_path(
    git: &Git,
    file: &TrackedFile,
    reviewed: &ReviewedSet,
) -> Result<ClassifiedFile, AppError> {
    match reviewed.contains(&file.blob) {
        true => Ok(ClassifiedFile {
            path: file.path.clone(),
            state: ReviewState::Vetted,
            blob: file.blob,
            metadata: reviewed.metadata(&file.blob),
        }),
        false => {
            let baseline = git
                .historical_blobs(&file.path, &file.blob)?
                .into_iter()
                .find(|entry| reviewed.contains(&entry.blob));
            let metadata = baseline
                .as_ref()
                .and_then(|entry| reviewed.metadata(&entry.blob));
            let state = baseline
                .map(|baseline| ReviewState::Stale {
                    baseline: baseline.blob,
                    baseline_mode: baseline.mode,
                })
                .unwrap_or(ReviewState::New);
            Ok(ClassifiedFile {
                path: file.path.clone(),
                state,
                blob: file.blob,
                metadata,
            })
        }
    }
}
