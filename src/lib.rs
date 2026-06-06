use std::collections::HashMap;
use std::env;
use std::fmt;
use std::path::{Component, Path, PathBuf};
use std::process::ExitCode;
use std::str::FromStr;

use chrono::{SecondsFormat, Utc};
use clap::{Parser, Subcommand};
use gix::bstr::ByteSlice;
use gix::diff::blob::platform::prepare_diff::Operation;
use gix::diff::blob::unified_diff::{ConsumeBinaryHunk, ContextSize};
use gix::diff::blob::{ResourceKind, UnifiedDiff};
use gix::objs::tree::{EntryKind, EntryMode};
use ignore::gitignore::{Gitignore, GitignoreBuilder};
use serde::{Serialize, Serializer};
use thiserror::Error;

const NOTES_REF_PREFIX: &str = "refs/notes/vet";
const DEFAULT_REVIEW_CHANNEL: &str = "default";
const NOTES_MERGE_STRATEGY_KEY: &str = "notes.mergeStrategy";
const NOTES_MERGE_STRATEGY: &str = "cat_sort_uniq";

#[derive(Parser, Debug)]
#[command(
    name = "git-vet",
    version,
    about = "Track human review state for Git-tracked file contents"
)]
pub struct Cli {
    /// Review channel/pipeline to read or write.
    #[arg(long, global = true, default_value = DEFAULT_REVIEW_CHANNEL)]
    channel: String,
    #[command(subcommand)]
    command: CommandKind,
}

#[derive(Subcommand, Debug)]
enum CommandKind {
    /// Sign off the current HEAD content of tracked files.
    Mark {
        #[arg(required = true)]
        paths: Vec<PathBuf>,
    },
    /// Show review state for tracked files.
    Status {
        /// Emit stable machine-readable JSON.
        #[arg(long)]
        json: bool,
        /// Exit 1 when any in-scope tracked file is unreviewed.
        #[arg(long)]
        check: bool,
    },
    /// Show the diff that still needs review for a tracked file.
    Diff { path: PathBuf },
    /// Prune notes for objects that are no longer present.
    Prune,
}

pub fn run_cli() -> Result<ExitCode, AppError> {
    let cli = Cli::parse();
    let channel = ReviewChannel::from_str(&cli.channel)?;
    let git = Git::discover()?;
    let notes = GixNotesStore::new(&git, channel.notes_ref().clone());

    match cli.command {
        CommandKind::Mark { paths } => {
            mark_paths(&git, &notes, paths)?;
            Ok(ExitCode::SUCCESS)
        }
        CommandKind::Status { json, check } => {
            match status(&git, &notes, &channel, StatusMode { json, check })? {
                Gate::Open => Ok(ExitCode::SUCCESS),
                Gate::Closed => Ok(ExitCode::from(1)),
            }
        }
        CommandKind::Diff { path } => {
            diff_path(&git, &notes, path)?;
            Ok(ExitCode::SUCCESS)
        }
        CommandKind::Prune => {
            notes.prune()?;
            Ok(ExitCode::SUCCESS)
        }
    }
}

