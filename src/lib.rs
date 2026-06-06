use std::collections::HashMap;
use std::env;
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::io::Write;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use chrono::{SecondsFormat, Utc};
use clap::{Parser, Subcommand};
use ignore::gitignore::{Gitignore, GitignoreBuilder};
use serde::Serialize;
use thiserror::Error;

const NOTES_REF: &str = "vet";
const ZERO_OID_PREFIX: char = '0';

#[derive(Parser, Debug)]
#[command(
    name = "git-vet",
    version,
    about = "Track human review state for Git-tracked file contents"
)]
pub struct Cli {
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
    let git = Git::discover()?;
    let notes = GitNotesStore::new(git.clone());

    match cli.command {
        CommandKind::Mark { paths } => {
            mark_paths(&git, &notes, paths)?;
            Ok(ExitCode::SUCCESS)
        }
        CommandKind::Status { json, check } => {
            match status(&git, &notes, StatusMode { json, check })? {
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
    #[error("git command failed (exit {code:?}): git {args}\n{stderr}")]
    GitCommand {
        args: String,
        code: Option<i32>,
        stderr: String,
    },
    #[error("failed to run git: {0}")]
    GitIo(#[source] std::io::Error),
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
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
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

    fn as_str(&self) -> &str {
        &self.0
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

#[derive(Clone, Debug, Eq, PartialEq, Hash, Ord, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct BlobOid(String);

impl BlobOid {
    fn new(oid: impl Into<String>) -> Self {
        Self(oid.into())
    }

    fn as_str(&self) -> &str {
        &self.0
    }

    fn is_all_zero(&self) -> bool {
        self.0.chars().all(|ch| ch == ZERO_OID_PREFIX)
    }
}

impl fmt::Display for BlobOid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
struct CommitOid(String);

impl CommitOid {
    fn new(oid: impl Into<String>) -> Self {
        Self(oid.into())
    }
}

impl fmt::Display for CommitOid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
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
    Stale { baseline: BlobOid },
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
            Self::Stale { baseline } => Some(baseline),
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
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum GitObjectKind {
    Blob,
    Tree,
    Commit,
    Other(String),
}

impl GitObjectKind {
    fn from_git(kind: &str) -> Self {
        match kind {
            "blob" => Self::Blob,
            "tree" => Self::Tree,
            "commit" => Self::Commit,
            other => Self::Other(other.to_owned()),
        }
    }
}

#[derive(Clone, Debug)]
struct TreeEntry {
    kind: GitObjectKind,
    oid: BlobOid,
    path: RepoPath,
}

#[derive(Clone, Debug)]
struct Git {
    root: PathBuf,
    prefix: PathBuf,
}

impl Git {
    fn discover() -> Result<Self, AppError> {
        let root_stdout = run_git_from_current(["rev-parse", "--show-toplevel"])?;
        let root = PathBuf::from(trim_stdout(&root_stdout));
        let prefix = match env::var_os("GIT_PREFIX") {
            Some(value) if !value.is_empty() => PathBuf::from(value),
            _ => {
                let prefix_stdout = run_git_from_current(["rev-parse", "--show-prefix"])?;
                PathBuf::from(trim_stdout(&prefix_stdout))
            }
        };

        Ok(Self { root, prefix })
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
        self.ls_tree(["-rz", "--full-tree", "HEAD"])?
            .into_iter()
            .filter_map(|entry| match entry.kind {
                GitObjectKind::Blob => Some(Ok(TrackedFile {
                    path: entry.path,
                    blob: entry.oid,
                })),
                GitObjectKind::Commit | GitObjectKind::Tree | GitObjectKind::Other(_) => None,
            })
            .collect::<Result<Vec<_>, _>>()
            .map(|mut files| {
                files.sort_by(|left, right| left.path.cmp(&right.path));
                files
            })
    }

    fn blob_at_head(&self, path: &RepoPath) -> Result<BlobOid, AppError> {
        let entries = self.ls_tree(["-z", "HEAD", "--", path.as_str()])?;
        match entries.into_iter().find(|entry| entry.path == *path) {
            Some(TreeEntry {
                kind: GitObjectKind::Blob,
                oid,
                ..
            }) => Ok(oid),
            Some(TreeEntry {
                kind: GitObjectKind::Commit,
                ..
            }) => Err(AppError::PathIsSubmodule(path.clone())),
            Some(_) | None => Err(AppError::PathNotTracked(path.clone())),
        }
    }

    fn head_commit(&self) -> Result<CommitOid, AppError> {
        self.run_string(["rev-parse", "HEAD"])
            .map(|oid| CommitOid::new(oid.trim().to_owned()))
    }

    fn reviewer(&self) -> Result<String, AppError> {
        let reviewer = self.run_string(["config", "user.email"])?;
        let reviewer = reviewer.trim().to_owned();
        match reviewer.is_empty() {
            true => Err(AppError::MissingUserEmail),
            false => Ok(reviewer),
        }
    }

    fn historical_blobs(
        &self,
        path: &RepoPath,
        current: &BlobOid,
    ) -> Result<Vec<BlobOid>, AppError> {
        let output = self.run_string([
            "log",
            "--follow",
            "--raw",
            "--no-abbrev",
            "--format=format:",
            "--",
            path.as_str(),
        ])?;

        Ok(output
            .lines()
            .filter_map(parse_raw_log_new_oid)
            .filter(|oid| !oid.is_all_zero() && oid != current)
            .collect())
    }

    fn empty_tree_oid(&self) -> Result<BlobOid, AppError> {
        let output = self.try_run_with_stdin(["mktree"], b"")?;
        match output.status.success() {
            true => String::from_utf8(output.stdout)
                .map(|oid| BlobOid::new(oid.trim().to_owned()))
                .map_err(|err| AppError::NonUtf8Path(err.to_string())),
            false => Err(AppError::GitCommand {
                args: output.args,
                code: output.status.code(),
                stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            }),
        }
    }

    fn diff_empty_tree_to_head(&self, path: &RepoPath) -> Result<String, AppError> {
        let empty_tree = self.empty_tree_oid()?;
        self.run_string([
            "diff",
            "--no-ext-diff",
            empty_tree.as_str(),
            "HEAD",
            "--",
            path.as_str(),
        ])
    }

    fn diff_blobs_with_path_label(
        &self,
        baseline: &BlobOid,
        current: &BlobOid,
        path: &RepoPath,
    ) -> Result<String, AppError> {
        let tempdir = ScopedTempDir::new("git-vet-diff")?;
        let baseline_path = tempdir.path().join("baseline").join(path.to_path_buf());
        let current_path = tempdir.path().join("current").join(path.to_path_buf());
        write_blob_file(&baseline_path, &self.cat_blob(baseline)?)?;
        write_blob_file(&current_path, &self.cat_blob(current)?)?;

        let output = self.try_run([
            OsString::from("diff"),
            OsString::from("--no-ext-diff"),
            OsString::from("--no-index"),
            baseline_path.as_os_str().to_owned(),
            current_path.as_os_str().to_owned(),
        ])?;

        match output.status.code() {
            Some(0 | 1) => String::from_utf8(output.stdout)
                .map(|diff| relabel_no_index_diff(&diff, &baseline_path, &current_path, path))
                .map_err(|err| AppError::NonUtf8Path(err.to_string())),
            code => Err(AppError::GitCommand {
                args: output.args,
                code,
                stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            }),
        }
    }

    fn cat_blob(&self, oid: &BlobOid) -> Result<Vec<u8>, AppError> {
        self.run_bytes(["cat-file", "blob", oid.as_str()])
    }

    fn ls_tree<I, S>(&self, args: I) -> Result<Vec<TreeEntry>, AppError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let args = std::iter::once(OsString::from("ls-tree"))
            .chain(args.into_iter().map(|arg| arg.as_ref().to_owned()))
            .collect::<Vec<_>>();
        let output = self.run_bytes(args)?;
        parse_ls_tree(&output)
    }

    fn run_string<I, S>(&self, args: I) -> Result<String, AppError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let output = self.run_bytes(args)?;
        String::from_utf8(output).map_err(|err| AppError::NonUtf8Path(err.to_string()))
    }

    fn run_bytes<I, S>(&self, args: I) -> Result<Vec<u8>, AppError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let output = self.try_run(args)?;
        match output.status.success() {
            true => Ok(output.stdout),
            false => Err(AppError::GitCommand {
                args: output.args,
                code: output.status.code(),
                stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            }),
        }
    }

    fn try_run<I, S>(&self, args: I) -> Result<GitOutput, AppError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        run_git(Some(&self.root), args, None)
    }

    fn try_run_with_stdin<I, S>(&self, args: I, stdin: &[u8]) -> Result<GitOutput, AppError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        run_git(Some(&self.root), args, Some(stdin))
    }
}

#[derive(Debug)]
struct GitOutput {
    args: String,
    status: std::process::ExitStatus,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

trait NotesStore {
    fn list_reviewed(&self) -> Result<ReviewedSet, AppError>;
    fn note_body(&self, oid: &BlobOid) -> Result<Option<String>, AppError>;
    fn write_note_body(&self, oid: &BlobOid, body: &str) -> Result<(), AppError>;
    fn prune(&self) -> Result<(), AppError>;
}

#[derive(Clone, Debug)]
struct GitNotesStore {
    git: Git,
}

impl GitNotesStore {
    fn new(git: Git) -> Self {
        Self { git }
    }

