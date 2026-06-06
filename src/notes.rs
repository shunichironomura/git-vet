use std::ffi::OsStr;
use std::io::Write;
use std::process::{Command, Output, Stdio};

use crate::channel::NotesRef;
use crate::error::{AppError, git_error};
use crate::git::Git;
use crate::git_types::BlobOid;
use crate::review::{ReviewInfo, ReviewedSet, parse_note_records};

const NOTES_MERGE_STRATEGY_KEY: &str = "notes.mergeStrategy";
const NOTES_MERGE_STRATEGY: &str = "cat_sort_uniq";

pub trait NotesStore {
    fn list_reviewed(&self) -> Result<ReviewedSet, AppError>;
    fn note_body(&self, oid: &BlobOid) -> Result<Option<String>, AppError>;
    fn write_note_body(&self, oid: &BlobOid, body: &str) -> Result<(), AppError>;
    fn prune(&self) -> Result<(), AppError>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct NoteListEntry {
    annotated: BlobOid,
}

#[derive(Clone)]
pub struct GitNotesStore<'git> {
    git: &'git Git,
    notes_ref: NotesRef,
}

impl<'git> GitNotesStore<'git> {
    pub(crate) const fn new(git: &'git Git, notes_ref: NotesRef) -> Self {
        Self { git, notes_ref }
    }

    fn configure_merge_strategy(&self) -> Result<(), AppError> {
        self.git_output(
            "configuring notes merge strategy",
            [
                "config",
                "set",
                "--local",
                "--all",
                NOTES_MERGE_STRATEGY_KEY,
                NOTES_MERGE_STRATEGY,
            ],
        )?;
        Ok(())
    }

    fn note_entries(&self) -> Result<Vec<NoteListEntry>, AppError> {
        let ref_arg = self.notes_ref_arg();
        let stdout = self.git_output("listing git notes", ["notes", ref_arg.as_str(), "list"])?;
        let output =
            String::from_utf8(stdout).map_err(|err| git_error("decoding notes list", err))?;
        output.lines().map(parse_note_list_entry).collect()
    }

    fn show_note_body(&self, oid: &BlobOid) -> Result<String, AppError> {
        let ref_arg = self.notes_ref_arg();
        let oid_arg = oid.to_string();
        let stdout = self.git_output(
            "showing git note",
            ["notes", ref_arg.as_str(), "show", oid_arg.as_str()],
        )?;
        String::from_utf8(stdout).map_err(|err| git_error("decoding note body", err))
    }

    fn notes_ref_arg(&self) -> String {
        format!("--ref={}", self.notes_ref)
    }

    fn git_command(&self) -> Command {
        let mut command = Command::new("git");
        command
            .current_dir(&self.git.root)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_PREFIX");
        command
    }

    fn git_output<I, S>(&self, operation: &'static str, args: I) -> Result<Vec<u8>, AppError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let output = self.git_command().args(args).output()?;
        stdout_from_success(operation, output)
    }

    fn git_output_with_stdin<I, S>(
        &self,
        operation: &'static str,
        args: I,
        stdin: &str,
    ) -> Result<Vec<u8>, AppError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let mut child = self
            .git_command()
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let mut child_stdin = child
            .stdin
            .take()
            .ok_or_else(|| git_error(operation, "git stdin was not available"))?;
        child_stdin.write_all(stdin.as_bytes())?;
        drop(child_stdin);
        let output = child.wait_with_output()?;
        stdout_from_success(operation, output)
    }
}

impl NotesStore for GitNotesStore<'_> {
    fn list_reviewed(&self) -> Result<ReviewedSet, AppError> {
        self.note_entries()?
            .into_iter()
            .try_fold(ReviewedSet::default(), |mut reviewed, entry| {
                let body = self.show_note_body(&entry.annotated)?;
                let records = parse_note_records(&body);
                reviewed
                    .by_blob
                    .insert(entry.annotated, ReviewInfo { records });
                Ok(reviewed)
            })
    }

    fn note_body(&self, oid: &BlobOid) -> Result<Option<String>, AppError> {
        if self
            .note_entries()?
            .into_iter()
            .any(|entry| entry.annotated == *oid)
        {
            self.show_note_body(oid).map(Some)
        } else {
            Ok(None)
        }
    }

    fn write_note_body(&self, oid: &BlobOid, body: &str) -> Result<(), AppError> {
        self.configure_merge_strategy()?;
        let ref_arg = self.notes_ref_arg();
        let oid_arg = oid.to_string();
        self.git_output_with_stdin(
            "writing git note",
            [
                "notes",
                ref_arg.as_str(),
                "add",
                "-f",
                "--no-stripspace",
                "-F",
                "-",
                oid_arg.as_str(),
            ],
            body,
        )?;
        Ok(())
    }

    fn prune(&self) -> Result<(), AppError> {
        let ref_arg = self.notes_ref_arg();
        self.git_output("pruning git notes", ["notes", ref_arg.as_str(), "prune"])?;
        Ok(())
    }
}

fn parse_note_list_entry(line: &str) -> Result<NoteListEntry, AppError> {
    let mut fields = line.split_whitespace();
    match (fields.next(), fields.next(), fields.next()) {
        (Some(note_oid), Some(annotated), None) => {
            parse_object_id("parsing notes list note object id", note_oid)?;
            parse_object_id("parsing notes list annotated object id", annotated).map(|annotated| {
                NoteListEntry {
                    annotated: BlobOid::new(annotated),
                }
            })
        }
        (None, _, _) => Err(git_error(
            "parsing notes list",
            format!("missing note object id in {line:?}"),
        )),
        (Some(_), None, _) => Err(git_error(
            "parsing notes list",
            format!("missing annotated object id in {line:?}"),
        )),
        (Some(_), Some(_), Some(_)) => Err(git_error(
            "parsing notes list",
            format!("unexpected extra fields in {line:?}"),
        )),
    }
}

fn parse_object_id(operation: &'static str, oid: &str) -> Result<gix::ObjectId, AppError> {
    gix::ObjectId::from_hex(oid.as_bytes()).map_err(|err| git_error(operation, err))
}

fn stdout_from_success(operation: &'static str, output: Output) -> Result<Vec<u8>, AppError> {
    if output.status.success() {
        Ok(output.stdout)
    } else {
        Err(git_error(operation, command_failure_details(&output)))
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
