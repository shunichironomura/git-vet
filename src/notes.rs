use std::ffi::OsStr;
use std::io::Write;
use std::process::{Command, Output, Stdio};

use gix::refs::transaction::{Change, PreviousValue, RefEdit, RefLog};

use crate::channel::{ChannelTransfer, ChannelTransferKind, NotesRef};
use crate::error::{AppError, git_error};
use crate::git::Git;
use crate::git_types::BlobOid;
use crate::remote::RemoteName;
use crate::review::{ReviewInfo, ReviewedSet, parse_note_records};

pub(crate) trait NotesStore {
    fn list_reviewed(&self) -> Result<ReviewedSet, AppError>;
    fn note_body(&self, oid: &BlobOid) -> Result<Option<String>, AppError>;
    fn write_note_body(&self, oid: &BlobOid, body: &str) -> Result<(), AppError>;
    fn remove_note(&self, oid: &BlobOid) -> Result<NoteRemoval, AppError>;
    fn prune(&self) -> Result<(), AppError>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum NoteRemoval {
    Removed,
    Absent,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct NoteListEntry {
    note: gix::ObjectId,
    annotated: BlobOid,
}

#[derive(Clone)]
pub(crate) struct GitNotesStore<'git> {
    git: &'git Git,
    notes_ref: NotesRef,
}

pub(crate) struct GitNotesChannelStore<'git> {
    git: &'git Git,
}

impl<'git> GitNotesChannelStore<'git> {
    pub(crate) const fn new(git: &'git Git) -> Self {
        Self { git }
    }

    pub(crate) fn transfer(&self, transfer: &ChannelTransfer) -> Result<(), AppError> {
        let source_ref = transfer.source().notes_ref();
        let destination_ref = transfer.destination().notes_ref();
        let source_target = self
            .git
            .repo
            .try_find_reference(source_ref.as_str())
            .map_err(|error| git_error("reading source channel notes ref", error))?
            .ok_or_else(|| AppError::MissingSourceChannelNotes {
                channel: transfer.source().to_string(),
            })?
            .target()
            .try_id()
            .map(ToOwned::to_owned)
            .ok_or_else(|| AppError::SymbolicChannelNotesRef {
                channel: transfer.source().to_string(),
            })?;

        if self
            .git
            .repo
            .try_find_reference(destination_ref.as_str())
            .map_err(|error| git_error("reading destination channel notes ref", error))?
            .is_some()
        {
            return Err(AppError::ExistingDestinationChannelNotes {
                channel: transfer.destination().to_string(),
            });
        }

        let source_oid = source_target.to_string();
        let instructions = update_ref_transaction(transfer, &source_oid);
        // The documented update-ref transaction protocol has a verify-only operation,
        // which lets copy guard the source without rewriting it. Disable automatic reflog
        // creation so transferring an existing notes commit does not require reviewer identity.
        run_git_with_stdin(
            self.git,
            "transferring channel review notes",
            [
                "-c",
                "core.logAllRefUpdates=false",
                "update-ref",
                "--stdin",
                "-z",
            ],
            &instructions,
        )?;
        Ok(())
    }
}

impl<'git> GitNotesStore<'git> {
    pub(crate) const fn new(git: &'git Git, notes_ref: NotesRef) -> Self {
        Self { git, notes_ref }
    }

    fn note_entries(&self) -> Result<Vec<NoteListEntry>, AppError> {
        // gix exposes point lookups and mutations, but not iteration over every note mapping.
        // Keep one machine-oriented Git query, then load the returned blobs through gix.
        let ref_arg = self.notes_ref_arg();
        let stdout = self.git_output("listing git notes", ["notes", ref_arg.as_str(), "list"])?;
        let output =
            String::from_utf8(stdout).map_err(|err| git_error("decoding notes list", err))?;
        output.lines().map(parse_note_list_entry).collect()
    }

    fn note_body_from_blob(&self, note: gix::ObjectId) -> Result<String, AppError> {
        let blob = self
            .git
            .repo
            .find_blob(note)
            .map_err(|error| git_error("reading git note blob", error))?;
        decode_note_body(&blob.data)
    }