    fn configure_merge_strategy(&self) -> Result<(), AppError> {
        self.git
            .run_bytes(["config", "notes.mergeStrategy", "cat_sort_uniq"])
            .map(|_| ())
    }
}

impl NotesStore for GitNotesStore {
    fn list_reviewed(&self) -> Result<ReviewedSet, AppError> {
        let output = self.git.run_string(["notes", "--ref", NOTES_REF, "list"])?;
        output
            .lines()
            .filter_map(|line| {
                line.split_once(' ')
                    .map(|(_, annotated)| annotated.to_owned())
            })
            .try_fold(ReviewedSet::default(), |mut reviewed, oid| {
                let blob = BlobOid::new(oid);
                let records = self
                    .note_body(&blob)?
                    .map(|body| parse_note_records(&body))
                    .unwrap_or_default();
                reviewed.by_blob.insert(blob, ReviewInfo { records });
                Ok(reviewed)
            })
    }

    fn note_body(&self, oid: &BlobOid) -> Result<Option<String>, AppError> {
        let output = self
            .git
            .try_run(["notes", "--ref", NOTES_REF, "show", oid.as_str()])?;
        match output.status.success() {
            true => String::from_utf8(output.stdout)
                .map(Some)
                .map_err(|err| AppError::NonUtf8Path(err.to_string())),
            false if output.status.code() == Some(1) => Ok(None),
            false => Err(AppError::GitCommand {
                args: output.args,
                code: output.status.code(),
                stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            }),
        }
    }