#[derive(Debug, Error)]
pub enum AppError {
    #[error("git operation failed while {operation}: {details}")]
    Git {
        operation: &'static str,
        details: String,
    },
    #[error("repository has no worktree")]
    MissingWorktree,
    #[error("path is not valid UTF-8: {0}")]
    NonUtf8Path(String),
    #[error("path escapes the repository root: {0}")]
    PathOutsideRepo(String),
    #[error("empty paths are not valid tracked files")]
    EmptyPath,
    #[error("path is not tracked at HEAD: {0}")]
    PathNotTracked(RepoPath),
    #[error("path is a submodule/gitlink and is out of scope: {0}")]
    PathIsSubmodule(RepoPath),
    #[error("failed to read .vetignore: {0}")]
    Vetignore(String),
    #[error("missing git config user.email")]
    MissingUserEmail,
    #[error("invalid review channel {channel:?}: {details}")]
    InvalidChannel { channel: String, details: String },
    #[error("notes ref points to a {actual}; expected a commit")]
    InvalidNotesRefTarget { actual: &'static str },
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
}

fn git_error(operation: &'static str, source: impl fmt::Display) -> AppError {
    AppError::Git {
        operation,
        details: source.to_string(),
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Hash, Ord, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct RepoPath(String);

impl RepoPath {
    fn from_git_path(path: &str) -> Result<Self, AppError> {
        if path.is_empty() {
            return Err(AppError::EmptyPath);
        }
        Ok(Self(path.to_owned()))
    }

    fn as_bstr(&self) -> &gix::bstr::BStr {
        self.0.as_bytes().as_bstr()
    }

    fn to_path_buf(&self) -> PathBuf {
        self.0.split('/').collect()
    }
}

impl fmt::Display for RepoPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ReviewChannel {
    name: String,
    notes_ref: NotesRef,
}

impl ReviewChannel {
    fn notes_ref(&self) -> &NotesRef {
        &self.notes_ref
    }
}

impl Default for ReviewChannel {
    fn default() -> Self {
        Self::from_str(DEFAULT_REVIEW_CHANNEL)
            .expect("the built-in default review channel must be a valid notes ref")
    }
}

impl FromStr for ReviewChannel {
    type Err = AppError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        if input.is_empty() {
            return Err(AppError::InvalidChannel {
                channel: input.to_owned(),
                details: "channel name must not be empty".to_owned(),
            });
        }

        let ref_name = format!("{NOTES_REF_PREFIX}/{input}");
        let notes_ref = NotesRef::new(ref_name).map_err(|details| AppError::InvalidChannel {
            channel: input.to_owned(),
            details,
        })?;

        Ok(Self {
            name: input.to_owned(),
            notes_ref,
        })
    }
}

impl Serialize for ReviewChannel {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.name)
    }
}

impl fmt::Display for ReviewChannel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.name.fmt(f)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct NotesRef {
    name: String,
    full_name: gix::refs::FullName,
}

impl NotesRef {
    fn new(name: String) -> Result<Self, String> {
        let full_name =
            gix::refs::FullName::try_from(name.clone()).map_err(|error| error.to_string())?;
        Ok(Self { name, full_name })
    }

    fn as_str(&self) -> &str {
        &self.name
    }

    fn full_name(&self) -> gix::refs::FullName {
        self.full_name.clone()
    }
}

impl fmt::Display for NotesRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.name.fmt(f)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub struct BlobOid(gix::ObjectId);

impl BlobOid {
    fn new(oid: gix::ObjectId) -> Self {
        Self(oid)
    }

    fn as_object_id(&self) -> gix::ObjectId {
        self.0
    }

    fn short(&self) -> String {
        self.0.to_hex_with_len(12).to_string()
    }
}

impl Serialize for BlobOid {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

impl fmt::Display for BlobOid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
struct CommitOid(gix::ObjectId);

impl CommitOid {
    fn new(oid: gix::ObjectId) -> Self {
        Self(oid)
    }
}

impl fmt::Display for CommitOid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct FileMode(EntryMode);

impl FileMode {
    fn new(mode: EntryMode) -> Self {
        Self(mode)
    }

    fn kind(&self) -> EntryKind {
        self.0.kind()
    }

    fn is_reviewable_file(&self) -> bool {
        self.0.is_blob_or_symlink()
    }

    fn is_submodule(&self) -> bool {
        self.0.is_commit()
    }