    fn notes_platform(&self) -> Result<gix::note::Platform<'_>, AppError> {
        self.git
            .repo
            .notes()
            .map_err(|error| git_error("initializing git notes", error))?
            .with_refs([self.notes_ref.as_str()])
            .map_err(|error| git_error("selecting review notes ref", error))
    }

    pub(crate) const fn notes_ref(&self) -> &NotesRef {
        &self.notes_ref
    }

    pub(crate) fn sync_temp_ref(&self) -> Result<String, AppError> {
        sync_temp_ref(self.notes_ref.as_str())
    }

    pub(crate) fn remote_ref_exists(&self, remote: &RemoteName) -> Result<bool, AppError> {
        let output = self
            .git_command()
            .arg("ls-remote")
            .arg("--exit-code")
            .arg(remote.as_str())
            .arg(self.notes_ref.as_str())
            .output()?;
        match output.status.code() {
            Some(0) => Ok(true),
            Some(2) => Ok(false),
            _ => Err(git_error(
                "checking remote notes ref",
                command_failure_details(&output),
            )),
        }
    }

    pub(crate) fn local_ref_exists(&self) -> Result<bool, AppError> {
        self.git
            .repo
            .try_find_reference(self.notes_ref.as_str())
            .map(|reference| reference.is_some())
            .map_err(|error| git_error("checking local notes ref", error))
    }

    pub(crate) fn fetch_remote_notes(
        &self,
        remote: &RemoteName,
        temp_ref: &str,
    ) -> Result<(), AppError> {
        let refspec = format!("+{}:{temp_ref}", self.notes_ref);
        self.git_output(
            "fetching remote notes ref",
            ["fetch", "--no-tags", remote.as_str(), refspec.as_str()],
        )?;
        Ok(())
    }

    pub(crate) fn merge_notes_ref(&self, merge_ref: &str) -> Result<(), AppError> {
        let ref_arg = self.notes_ref_arg();
        self.git_output(
            "merging git notes",
            [
                "notes",
                ref_arg.as_str(),
                "merge",
                "-s",
                "cat_sort_uniq",
                merge_ref,
            ],
        )?;
        Ok(())
    }

    pub(crate) fn push_notes_ref(&self, remote: &RemoteName) -> Result<(), AppError> {
        let refspec = format!("{}:{}", self.notes_ref, self.notes_ref);
        self.git_output(
            "pushing notes ref",
            ["push", remote.as_str(), refspec.as_str()],
        )?;
        Ok(())
    }

    pub(crate) fn delete_ref(&self, ref_name: &str) -> Result<(), AppError> {
        let name = gix::refs::FullName::try_from(ref_name)
            .map_err(|error| git_error("validating temporary sync notes ref", error))?;
        self.git
            .repo
            .edit_references_as(
                [RefEdit {
                    change: Change::Delete {
                        expected: PreviousValue::Any,
                        log: RefLog::AndReference,
                    },
                    name,
                    deref: false,
                }],
                None,
            )
            .map(|_| ())
            .map_err(|error| git_error("deleting temporary sync notes ref", error))
    }

    fn notes_ref_arg(&self) -> String {
        format!("--ref={}", self.notes_ref)
    }

    fn git_command(&self) -> Command {
        git_command(self.git)
    }

    fn git_output<I, S>(&self, operation: &'static str, args: I) -> Result<Vec<u8>, AppError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let output = self.git_command().args(args).output()?;
        stdout_from_success(operation, output)
    }
}

impl NotesStore for GitNotesStore<'_> {
    fn list_reviewed(&self) -> Result<ReviewedSet, AppError> {
        self.note_entries()?
            .into_iter()
            .try_fold(ReviewedSet::default(), |mut reviewed, entry| {
                let body = self.note_body_from_blob(entry.note)?;
                let records = parse_note_records(&body);
                reviewed
                    .by_blob
                    .insert(entry.annotated, ReviewInfo { records });
                Ok(reviewed)
            })
    }

    fn note_body(&self, oid: &BlobOid) -> Result<Option<String>, AppError> {
        let mut notes = self.notes_platform()?;
        let mut found = notes
            .get(oid.as_object_id())
            .map_err(|error| git_error("reading git note", error))?;
        match found.len() {
            0 => Ok(None),
            1 => {
                let note = found.pop().ok_or_else(|| {
                    git_error(
                        "reading git note",
                        "note lookup reported one result but returned none",
                    )
                })?;
                decode_note_body(&note.blob.data).map(Some)
            }
            count => Err(git_error(
                "reading git note",
                format!("expected at most one note from the selected ref, found {count}"),
            )),
        }
    }

    fn write_note_body(&self, oid: &BlobOid, body: &str) -> Result<(), AppError> {
        self.git
            .repo
            .notes()
            .map_err(|error| git_error("initializing git notes", error))?
            .replace(self.notes_ref.as_str(), oid.as_object_id(), body.as_bytes())
            .map_err(|error| git_error("writing git note", error))?;
        Ok(())
    }

    fn remove_note(&self, oid: &BlobOid) -> Result<NoteRemoval, AppError> {
        self.git
            .repo
            .notes()
            .map_err(|error| git_error("initializing git notes", error))?
            .remove(self.notes_ref.as_str(), oid.as_object_id())
            .map(|removed| match removed {
                Some(_) => NoteRemoval::Removed,
                None => NoteRemoval::Absent,
            })
            .map_err(|error| git_error("removing git note", error))
    }

    fn prune(&self) -> Result<(), AppError> {
        // gix does not yet expose Git's reachability-aware notes pruning behavior.
        let ref_arg = self.notes_ref_arg();
        self.git_output("pruning git notes", ["notes", ref_arg.as_str(), "prune"])?;
        Ok(())
    }
}

