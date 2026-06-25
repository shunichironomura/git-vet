use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::io::{self, BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};

use chrono::{SecondsFormat, Utc};

use crate::channel::ReviewChannel;
use crate::error::AppError;
use crate::git::{Git, HistoryChange, HistoryChangeStatus};
use crate::git_types::{BlobOid, TrackedFile};
use crate::notes::{GitNotesStore, NoteRemoval, NotesStore};
use crate::path::RepoPath;
use crate::remote::RemoteName;
use crate::review::{ClassifiedFile, ReviewRecord, ReviewState, ReviewedSet, append_record};
use crate::status_output::{HumanStatusOptions, check_status, human_status, json_status};
use crate::sync_progress::{SyncContext, SyncOutcome, SyncProgress, SyncReport, SyncStep};
use crate::vetignore::Vetignore;

#[derive(Clone, Copy, Debug)]
pub(crate) struct StatusMode {
    pub(crate) json: bool,
    pub(crate) all: bool,
    pub(crate) check: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct MarkOptions {
    pub(crate) dirty_paths: DirtyPathHandling,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum DiffTarget {
    Head,
    Worktree,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum DirtyPathHandling {
    Prompt,
    Allow,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Gate {
    Open,
    Closed,
}

pub(crate) fn mark_paths(
    git: &Git,
    notes: &impl NotesStore,
    paths: &[PathBuf],
    options: MarkOptions,
) -> Result<(), AppError> {
    let paths = paths
        .iter()
        .map(|path| git.normalize_user_path(path))
        .collect::<Result<Vec<_>, _>>()?;
    let targets = paths
        .iter()
        .map(|path| git.blob_at_head(path))
        .collect::<Result<Vec<_>, _>>()?;
    let dirty_paths = git.dirty_paths_against_head(&targets)?;
    handle_dirty_paths(&dirty_paths, options.dirty_paths)?;

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
        let existing = notes.note_body(&file.blob)?;
        let body = append_record(existing.as_deref(), &record)?;
        if existing.as_deref() != Some(body.as_str()) {
            notes.write_note_body(&file.blob, &body)?;
        }
        stdout_line(format_args!("marked {}", file.path))
    })
}

pub(crate) fn unmark_paths(
    git: &Git,
    notes: &impl NotesStore,
    paths: &[PathBuf],
) -> Result<(), AppError> {
    let paths = paths
        .iter()
        .map(|path| git.normalize_user_path(path))
        .collect::<Result<Vec<_>, _>>()?;
    let targets = paths
        .iter()
        .map(|path| git.blob_at_head(path))
        .collect::<Result<Vec<_>, _>>()?;

    stderr_line(format_args!(
        "warning: unmarking is blob-keyed; all paths sharing the same current content are affected in this channel"
    ))?;

    let removals = targets
        .iter()
        .map(|file| file.blob)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .map(|blob| notes.remove_note(&blob).map(|removal| (blob, removal)))
        .collect::<Result<BTreeMap<BlobOid, NoteRemoval>, _>>()?;

    targets
        .iter()
        .try_for_each(|file| match removals[&file.blob] {
            NoteRemoval::Removed => stdout_line(format_args!("unmarked {}", file.path)),
            NoteRemoval::Absent => stdout_line(format_args!("{} was not marked", file.path)),
        })
}

pub(crate) fn status(
    git: &Git,
    notes: &impl NotesStore,
    channel: &ReviewChannel,
    mode: StatusMode,
) -> Result<Gate, AppError> {
    let vetignore = Vetignore::load(&git.root, channel)?;
    let tracked = git
        .tracked_files_at_head()?
        .into_iter()
        .filter(|file| !vetignore.is_ignored(&file.path))
        .collect::<Vec<_>>();
    let reviewed = notes.list_reviewed()?;

    let mut classified = classify_tracked_files(&tracked, &reviewed, || git.history_changes())?;
    classified.sort_by(|left, right| left.path.cmp(&right.path));

    if mode.check {
        let gate = if classified
            .iter()
            .all(|file| matches!(file.state, ReviewState::Vetted))
        {
            Gate::Open
        } else {
            Gate::Closed
        };
        stdout_str(&check_status(channel, &classified))?;
        Ok(gate)
    } else {
        let output = if mode.json {
            json_status(channel, &classified)?
        } else {
            human_status(
                channel,
                &classified,
                HumanStatusOptions {
                    show_all: mode.all,
                    color: human_status_color_enabled(),
                },
            )
        };
        stdout_str(&output)?;
        Ok(Gate::Open)
    }
}

fn human_status_color_enabled() -> bool {
    io::stdout().is_terminal() && env::var_os("NO_COLOR").is_none()
}

pub(crate) fn diff_path(
    git: &Git,
    notes: &impl NotesStore,
    path: &Path,
    target: DiffTarget,
) -> Result<(), AppError> {
    let path = git.normalize_user_path(path)?;
    let file = git.blob_at_head(&path)?;
    let reviewed = notes.list_reviewed()?;
    let classified = classify_path(git, &file, &reviewed)?;

    match target {
        DiffTarget::Head => diff_classified_head(git, &path, &file, &classified.state),
        DiffTarget::Worktree => diff_classified_worktree(git, &file, &classified.state),
    }
}

fn diff_classified_head(
    git: &Git,
    path: &RepoPath,
    file: &TrackedFile,
    state: &ReviewState,
) -> Result<(), AppError> {
    match state {
        ReviewState::Vetted => stdout_line(format_args!("{path} is up to date")),
        ReviewState::New => git.diff_empty_to_head(file),
        ReviewState::Stale { baseline } => git.diff_blobs(baseline, &file.blob),
    }
}

fn diff_classified_worktree(
    git: &Git,
    file: &TrackedFile,
    state: &ReviewState,
) -> Result<(), AppError> {
    match state {
        ReviewState::Vetted => git.diff_blob_to_worktree(&file.blob, file),
        ReviewState::New => git.diff_empty_to_worktree(file),
        ReviewState::Stale { baseline } => git.diff_blob_to_worktree(baseline, file),
    }
}

pub(crate) fn sync_notes(
    notes: &GitNotesStore<'_>,
    channel: &ReviewChannel,
    remote: &RemoteName,
    progress: &mut impl SyncProgress,
) -> Result<SyncReport, AppError> {
    progress.started(&SyncContext {
        channel,
        remote,
        notes_ref: notes.notes_ref(),
    })?;

    let remote_has_notes = run_sync_step(progress, SyncStep::CheckRemote, || {
        notes.remote_ref_exists(remote)
    })?;

    let outcome = match remote_has_notes {
        true => sync_existing_remote_notes(notes, remote, progress)?,
        false if run_sync_step(progress, SyncStep::CheckLocal, || notes.local_ref_exists())? => {
            run_sync_step(progress, SyncStep::Push, || notes.push_notes_ref(remote))?;
            SyncOutcome::PushedLocalOnly
        }
        false => SyncOutcome::NothingToSync,
    };

    let report = SyncReport {
        channel: channel.clone(),
        remote: remote.clone(),
        notes_ref: notes.notes_ref().clone(),
        outcome,
    };
    progress.finished(&report)?;
    Ok(report)
}

fn sync_existing_remote_notes(
    notes: &GitNotesStore<'_>,
    remote: &RemoteName,
    progress: &mut impl SyncProgress,
) -> Result<SyncOutcome, AppError> {
    let temp_ref = notes.sync_temp_ref()?;
    let sync_result: Result<SyncOutcome, AppError> = (|| {
        run_sync_step(progress, SyncStep::Fetch, || {
            notes.fetch_remote_notes(remote, &temp_ref)
        })?;
        run_sync_step(progress, SyncStep::Merge, || {
            notes.merge_notes_ref(&temp_ref)
        })?;
        run_sync_step(progress, SyncStep::Push, || notes.push_notes_ref(remote))?;
        Ok(SyncOutcome::FetchedMergedPushed)
    })();
    match sync_result {
        Ok(outcome) => {
            run_sync_step(progress, SyncStep::Cleanup, || notes.delete_ref(&temp_ref))?;
            Ok(outcome)
        }
        Err(error) => {
            let _cleanup_result = notes.delete_ref(&temp_ref);
            Err(error)
        }
    }
}

fn run_sync_step<T>(
    progress: &mut impl SyncProgress,
    step: SyncStep,
    action: impl FnOnce() -> Result<T, AppError>,
) -> Result<T, AppError> {
    progress.step_started(step)?;
    match action() {
        Ok(value) => {
            progress.step_finished(step)?;
            Ok(value)
        }
        Err(error) => {
            progress.step_failed(step)?;
            Err(error)
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

fn stderr_line(args: std::fmt::Arguments<'_>) -> Result<(), AppError> {
    let mut stderr = io::stderr().lock();
    stderr.write_fmt(args)?;
    stderr.write_all(b"\n")?;
    Ok(())
}

fn handle_dirty_paths(
    dirty_paths: &[RepoPath],
    handling: DirtyPathHandling,
) -> Result<(), AppError> {
    if dirty_paths.is_empty() {
        return Ok(());
    }

    let stdin = io::stdin();
    let interactive = stdin.is_terminal();
    let mut input = stdin.lock();
    let mut output = io::stderr().lock();
    handle_dirty_paths_with_io(dirty_paths, handling, interactive, &mut input, &mut output)
}

fn handle_dirty_paths_with_io(
    dirty_paths: &[RepoPath],
    handling: DirtyPathHandling,
    interactive: bool,
    input: &mut impl BufRead,
    output: &mut impl Write,
) -> Result<(), AppError> {
    write_dirty_paths_warning(dirty_paths, output)?;

    match handling {
        DirtyPathHandling::Allow => Ok(()),
        DirtyPathHandling::Prompt if interactive => prompt_for_dirty_confirmation(input, output),
        DirtyPathHandling::Prompt => Err(AppError::DirtyPathsRequireAllowDirty),
    }
}

fn write_dirty_paths_warning(
    dirty_paths: &[RepoPath],
    output: &mut impl Write,
) -> Result<(), AppError> {
    writeln!(
        output,
        "warning: these paths have uncommitted changes relative to HEAD:"
    )?;
    dirty_paths
        .iter()
        .try_for_each(|path| writeln!(output, "  {path}"))?;
    writeln!(output)?;
    writeln!(output, "git-vet marks only committed HEAD:<path> bytes.")?;
    writeln!(output, "Your working-tree changes will not be vetted.")?;
    Ok(())
}

fn prompt_for_dirty_confirmation(
    input: &mut impl BufRead,
    output: &mut impl Write,
) -> Result<(), AppError> {
    loop {
        write!(output, "Proceed with the committed HEAD version? [y/N] ")?;
        output.flush()?;

        let mut answer = String::new();
        if input.read_line(&mut answer)? == 0 {
            writeln!(output)?;
            return Err(AppError::DirtyPathsDeclined);
        }

        match DirtyPathAnswer::parse(&answer) {
            DirtyPathAnswer::Proceed => return Ok(()),
            DirtyPathAnswer::Abort => return Err(AppError::DirtyPathsDeclined),
            DirtyPathAnswer::Invalid => writeln!(output, "Please answer yes or no.")?,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DirtyPathAnswer {
    Proceed,
    Abort,
    Invalid,
}

impl DirtyPathAnswer {
    fn parse(input: &str) -> Self {
        match input.trim().to_ascii_lowercase().as_str() {
            "y" | "yes" => Self::Proceed,
            "" | "n" | "no" => Self::Abort,
            _ => Self::Invalid,
        }
    }
}

fn classify_path(
    git: &Git,
    file: &TrackedFile,
    reviewed: &ReviewedSet,
) -> Result<ClassifiedFile, AppError> {
    let mut historical_blobs = |file: &TrackedFile| git.historical_blobs(&file.path, &file.blob);
    classify_file(file, reviewed, &mut historical_blobs)
}

fn classify_tracked_files(
    files: &[TrackedFile],
    reviewed: &ReviewedSet,
    history_changes: impl FnOnce() -> Result<Vec<HistoryChange>, AppError>,
) -> Result<Vec<ClassifiedFile>, AppError> {
    if reviewed.is_empty() {
        return Ok(files.iter().map(classify_new_file).collect());
    }

    let mut classified = files
        .iter()
        .map(|file| classify_file_from_current_blob(file, reviewed))
        .collect::<Vec<_>>();
    let mut active = active_unreviewed_paths(files, reviewed);
    if active.is_empty() {
        return Ok(classified);
    }

    for change in history_changes()? {
        apply_history_change(&mut classified, &mut active, reviewed, &change);
        if active.is_empty() {
            break;
        }
    }

    Ok(classified)
}

fn classify_file(
    file: &TrackedFile,
    reviewed: &ReviewedSet,
    historical_blobs: &mut impl FnMut(&TrackedFile) -> Result<Vec<BlobOid>, AppError>,
) -> Result<ClassifiedFile, AppError> {
    if reviewed.is_empty() {
        return Ok(classify_new_file(file));
    }

    if reviewed.contains(&file.blob) {
        Ok(classify_vetted_file(file, reviewed))
    } else {
        let baseline = historical_blobs(file)?
            .into_iter()
            .find(|blob| reviewed.contains(blob));
        let metadata = baseline.as_ref().and_then(|blob| reviewed.metadata(blob));
        let state = baseline.map_or(ReviewState::New, |baseline| ReviewState::Stale { baseline });
        Ok(ClassifiedFile {
            path: file.path.clone(),
            state,
            blob: file.blob,
            metadata,
        })
    }
}

fn classify_file_from_current_blob(file: &TrackedFile, reviewed: &ReviewedSet) -> ClassifiedFile {
    if reviewed.contains(&file.blob) {
        classify_vetted_file(file, reviewed)
    } else {
        classify_new_file(file)
    }
}

fn classify_vetted_file(file: &TrackedFile, reviewed: &ReviewedSet) -> ClassifiedFile {
    ClassifiedFile {
        path: file.path.clone(),
        state: ReviewState::Vetted,
        blob: file.blob,
        metadata: reviewed.metadata(&file.blob),
    }
}

fn classify_new_file(file: &TrackedFile) -> ClassifiedFile {
    ClassifiedFile {
        path: file.path.clone(),
        state: ReviewState::New,
        blob: file.blob,
        metadata: None,
    }
}

fn active_unreviewed_paths(
    files: &[TrackedFile],
    reviewed: &ReviewedSet,
) -> BTreeMap<RepoPath, Vec<usize>> {
    files
        .iter()
        .enumerate()
        .filter(|(_, file)| !reviewed.contains(&file.blob))
        .fold(BTreeMap::new(), |mut active, (index, file)| {
            active.entry(file.path.clone()).or_default().push(index);
            active
        })
}

fn apply_history_change(
    classified: &mut [ClassifiedFile],
    active: &mut BTreeMap<RepoPath, Vec<usize>>,
    reviewed: &ReviewedSet,
    change: &HistoryChange,
) {
    let Some(after_path) = &change.after_path else {
        return;
    };
    let Some(indices) = active.remove(after_path) else {
        return;
    };

    let mut still_active = Vec::new();
    for index in indices {
        match change.before_blob.filter(|blob| reviewed.contains(blob)) {
            Some(baseline) => {
                classified[index].state = ReviewState::Stale { baseline };
                classified[index].metadata = reviewed.metadata(&baseline);
            }
            None if history_change_keeps_path(change.status) => still_active.push(index),
            None => {}
        }
    }

    if !still_active.is_empty() {
        let previous_path = match change.status {
            HistoryChangeStatus::Renamed => &change.before_path,
            HistoryChangeStatus::Modified | HistoryChangeStatus::TypeChanged => after_path,
            HistoryChangeStatus::Added
            | HistoryChangeStatus::Copied
            | HistoryChangeStatus::Deleted => return,
        };
        active
            .entry(previous_path.clone())
            .or_default()
            .extend(still_active);
    }
}

const fn history_change_keeps_path(status: HistoryChangeStatus) -> bool {
    matches!(
        status,
        HistoryChangeStatus::Modified
            | HistoryChangeStatus::Renamed
            | HistoryChangeStatus::TypeChanged
    )
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::io::Cursor;

    use super::*;
    use crate::git_types::FileMode;
    use crate::review::ReviewInfo;

    fn dirty_paths() -> Result<Vec<RepoPath>, AppError> {
        Ok(vec![RepoPath::from_git_path("src/lib.rs")?])
    }

    fn blob(hex: &str) -> Result<BlobOid, AppError> {
        gix::ObjectId::from_hex(hex.as_bytes())
            .map(BlobOid::new)
            .map_err(|err| crate::error::git_error("parsing test blob oid", err))
    }

    fn tracked(path: &str, blob: BlobOid) -> Result<TrackedFile, AppError> {
        Ok(TrackedFile {
            path: RepoPath::from_git_path(path)?,
            blob,
            mode: regular_file_mode()?,
        })
    }

    fn regular_file_mode() -> Result<FileMode, AppError> {
        gix::objs::tree::EntryMode::try_from(0o100_644)
            .map(FileMode::new)
            .map_err(|mode| crate::error::git_error("creating test file mode", mode))
    }

    fn change(
        status: HistoryChangeStatus,
        before_path: &str,
        after_path: Option<&str>,
        before_blob: Option<BlobOid>,
    ) -> Result<HistoryChange, AppError> {
        Ok(HistoryChange {
            status,
            before_path: RepoPath::from_git_path(before_path)?,
            after_path: after_path.map(RepoPath::from_git_path).transpose()?,
            before_blob,
        })
    }

    #[test]
    fn dirty_prompt_accepts_yes_after_warning() -> Result<(), AppError> {
        let dirty_paths = dirty_paths()?;
        let mut input = Cursor::new(b"yes\n".as_slice());
        let mut output = Vec::new();

        handle_dirty_paths_with_io(
            &dirty_paths,
            DirtyPathHandling::Prompt,
            true,
            &mut input,
            &mut output,
        )?;

        let output = String::from_utf8_lossy(&output);
        assert!(output.contains("uncommitted changes relative to HEAD"));
        assert!(output.contains("src/lib.rs"));
        assert!(output.contains("Proceed with the committed HEAD version? [y/N]"));
        Ok(())
    }

    #[test]
    fn empty_reviewed_set_classifies_everything_new_without_history_walk() -> Result<(), AppError> {
        let files = vec![
            tracked("a.txt", blob("1111111111111111111111111111111111111111")?)?,
            tracked("b.txt", blob("2222222222222222222222222222222222222222")?)?,
        ];
        let history_calls = Cell::new(0);

        let classified = classify_tracked_files(&files, &ReviewedSet::default(), || {
            history_calls.set(history_calls.get() + 1);
            Ok(Vec::new())
        })?;

        assert_eq!(history_calls.get(), 0);
        assert!(
            classified
                .iter()
                .all(|file| matches!(file.state, ReviewState::New))
        );
        Ok(())
    }

    #[test]
    fn non_empty_reviewed_set_uses_one_bulk_history_walk() -> Result<(), AppError> {
        let reviewed_blob = blob("1111111111111111111111111111111111111111")?;
        let stale_blob = blob("2222222222222222222222222222222222222222")?;
        let baseline_blob = blob("3333333333333333333333333333333333333333")?;
        let files = vec![
            tracked("reviewed.txt", reviewed_blob)?,
            tracked("stale.txt", stale_blob)?,
        ];
        let mut reviewed = ReviewedSet::default();
        reviewed
            .by_blob
            .insert(reviewed_blob, ReviewInfo::default());
        reviewed
            .by_blob
            .insert(baseline_blob, ReviewInfo::default());
        let history_calls = Cell::new(0);

        let classified = classify_tracked_files(&files, &reviewed, || {
            history_calls.set(history_calls.get() + 1);
            Ok(vec![change(
                HistoryChangeStatus::Modified,
                "stale.txt",
                Some("stale.txt"),
                Some(baseline_blob),
            )?])
        })?;

        assert_eq!(history_calls.get(), 1);
        assert!(matches!(classified[0].state, ReviewState::Vetted));
        assert!(matches!(
            classified[1].state,
            ReviewState::Stale { baseline } if baseline == baseline_blob
        ));
        Ok(())
    }

    #[test]
    fn bulk_history_walk_follows_renames_back_to_reviewed_baseline() -> Result<(), AppError> {
        let current_blob = blob("1111111111111111111111111111111111111111")?;
        let intermediate_blob = blob("2222222222222222222222222222222222222222")?;
        let baseline_blob = blob("3333333333333333333333333333333333333333")?;
        let files = vec![tracked("new.txt", current_blob)?];
        let mut reviewed = ReviewedSet::default();
        reviewed
            .by_blob
            .insert(baseline_blob, ReviewInfo::default());

        let classified = classify_tracked_files(&files, &reviewed, || {
            Ok(vec![
                change(
                    HistoryChangeStatus::Modified,
                    "new.txt",
                    Some("new.txt"),
                    Some(intermediate_blob),
                )?,
                change(
                    HistoryChangeStatus::Renamed,
                    "old.txt",
                    Some("new.txt"),
                    Some(baseline_blob),
                )?,
            ])
        })?;

        assert!(matches!(
            classified[0].state,
            ReviewState::Stale { baseline } if baseline == baseline_blob
        ));
        Ok(())
    }

    #[test]
    fn dirty_prompt_reprompts_after_invalid_answer() -> Result<(), AppError> {
        let dirty_paths = dirty_paths()?;
        let mut input = Cursor::new(b"maybe\ny\n".as_slice());
        let mut output = Vec::new();

        handle_dirty_paths_with_io(
            &dirty_paths,
            DirtyPathHandling::Prompt,
            true,
            &mut input,
            &mut output,
        )?;

        let output = String::from_utf8_lossy(&output);
        assert!(output.contains("Please answer yes or no."));
        Ok(())
    }

    #[test]
    fn dirty_prompt_treats_no_and_enter_as_abort() -> Result<(), AppError> {
        for answer in ["no\n", "\n"] {
            let dirty_paths = dirty_paths()?;
            let mut input = Cursor::new(answer.as_bytes());
            let mut output = Vec::new();

            let result = handle_dirty_paths_with_io(
                &dirty_paths,
                DirtyPathHandling::Prompt,
                true,
                &mut input,
                &mut output,
            );

            assert!(matches!(result, Err(AppError::DirtyPathsDeclined)));
        }
        Ok(())
    }

    #[test]
    fn dirty_prompt_fails_noninteractive_without_allow_dirty() -> Result<(), AppError> {
        let dirty_paths = dirty_paths()?;
        let mut input = Cursor::new(b"yes\n".as_slice());
        let mut output = Vec::new();

        let result = handle_dirty_paths_with_io(
            &dirty_paths,
            DirtyPathHandling::Prompt,
            false,
            &mut input,
            &mut output,
        );

        assert!(matches!(result, Err(AppError::DirtyPathsRequireAllowDirty)));
        let output = String::from_utf8_lossy(&output);
        assert!(output.contains("uncommitted changes relative to HEAD"));
        assert!(!output.contains("Proceed with the committed HEAD version? [y/N]"));
        Ok(())
    }

    #[test]
    fn allow_dirty_warns_without_prompting() -> Result<(), AppError> {
        let dirty_paths = dirty_paths()?;
        let mut input = Cursor::new(b"".as_slice());
        let mut output = Vec::new();

        handle_dirty_paths_with_io(
            &dirty_paths,
            DirtyPathHandling::Allow,
            false,
            &mut input,
            &mut output,
        )?;

        let output = String::from_utf8_lossy(&output);
        assert!(output.contains("uncommitted changes relative to HEAD"));
        assert!(output.contains("src/lib.rs"));
        assert!(!output.contains("Proceed with the committed HEAD version? [y/N]"));
        Ok(())
    }
}