    fn as_octal(&self) -> String {
        format!("{:o}", self.0)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ReviewRecord {
    reviewed_at: String,
    reviewer: String,
    commit: CommitOid,
    path: RepoPath,
}

impl ReviewRecord {
    fn render(&self) -> String {
        format!(
            "reviewed-at={} reviewer={} commit={} path={}",
            self.reviewed_at, self.reviewer, self.commit, self.path
        )
    }
}

#[derive(Clone, Debug, Default)]
struct ReviewInfo {
    records: Vec<ReviewRecord>,
}

impl ReviewInfo {
    fn latest_metadata(&self) -> Option<ReviewMetadata> {
        self.records
            .iter()
            .max_by(|left, right| left.reviewed_at.cmp(&right.reviewed_at))
            .map(|record| ReviewMetadata {
                last_reviewed_at: record.reviewed_at.clone(),
                reviewer: record.reviewer.clone(),
            })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ReviewMetadata {
    last_reviewed_at: String,
    reviewer: String,
}

#[derive(Clone, Debug, Default)]
struct ReviewedSet {
    by_blob: HashMap<BlobOid, ReviewInfo>,
}

impl ReviewedSet {
    fn contains(&self, oid: &BlobOid) -> bool {
        self.by_blob.contains_key(oid)
    }

    fn metadata(&self, oid: &BlobOid) -> Option<ReviewMetadata> {
        self.by_blob.get(oid).and_then(ReviewInfo::latest_metadata)
    }
}

#[derive(Clone, Debug)]
enum ReviewState {
    Vetted,
    Stale {
        baseline: BlobOid,
        baseline_mode: FileMode,
    },
    New,
}

impl ReviewState {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Vetted => "vetted",
            Self::Stale { .. } => "stale",
            Self::New => "new",
        }
    }

    fn baseline(&self) -> Option<&BlobOid> {
        match self {
            Self::Stale { baseline, .. } => Some(baseline),
            Self::Vetted | Self::New => None,
        }
    }
}

#[derive(Clone, Debug)]
struct ClassifiedFile {
    path: RepoPath,
    state: ReviewState,
    blob: BlobOid,
    metadata: Option<ReviewMetadata>,
}

#[derive(Clone, Debug)]
struct TrackedFile {
    path: RepoPath,
    blob: BlobOid,
    mode: FileMode,
}

#[derive(Clone, Debug)]
struct HistoricalBlob {
    blob: BlobOid,
    mode: FileMode,
}

struct Git {
    repo: gix::Repository,
    root: PathBuf,
    prefix: PathBuf,
}

impl Git {
    fn discover() -> Result<Self, AppError> {
        let cwd = env::current_dir()?;
        let repo = gix::discover_with_environment_overrides(&cwd)
            .map_err(|err| git_error("discovering repository", err))?;
        let root = repo
            .workdir()
            .ok_or(AppError::MissingWorktree)?
            .to_path_buf();
        let prefix = match env::var_os("GIT_PREFIX") {
            Some(value) if !value.is_empty() => PathBuf::from(value),
            _ => prefix_from_cwd(&root, &cwd)?,
        };

        Ok(Self { repo, root, prefix })
    }

    fn normalize_user_path(&self, input: &Path) -> Result<RepoPath, AppError> {
        let joined = match input.is_absolute() {
            true => input.to_path_buf(),
            false => self.root.join(&self.prefix).join(input),
        };
        let normalized = normalize_lexically(&joined);
        let root = normalize_lexically(&self.root);
        let relative = normalized
            .strip_prefix(&root)
            .map_err(|_| AppError::PathOutsideRepo(input.display().to_string()))?;
        let path = repo_path_from_relative(relative)?;
        RepoPath::from_git_path(&path)
    }

    fn tracked_files_at_head(&self) -> Result<Vec<TrackedFile>, AppError> {
        let tree = self
            .repo
            .head_tree()
            .map_err(|err| git_error("reading HEAD tree", err))?;
        let mut files = tree
            .traverse()
            .breadthfirst
            .files()
            .map_err(|err| git_error("walking HEAD tree", err))?
            .into_iter()
            .filter(|entry| entry.mode.is_blob_or_symlink())
            .map(|entry| {
                Ok(TrackedFile {
                    path: repo_path_from_bstr(entry.filepath.as_bstr())?,
                    blob: BlobOid::new(entry.oid),
                    mode: FileMode::new(entry.mode),
                })
            })
            .collect::<Result<Vec<_>, AppError>>()?;
        files.sort_by(|left, right| left.path.cmp(&right.path));
        Ok(files)
    }

    fn blob_at_head(&self, path: &RepoPath) -> Result<TrackedFile, AppError> {
        let tree = self
            .repo
            .head_tree()
            .map_err(|err| git_error("reading HEAD tree", err))?;
        match self.lookup_file_in_tree(&tree, path)? {
            Some(file) => Ok(file),
            None => Err(AppError::PathNotTracked(path.clone())),
        }
    }

    fn head_commit(&self) -> Result<CommitOid, AppError> {
        self.repo
            .head_id()
            .map(|id| CommitOid::new(id.detach()))
            .map_err(|err| git_error("reading HEAD", err))
    }

    fn reviewer(&self) -> Result<String, AppError> {
        let reviewer = self
            .repo
            .config_snapshot()
            .string("user.email")
            .ok_or(AppError::MissingUserEmail)?;
        let reviewer = reviewer
            .to_str()
            .map_err(|err| AppError::NonUtf8Path(err.to_string()))?
            .trim()
            .to_owned();
        match reviewer.is_empty() {
            true => Err(AppError::MissingUserEmail),
            false => Ok(reviewer),
        }
    }

    fn historical_blobs(
        &self,
        path: &RepoPath,
        current: &BlobOid,
    ) -> Result<Vec<HistoricalBlob>, AppError> {
        let head = self
            .repo
            .head_id()
            .map_err(|err| git_error("reading HEAD", err))?
            .detach();
        let commits = self
            .repo
            .rev_walk([head])
            .sorting(gix::revision::walk::Sorting::ByCommitTime(
                gix::traverse::commit::simple::CommitTimeOrder::NewestFirst,
            ))
            .all()
            .map_err(|err| git_error("walking commit history", err))?;
        let mut followed_path = path.clone();
        let mut previous_blob = None;
        let mut history = Vec::new();

        for info in commits {
            let info = info.map_err(|err| git_error("walking commit history", err))?;
            let commit = info
                .object()
                .map_err(|err| git_error("reading historical commit", err))?;
            let tree = commit
                .tree()
                .map_err(|err| git_error("reading historical tree", err))?;
            if let Some(file) = self.lookup_file_in_tree(&tree, &followed_path)? {
                if file.blob != *current && previous_blob != Some(file.blob) {
                    history.push(HistoricalBlob {
                        blob: file.blob,
                        mode: file.mode,
                    });
                }
                previous_blob = Some(file.blob);
            }

            if let Some(parent_id) = info.parent_ids().next() {
                let parent = parent_id
                    .object()
                    .map_err(|err| git_error("reading historical parent commit", err))?
                    .into_commit();
                let parent_tree = parent
                    .tree()
                    .map_err(|err| git_error("reading historical parent tree", err))?;
                if let Some(source_path) =
                    self.rename_source(&parent_tree, &tree, &followed_path)?
                {
                    followed_path = source_path;
                }
            }
        }

        Ok(history)
    }

    fn diff_empty_to_head(&self, file: &TrackedFile) -> Result<String, AppError> {
        self.render_blob_diff(None, file)
    }

    fn diff_blobs_with_path_label(
        &self,
        baseline: &HistoricalBlob,
        current: &TrackedFile,
    ) -> Result<String, AppError> {
        self.render_blob_diff(Some(baseline), current)
    }

    fn render_blob_diff(
        &self,
        baseline: Option<&HistoricalBlob>,
        current: &TrackedFile,
    ) -> Result<String, AppError> {
        let old_label = baseline
            .map(|_| format!("a/{}", current.path))
            .unwrap_or_else(|| "/dev/null".to_owned());
        let new_label = format!("b/{}", current.path);
        let old_blob = baseline.map(|baseline| baseline.blob);
        let old_mode = baseline.map(|baseline| baseline.mode);
        let old_id = old_blob
            .map(|oid| oid.as_object_id())
            .unwrap_or_else(|| gix::ObjectId::null(self.repo.object_hash()));
        let old_kind = old_mode.map(|mode| mode.kind()).unwrap_or(EntryKind::Blob);
        let mut cache = self
            .repo
            .diff_resource_cache_for_tree_diff()
            .map_err(|err| git_error("creating diff resource cache", err))?;

        cache
            .set_resource(
                old_id,
                old_kind,
                current.path.as_bstr(),
                ResourceKind::OldOrSource,
                &self.repo.objects,
            )
            .map_err(|err| git_error("setting diff source", err))?;
        cache
            .set_resource(
                current.blob.as_object_id(),
                current.mode.kind(),
                current.path.as_bstr(),
                ResourceKind::NewOrDestination,
                &self.repo.objects,
            )
            .map_err(|err| git_error("setting diff destination", err))?;
        cache.options.skip_internal_diff_if_external_is_configured = false;

        let mut output = String::new();
        output.push_str(&format!(
            "diff --git a/{path} b/{path}\n",
            path = current.path
        ));
        match baseline {
            Some(baseline) if baseline.mode != current.mode => {
                output.push_str(&format!("old mode {}\n", baseline.mode.as_octal()));
                output.push_str(&format!("new mode {}\n", current.mode.as_octal()));
                output.push_str(&format!(
                    "index {}..{}\n",
                    baseline.blob.short(),
                    current.blob.short()
                ));
            }
            Some(baseline) => {
                output.push_str(&format!(
                    "index {}..{} {}\n",
                    baseline.blob.short(),
                    current.blob.short(),
                    current.mode.as_octal()
                ));
            }
            None => {
                output.push_str(&format!("new file mode {}\n", current.mode.as_octal()));
                output.push_str(&format!(
                    "index {}..{}\n",
                    zero_oid(self.repo.object_hash()),
                    current.blob.short()
                ));
            }
        }

        let prepared = cache
            .prepare_diff()
            .map_err(|err| git_error("preparing blob diff", err))?;
        match prepared.operation {
            Operation::InternalDiff { algorithm } => {
                output.push_str(&format!("--- {old_label}\n+++ {new_label}\n"));
                let input = prepared.interned_input();
                let diff = gix::diff::blob::diff_with_slider_heuristics(algorithm, &input);
                let hunk = UnifiedDiff::new(
                    &diff,
                    &input,
                    ConsumeBinaryHunk::new(Vec::<u8>::new(), "\n"),
                    ContextSize::default(),
                )
                .consume()?;
                output.push_str(&String::from_utf8_lossy(&hunk));
            }
            Operation::SourceOrDestinationIsBinary => {
                output.push_str(&format!(
                    "Binary files {old_label} and {new_label} differ\n"
                ));
            }
            Operation::ExternalCommand { .. } => unreachable!("external diffs are disabled"),
        }

        Ok(output)
    }

    fn lookup_file_in_tree(
        &self,
        tree: &gix::Tree<'_>,
        path: &RepoPath,
    ) -> Result<Option<TrackedFile>, AppError> {
        let entry = tree
            .lookup_entry_by_path(path.to_path_buf())
            .map_err(|err| git_error("looking up path in tree", err))?;
        match entry {
            Some(entry) => {
                let mode = FileMode::new(entry.mode());
                if mode.is_submodule() {
                    return Err(AppError::PathIsSubmodule(path.clone()));
                }
                match mode.is_reviewable_file() {
                    true => Ok(Some(TrackedFile {
                        path: path.clone(),
                        blob: BlobOid::new(entry.object_id()),
                        mode,
                    })),
                    false => Ok(None),
                }
            }
            None => Ok(None),
        }
    }

    fn rename_source(
        &self,
        old_tree: &gix::Tree<'_>,
        new_tree: &gix::Tree<'_>,
        destination: &RepoPath,
    ) -> Result<Option<RepoPath>, AppError> {
        let changes = self
            .repo
            .diff_tree_to_tree(Some(old_tree), Some(new_tree), None)
            .map_err(|err| git_error("detecting renames", err))?;
        for change in changes {
            match change {
                gix::diff::tree_with_rewrites::Change::Rewrite {
                    source_location,
                    location,
                    copy: false,
                    ..
                } if repo_path_from_bstr(location.as_bstr())? == *destination => {
                    return repo_path_from_bstr(source_location.as_bstr()).map(Some);
                }
                _ => {}
            }
        }
        Ok(None)
    }
}

trait NotesStore {
    fn list_reviewed(&self) -> Result<ReviewedSet, AppError>;
    fn note_body(&self, oid: &BlobOid) -> Result<Option<String>, AppError>;
    fn write_note_body(&self, oid: &BlobOid, body: &str) -> Result<(), AppError>;
    fn prune(&self) -> Result<(), AppError>;
}

#[derive(Clone, Debug)]
struct NoteEntry {
    annotated: BlobOid,
    note_blob: gix::ObjectId,
    path: String,
}

#[derive(Clone)]
struct GixNotesStore<'git> {
    git: &'git Git,
    notes_ref: NotesRef,
}

impl<'git> GixNotesStore<'git> {
    fn new(git: &'git Git, notes_ref: NotesRef) -> Self {
        Self { git, notes_ref }
    }