fn update_ref_transaction(transfer: &ChannelTransfer, source_oid: &str) -> Vec<u8> {
    let mut instructions = Vec::new();
    append_transaction_command(&mut instructions, "start");

    match transfer.kind() {
        ChannelTransferKind::Copy => {
            append_transaction_command(&mut instructions, "option no-deref");
            append_ref_instruction(
                &mut instructions,
                "verify",
                transfer.source().notes_ref(),
                source_oid,
            );
        }
        ChannelTransferKind::Move => {}
    }

    append_transaction_command(&mut instructions, "option no-deref");
    append_ref_instruction(
        &mut instructions,
        "create",
        transfer.destination().notes_ref(),
        source_oid,
    );

    match transfer.kind() {
        ChannelTransferKind::Copy => {}
        ChannelTransferKind::Move => {
            append_transaction_command(&mut instructions, "option no-deref");
            append_ref_instruction(
                &mut instructions,
                "delete",
                transfer.source().notes_ref(),
                source_oid,
            );
        }
    }

    append_transaction_command(&mut instructions, "prepare");
    append_transaction_command(&mut instructions, "commit");
    instructions
}

fn append_transaction_command(instructions: &mut Vec<u8>, command: &str) {
    instructions.extend_from_slice(command.as_bytes());
    instructions.push(0);
}

fn append_ref_instruction(
    instructions: &mut Vec<u8>,
    command: &str,
    notes_ref: &NotesRef,
    oid: &str,
) {
    instructions.extend_from_slice(command.as_bytes());
    instructions.push(b' ');
    instructions.extend_from_slice(notes_ref.as_str().as_bytes());
    instructions.push(0);
    instructions.extend_from_slice(oid.as_bytes());
    instructions.push(0);
}

fn run_git_with_stdin<I, S>(
    git: &Git,
    operation: &'static str,
    args: I,
    stdin: &[u8],
) -> Result<Vec<u8>, AppError>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let mut child = git_command(git)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let mut child_stdin = child
        .stdin
        .take()
        .ok_or_else(|| git_error(operation, "git stdin was not available"))?;
    child_stdin.write_all(stdin)?;
    drop(child_stdin);
    let output = child.wait_with_output()?;
    stdout_from_success(operation, output)
}

fn git_command(git: &Git) -> Command {
    let mut command = Command::new("git");
    command
        .current_dir(&git.root)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_PREFIX");
    command
}

fn sync_temp_ref(notes_ref: &str) -> Result<String, AppError> {
    notes_ref
        .strip_prefix("refs/notes/vet/")
        .map(|channel| format!("refs/notes/vet-sync/{channel}"))
        .ok_or_else(|| {
            git_error(
                "building temporary sync notes ref",
                format!("notes ref {notes_ref:?} does not start with refs/notes/vet/"),
            )
        })
}

fn parse_note_list_entry(line: &str) -> Result<NoteListEntry, AppError> {
    let mut fields = line.split_whitespace();
    match (fields.next(), fields.next(), fields.next()) {
        (Some(note), Some(annotated), None) => {
            let note = parse_object_id("parsing notes list note object id", note)?;
            parse_object_id("parsing notes list annotated object id", annotated).map(|annotated| {
                NoteListEntry {
                    note,
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

fn decode_note_body(data: &[u8]) -> Result<String, AppError> {
    std::str::from_utf8(data)
        .map(ToOwned::to_owned)
        .map_err(|error| git_error("decoding note body", error))
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