    fn write_note_body(&self, oid: &BlobOid, body: &str) -> Result<(), AppError> {
        self.configure_merge_strategy()?;
        let output = self.git.try_run_with_stdin(
            [
                "notes",
                "--ref",
                NOTES_REF,
                "add",
                "-f",
                "-F",
                "-",
                oid.as_str(),
            ],
            body.as_bytes(),
        )?;
        match output.status.success() {
            true => Ok(()),
            false => Err(AppError::GitCommand {
                args: output.args,
                code: output.status.code(),
                stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            }),
        }
    }

    fn prune(&self) -> Result<(), AppError> {
        let stdout = self
            .git
            .run_string(["notes", "--ref", NOTES_REF, "prune"])?;
        print!("{stdout}");
        Ok(())
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
        .map(|path| git.blob_at_head(path).map(|blob| (path.clone(), blob)))
        .collect::<Result<Vec<_>, _>>()?;
    let reviewer = git.reviewer()?;
    let commit = git.head_commit()?;
    let reviewed_at = Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true);

    targets.iter().try_for_each(|(path, blob)| {
        let record = ReviewRecord {
            reviewed_at: reviewed_at.clone(),
            reviewer: reviewer.clone(),
            commit: commit.clone(),
            path: path.clone(),
        };
        let body = append_record(notes.note_body(blob)?.as_deref(), &record);
        notes.write_note_body(blob, &body)?;
        println!("marked {path}");
        Ok(())
    })
}

fn status(git: &Git, notes: &impl NotesStore, mode: StatusMode) -> Result<Gate, AppError> {
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
                true => print_json_status(&classified)?,
                false => print_human_status(&classified),
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
    let blob = git.blob_at_head(&path)?;
    let reviewed = notes.list_reviewed()?;
    let file = TrackedFile {
        path: path.clone(),
        blob: blob.clone(),
    };
    let classified = classify_path(git, &file, &reviewed)?;

    match classified.state {
        ReviewState::Vetted => {
            println!("{path} is up to date");
            Ok(())
        }
        ReviewState::New => {
            print!("{}", git.diff_empty_tree_to_head(&path)?);
            Ok(())
        }
        ReviewState::Stale { baseline } => {
            print!(
                "{}",
                git.diff_blobs_with_path_label(&baseline, &blob, &path)?
            );
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
            blob: file.blob.clone(),
            metadata: reviewed.metadata(&file.blob),
        }),
        false => {
            let baseline = git
                .historical_blobs(&file.path, &file.blob)?
                .into_iter()
                .find(|oid| reviewed.contains(oid));
            let metadata = baseline.as_ref().and_then(|oid| reviewed.metadata(oid));
            let state = baseline
                .map(|baseline| ReviewState::Stale { baseline })
                .unwrap_or(ReviewState::New);
            Ok(ClassifiedFile {
                path: file.path.clone(),
                state,
                blob: file.blob.clone(),
                metadata,
            })
        }
    }
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

fn print_json_status(classified: &[ClassifiedFile]) -> Result<(), AppError> {
    let records = classified
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
    println!("{}", serde_json::to_string_pretty(&records)?);
    Ok(())
}

fn print_human_status(classified: &[ClassifiedFile]) {
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
        commit: CommitOid::new(fields.get("commit")?.to_string()),
        path: RepoPath::from_git_path(fields.get("path")?).ok()?,
    })
}