    fn configure_merge_strategy(&self) -> Result<(), AppError> {
        let config_path = self.git.repo.common_dir().join("config");
        let mut config = match config_path.exists() {
            true => gix_config::File::from_path_no_includes(
                config_path.clone(),
                gix_config::Source::Local,
            )
            .map_err(|err| git_error("reading repository config", err))?,
            false => gix_config::File::default(),
        };
        config
            .set_raw_value(NOTES_MERGE_STRATEGY_KEY, NOTES_MERGE_STRATEGY)
            .map_err(|err| git_error("updating repository config", err))?;
        std::fs::write(config_path, config.to_bstring())?;
        Ok(())
    }

    fn note_entries(&self) -> Result<Vec<NoteEntry>, AppError> {
        let Some(tree) = self.notes_tree()? else {
            return Ok(Vec::new());
        };
        tree.traverse()
            .breadthfirst
            .files()
            .map_err(|err| git_error("walking notes tree", err))?
            .into_iter()
            .filter(|entry| entry.mode.is_blob())
            .filter_map(|entry| self.note_entry_from_tree_record(entry).transpose())
            .collect()
    }

    fn note_entry_from_tree_record(
        &self,
        entry: gix::traverse::tree::recorder::Entry,
    ) -> Result<Option<NoteEntry>, AppError> {
        let note_path = entry
            .filepath
            .to_str()
            .map_err(|err| AppError::NonUtf8Path(err.to_string()))?
            .to_owned();
        let hex = note_path.replace('/', "");
        if hex.len() != self.git.repo.object_hash().len_in_hex() {
            return Ok(None);
        }
        let annotated = match gix::ObjectId::from_hex(hex.as_bytes()) {
            Ok(oid) => BlobOid::new(oid),
            Err(_) => return Ok(None),
        };
        Ok(Some(NoteEntry {
            annotated,
            note_blob: entry.oid,
            path: note_path,
        }))
    }

