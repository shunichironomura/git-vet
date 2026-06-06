use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};

use chrono::{SecondsFormat, Utc};

use crate::channel::ReviewChannel;
use crate::error::AppError;
use crate::git::Git;
use crate::git_types::{BlobOid, TrackedFile};
use crate::notes::{NoteRemoval, NotesStore};
use crate::path::RepoPath;
use crate::review::{ClassifiedFile, ReviewRecord, ReviewState, ReviewedSet, append_record};
use crate::status_output::{human_status, json_status};
use crate::vetignore::Vetignore;

#[derive(Clone, Copy, Debug)]
pub struct StatusMode {
    pub(crate) json: bool,
    pub(crate) check: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MarkOptions {
    pub(crate) dirty_paths: DirtyPathHandling,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DirtyPathHandling {
    Prompt,
    Allow,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Gate {
    Open,
    Closed,
}

pub fn mark_paths(
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

pub fn unmark_paths(git: &Git, notes: &impl NotesStore, paths: &[PathBuf]) -> Result<(), AppError> {
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
        ReviewState::New => git.diff_empty_to_head(&file),
        ReviewState::Stale { baseline } => git.diff_blobs(&baseline, &file.blob),
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

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;

    fn dirty_paths() -> Result<Vec<RepoPath>, AppError> {
        Ok(vec![RepoPath::from_git_path("src/lib.rs")?])
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