fn parse_raw_log_new_oid(line: &str) -> Option<BlobOid> {
    let raw = line.strip_prefix(':')?;
    let (metadata, _) = raw.split_once('\t')?;
    let fields = metadata.split_whitespace().collect::<Vec<_>>();
    match fields.as_slice() {
        [_, _, _, new_oid, _status] => Some(BlobOid::new(*new_oid)),
        _ => None,
    }
}

#[derive(Debug)]
struct Vetignore {
    matcher: Gitignore,
}

impl Vetignore {
    fn load(root: &Path) -> Result<Self, AppError> {
        let path = root.join(".vetignore");
        let mut builder = GitignoreBuilder::new(root);
        if path.exists() {
            if let Some(error) = builder.add(&path) {
                return Err(AppError::Vetignore(error.to_string()));
            }
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

struct ScopedTempDir {
    path: PathBuf,
}

impl ScopedTempDir {
    fn new(prefix: &str) -> std::io::Result<Self> {
        let base = env::temp_dir();
        let pid = std::process::id();
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or_default();

        for attempt in 0..1000 {
            let path = base.join(format!("{prefix}-{pid}-{nanos}-{attempt}"));
            match std::fs::create_dir(&path) {
                Ok(()) => return Ok(Self { path }),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error),
            }
        }

        Err(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            "could not create a unique temporary directory",
        ))
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for ScopedTempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

fn write_blob_file(path: &Path, contents: &[u8]) -> Result<(), AppError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, contents)?;
    Ok(())
}

fn relabel_no_index_diff(
    diff: &str,
    baseline_path: &Path,
    current_path: &Path,
    repo_path: &RepoPath,
) -> String {
    let baseline = baseline_path.to_string_lossy();
    let current = current_path.to_string_lossy();
    [("1", "2"), ("a", "b")]
        .into_iter()
        .fold(diff.to_owned(), |diff, (old_prefix, new_prefix)| {
            diff.replace(
                &format!("{old_prefix}{baseline}"),
                &format!("a/{repo_path}"),
            )
            .replace(&format!("{new_prefix}{current}"), &format!("b/{repo_path}"))
        })
}

fn parse_ls_tree(output: &[u8]) -> Result<Vec<TreeEntry>, AppError> {
    output
        .split(|byte| *byte == b'\0')
        .filter(|record| !record.is_empty())
        .map(parse_ls_tree_record)
        .collect()
}

fn parse_ls_tree_record(record: &[u8]) -> Result<TreeEntry, AppError> {
    let text =
        String::from_utf8(record.to_vec()).map_err(|err| AppError::NonUtf8Path(err.to_string()))?;
    let (metadata, path) = text
        .split_once('\t')
        .ok_or_else(|| AppError::NonUtf8Path("malformed ls-tree record".to_owned()))?;
    let fields = metadata.split_whitespace().collect::<Vec<_>>();
    match fields.as_slice() {
        [_mode, kind, oid] => Ok(TreeEntry {
            kind: GitObjectKind::from_git(kind),
            oid: BlobOid::new(*oid),
            path: RepoPath::from_git_path(path)?,
        }),
        _ => Err(AppError::NonUtf8Path(
            "malformed ls-tree metadata".to_owned(),
        )),
    }
}

fn run_git_from_current<I, S>(args: I) -> Result<Vec<u8>, AppError>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let output = run_git(None, args, None)?;
    match output.status.success() {
        true => Ok(output.stdout),
        false => Err(AppError::GitCommand {
            args: output.args,
            code: output.status.code(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        }),
    }
}

fn run_git<I, S>(cwd: Option<&Path>, args: I, stdin: Option<&[u8]>) -> Result<GitOutput, AppError>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let args = args
        .into_iter()
        .map(|arg| arg.as_ref().to_owned())
        .collect::<Vec<_>>();
    let display_args = args
        .iter()
        .map(|arg| shell_display(arg))
        .collect::<Vec<_>>()
        .join(" ");