    fn note_entry(&self, oid: &BlobOid) -> Result<Option<NoteEntry>, AppError> {
        self.note_entries()
            .map(|entries| entries.into_iter().find(|entry| entry.annotated == *oid))
    }

    fn note_path(&self, oid: &BlobOid) -> Result<String, AppError> {
        self.note_entry(oid)
            .map(|entry| entry.map(|entry| entry.path))
            .map(|path| path.unwrap_or_else(|| oid.to_string()))
    }

    fn notes_tree(&self) -> Result<Option<gix::Tree<'_>>, AppError> {
        let reference = self
            .git
            .repo
            .try_find_reference(self.notes_ref.as_str())
            .map_err(|err| git_error("finding notes ref", err))?;
        match reference {
            Some(mut reference) => reference
                .peel_to_tree()
                .map(Some)
                .map_err(|err| git_error("reading notes tree", err)),
            None => Ok(None),
        }
    }

    fn notes_tree_id(&self) -> Result<Option<gix::ObjectId>, AppError> {
        self.notes_tree()
            .map(|tree| tree.map(|tree| tree.id().detach()))
    }

    fn notes_parent_commit(&self) -> Result<Option<gix::ObjectId>, AppError> {
        let Some(mut reference) = self
            .git
            .repo
            .try_find_reference(self.notes_ref.as_str())
            .map_err(|err| git_error("finding notes ref", err))?
        else {
            return Ok(None);
        };
        let target = reference
            .follow_to_object()
            .map_err(|err| git_error("resolving notes ref", err))?
            .detach();
        let object = self
            .git
            .repo
            .find_object(target)
            .map_err(|err| git_error("reading notes ref target", err))?;
        match object.kind {
            gix::objs::Kind::Commit => Ok(Some(target)),
            gix::objs::Kind::Tree => Err(AppError::InvalidNotesRefTarget { actual: "tree" }),
            gix::objs::Kind::Blob => Err(AppError::InvalidNotesRefTarget { actual: "blob" }),
            gix::objs::Kind::Tag => Err(AppError::InvalidNotesRefTarget { actual: "tag" }),
        }
    }

    fn commit_notes_tree(&self, tree_id: gix::ObjectId) -> Result<(), AppError> {
        let parent = self.notes_parent_commit()?;
        let parents = parent.into_iter().collect::<Vec<_>>();
        self.git
            .repo
            .commit(
                self.notes_ref.full_name(),
                "git-vet notes",
                tree_id,
                parents,
            )
            .map(|_| ())
            .map_err(|err| git_error("committing notes tree", err))
    }

    fn rewrite_notes_tree(
        &self,
        edit: impl FnOnce(&mut gix::object::tree::Editor<'_>) -> Result<(), AppError>,
    ) -> Result<(), AppError> {
        let base_tree = self
            .notes_tree_id()?
            .unwrap_or_else(|| gix::ObjectId::empty_tree(self.git.repo.object_hash()));
        let mut editor = self
            .git
            .repo
            .edit_tree(base_tree)
            .map_err(|err| git_error("editing notes tree", err))?;
        edit(&mut editor)?;
        let new_tree = editor
            .write()
            .map_err(|err| git_error("writing notes tree", err))?
            .detach();
        if new_tree != base_tree {
            self.commit_notes_tree(new_tree)?;
        }
        Ok(())
    }
}

impl NotesStore for GixNotesStore<'_> {
    fn list_reviewed(&self) -> Result<ReviewedSet, AppError> {
        self.note_entries()?
            .into_iter()
            .try_fold(ReviewedSet::default(), |mut reviewed, entry| {
                let mut body = self
                    .git
                    .repo
                    .find_blob(entry.note_blob)
                    .map_err(|err| git_error("reading note body", err))?;
                let body = String::from_utf8(body.take_data())
                    .map_err(|err| AppError::NonUtf8Path(err.to_string()))?;
                let records = parse_note_records(&body);
                reviewed
                    .by_blob
                    .insert(entry.annotated, ReviewInfo { records });
                Ok(reviewed)
            })
    }