    let mut command = Command::new("git");
    if let Some(cwd) = cwd {
        command.arg("-C").arg(cwd);
    }
    command.args(&args);
    command.stdout(Stdio::piped()).stderr(Stdio::piped());

    let output = match stdin {
        Some(input) => {
            let mut child = command
                .stdin(Stdio::piped())
                .spawn()
                .map_err(AppError::GitIo)?;
            child
                .stdin
                .take()
                .expect("stdin is piped")
                .write_all(input)?;
            child.wait_with_output().map_err(AppError::GitIo)?
        }
        None => command.output().map_err(AppError::GitIo)?,
    };

    Ok(GitOutput {
        args: display_args,
        status: output.status,
        stdout: output.stdout,
        stderr: output.stderr,
    })
}

fn trim_stdout(output: &[u8]) -> String {
    String::from_utf8_lossy(output)
        .trim_end_matches(['\r', '\n'])
        .to_string()
}

fn shell_display(arg: &OsStr) -> String {
    let value = arg.to_string_lossy();
    match value
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || "@%_+=:,./-".contains(ch))
    {
        true => value.into_owned(),
        false => format!("'{value}'"),
    }
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

    #[test]
    fn append_record_sorts_and_deduplicates_records() {
        let record = ReviewRecord {
            reviewed_at: "2026-06-06T00:00:00Z".to_owned(),
            reviewer: "reviewer@example.com".to_owned(),
            commit: CommitOid::new("abc"),
            path: RepoPath::from_git_path("src/main.rs").unwrap(),
        };
        let existing = "reviewed-at=2026-06-06T00:00:00Z reviewer=reviewer@example.com commit=abc path=src/main.rs\n";

        assert_eq!(append_record(Some(existing), &record), existing);
    }

    #[test]
    fn parse_raw_log_line_returns_new_oid() {
        let line = ":100644 100644 oldoid newoid M\tpath";

        assert_eq!(parse_raw_log_new_oid(line), Some(BlobOid::new("newoid")));
    }

    #[test]
    fn relabel_no_index_diff_handles_git_prefix_variants() {
        let baseline = Path::new("/tmp/git-vet/baseline/a.txt");
        let current = Path::new("/tmp/git-vet/current/a.txt");
        let path = RepoPath::from_git_path("a.txt").unwrap();

        let numbered = "diff --git 1/tmp/git-vet/baseline/a.txt 2/tmp/git-vet/current/a.txt\n--- 1/tmp/git-vet/baseline/a.txt\n+++ 2/tmp/git-vet/current/a.txt\n";
        assert_eq!(
            relabel_no_index_diff(numbered, baseline, current, &path),
            "diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n"
        );

        let lettered = "diff --git a/tmp/git-vet/baseline/a.txt b/tmp/git-vet/current/a.txt\n--- a/tmp/git-vet/baseline/a.txt\n+++ b/tmp/git-vet/current/a.txt\n";
        assert_eq!(
            relabel_no_index_diff(lettered, baseline, current, &path),
            "diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n"
        );
    }
}