    fn note_body(&self, oid: &BlobOid) -> Result<Option<String>, AppError> {
        let Some(entry) = self.note_entry(oid)? else {
            return Ok(None);
        };
        let mut body = self
            .git
            .repo
            .find_blob(entry.note_blob)
            .map_err(|err| git_error("reading note body", err))?;
        String::from_utf8(body.take_data())
            .map(Some)
            .map_err(|err| AppError::NonUtf8Path(err.to_string()))
    }

    fn write_note_body(&self, oid: &BlobOid, body: &str) -> Result<(), AppError> {
        self.configure_merge_strategy()?;
        let note_path = self.note_path(oid)?;
        let note_blob = self
            .git
            .repo
            .write_blob(body.as_bytes())
            .map_err(|err| git_error("writing note blob", err))?
            .detach();
        self.rewrite_notes_tree(|editor| {
            editor
                .upsert(&note_path, EntryKind::Blob, note_blob)
                .map_err(|err| git_error("updating note entry", err))?;
            Ok(())
        })
    }

    fn prune(&self) -> Result<(), AppError> {
        let entries = self.note_entries()?;
        let stale_paths = entries
            .into_iter()
            .filter(|entry| !self.git.repo.has_object(entry.annotated.as_object_id()))
            .map(|entry| entry.path)
            .collect::<Vec<_>>();
        if stale_paths.is_empty() {
            return Ok(());
        }
        self.rewrite_notes_tree(|editor| {
            stale_paths.iter().try_for_each(|path| {
                editor
                    .remove(path)
                    .map_err(|err| git_error("removing stale note", err))?;
                println!("Removing note for object {path}");
                Ok(())
            })
        })
    }
}

#[derive(Clone, Copy, Debug)]
struct StatusMode {
    json: bool,
    check: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Gate {
    Open,
    Closed,
}

fn mark_paths(git: &Git, notes: &impl NotesStore, paths: Vec<PathBuf>) -> Result<(), AppError> {
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

fn status(
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

fn diff_path(git: &Git, notes: &impl NotesStore, path: PathBuf) -> Result<(), AppError> {
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

fn print_json_status(
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

fn print_human_status(channel: &ReviewChannel, classified: &[ClassifiedFile]) {
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

fn append_record(existing: Option<&str>, new_record: &ReviewRecord) -> String {
    let already_recorded = existing
        .map(parse_note_records)
        .unwrap_or_default()
        .iter()
        .any(|record| {
            record.reviewer == new_record.reviewer
                && record.commit == new_record.commit
                && record.path == new_record.path
        });

    let mut lines = existing
        .into_iter()
        .flat_map(str::lines)
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(ToOwned::to_owned)
        .chain((!already_recorded).then(|| new_record.render()))
        .collect::<Vec<_>>();
    lines.sort();
    lines.dedup();

    match lines.is_empty() {
        true => String::new(),
        false => format!("{}\n", lines.join("\n")),
    }
}

fn parse_note_records(body: &str) -> Vec<ReviewRecord> {
    body.lines().filter_map(parse_note_record).collect()
}

fn parse_note_record(line: &str) -> Option<ReviewRecord> {
    let fields = line
        .split_whitespace()
        .filter_map(|field| field.split_once('='))
        .collect::<HashMap<_, _>>();

    Some(ReviewRecord {
        reviewed_at: fields.get("reviewed-at")?.to_string(),
        reviewer: fields.get("reviewer")?.to_string(),
        commit: CommitOid::new(gix::ObjectId::from_hex(fields.get("commit")?.as_bytes()).ok()?),
        path: RepoPath::from_git_path(fields.get("path")?).ok()?,
    })
}

#[derive(Debug)]
struct Vetignore {
    matcher: Gitignore,
}

impl Vetignore {
    fn load(root: &Path) -> Result<Self, AppError> {
        let path = root.join(".vetignore");
        let mut builder = GitignoreBuilder::new(root);
        if let Some(error) = path.exists().then(|| builder.add(&path)).flatten() {
            return Err(AppError::Vetignore(error.to_string()));
        }
        let matcher = builder
            .build()
            .map_err(|error| AppError::Vetignore(error.to_string()))?;
        Ok(Self { matcher })
    }

    fn is_ignored(&self, path: &RepoPath) -> bool {
        self.matcher
            .matched_path_or_any_parents(path.to_path_buf(), false)
            .is_ignore()
    }
}

fn prefix_from_cwd(root: &Path, cwd: &Path) -> Result<PathBuf, AppError> {
    let root = normalize_lexically(root);
    let cwd = normalize_lexically(cwd);
    cwd.strip_prefix(&root)
        .map(Path::to_path_buf)
        .map_err(|_| AppError::PathOutsideRepo(cwd.display().to_string()))
}

fn repo_path_from_bstr(path: &gix::bstr::BStr) -> Result<RepoPath, AppError> {
    let path = path
        .to_str()
        .map_err(|err| AppError::NonUtf8Path(err.to_string()))?;
    RepoPath::from_git_path(path)
}

fn zero_oid(kind: gix::hash::Kind) -> String {
    "0".repeat(kind.len_in_hex())
}

fn normalize_lexically(path: &Path) -> PathBuf {
    path.components()
        .fold(PathBuf::new(), |mut normalized, component| {
            match component {
                Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
                Component::RootDir => normalized.push(component.as_os_str()),
                Component::CurDir => {}
                Component::ParentDir => {
                    normalized.pop();
                }
                Component::Normal(part) => normalized.push(part),
            }
            normalized
        })
}

fn repo_path_from_relative(path: &Path) -> Result<String, AppError> {
    let parts = path
        .components()
        .map(|component| match component {
            Component::Normal(part) => part
                .to_str()
                .map(ToOwned::to_owned)
                .ok_or_else(|| AppError::NonUtf8Path(path.display().to_string())),
            Component::CurDir => Ok(String::new()),
            Component::Prefix(_) | Component::RootDir | Component::ParentDir => {
                Err(AppError::PathOutsideRepo(path.display().to_string()))
            }
        })
        .filter(|part| !matches!(part, Ok(value) if value.is_empty()))
        .collect::<Result<Vec<_>, _>>()?;

    match parts.is_empty() {
        true => Err(AppError::EmptyPath),
        false => Ok(parts.join("/")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn oid(hex: &str) -> gix::ObjectId {
        gix::ObjectId::from_hex(hex.as_bytes()).unwrap()
    }

    #[test]
    fn append_record_sorts_and_deduplicates_records() {
        let record = ReviewRecord {
            reviewed_at: "2026-06-06T00:00:00Z".to_owned(),
            reviewer: "reviewer@example.com".to_owned(),
            commit: CommitOid::new(oid("0123456789012345678901234567890123456789")),
            path: RepoPath::from_git_path("src/main.rs").unwrap(),
        };
        let existing = "reviewed-at=2026-06-06T00:00:00Z reviewer=reviewer@example.com commit=0123456789012345678901234567890123456789 path=src/main.rs\n";

        assert_eq!(append_record(Some(existing), &record), existing);
    }

    #[test]
    fn repo_path_from_relative_rejects_empty_paths() {
        assert!(matches!(
            repo_path_from_relative(Path::new("")),
            Err(AppError::EmptyPath)
        ));
    }
}
