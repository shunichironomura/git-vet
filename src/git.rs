use std::collections::BTreeSet;
use std::env;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use gix::bstr::ByteSlice;

use crate::error::{AppError, git_error};
use crate::git_types::{BlobOid, CommitOid, FileMode, TrackedFile};
use crate::path::{
    PathError, RepoPath, normalize_absolute_lexically, prefix_from_cwd, repo_path_from_bstr,
    repo_path_from_relative,
};
use crate::remote::{RemoteError, RemoteName, RemoteNameSource};
use crate::review::Vetter;

pub(crate) struct Git {
    pub(crate) repo: gix::Repository,
    pub(crate) root: PathBuf,
    prefix: PathBuf,
}

impl Git {
    pub(crate) fn discover() -> Result<Self, AppError> {
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

    pub(crate) fn normalize_user_path(&self, input: &Path) -> Result<RepoPath, AppError> {
        let joined = if input.is_absolute() {
            input.to_path_buf()
        } else {
            self.root.join(&self.prefix).join(input)
        };
        let normalized = normalize_absolute_lexically(&joined)?;
        let root = normalize_absolute_lexically(&self.root)?;
        let relative = normalized
            .strip_prefix(&root)
            .map_err(|_| AppError::from(PathError::OutsideRepo))?;
        repo_path_from_relative(relative).map_err(AppError::from)
    }

    pub(crate) fn tracked_files_at_head(&self) -> Result<Vec<TrackedFile>, AppError> {
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

    pub(crate) fn blob_at_head(&self, path: &RepoPath) -> Result<TrackedFile, AppError> {
        let tree = self
            .repo
            .head_tree()
            .map_err(|err| git_error("reading HEAD tree", err))?;
        Self::lookup_file_in_tree(&tree, path)?
            .ok_or_else(|| AppError::PathNotTracked(path.clone()))
    }

    pub(crate) fn head_commit(&self) -> Result<CommitOid, AppError> {
        self.repo
            .head_id()
            .map(|id| CommitOid::new(id.detach()))
            .map_err(|err| git_error("reading HEAD", err))
    }

    pub(crate) fn dirty_paths_against_head(
        &self,
        files: &[TrackedFile],
    ) -> Result<Vec<RepoPath>, AppError> {
        let dirty_paths = files.iter().try_fold(BTreeSet::new(), |mut dirty, file| {
            if self.path_has_worktree_changes_against_head(&file.path)? {
                dirty.insert(file.path.clone());
            }
            Ok::<_, AppError>(dirty)
        })?;
        Ok(dirty_paths.into_iter().collect())
    }

    fn path_has_worktree_changes_against_head(&self, path: &RepoPath) -> Result<bool, AppError> {
        let output = self
            .git_command()
            .arg("-c")
            .arg("core.fileMode=false")
            .arg("diff")
            .arg("--quiet")
            .arg("--no-ext-diff")
            .arg("--no-textconv")
            .arg("HEAD")
            .arg("--")
            .arg(path.to_os_path_buf())
            .output()?;

        match output.status.code() {
            Some(0) => Ok(false),
            Some(1) => Ok(true),
            _ => Err(git_error(
                "checking working-tree changes",
                command_failure_details(&output),
            )),
        }
    }

    pub(crate) fn vetter(&self) -> Result<Vetter, AppError> {
        let name = self.required_config_value(ReviewerConfigKey::UserName)?;
        let email = self.required_config_value(ReviewerConfigKey::UserEmail)?;
        Ok(Vetter::new(name, email))
    }

    pub(crate) fn configured_review_channel(&self) -> Result<Option<String>, AppError> {
        self.optional_config_value("vet.channel")
    }

    pub(crate) fn select_sync_remote(
        &self,
        explicit: Option<&str>,
    ) -> Result<RemoteName, AppError> {
        match explicit {
            Some(remote) => self.selected_usable_remote(remote, RemoteNameSource::Cli),
            None => self.optional_config_value("vet.syncRemote")?.map_or_else(
                || self.origin_fallback_sync_remote(),
                |remote| self.selected_usable_remote(&remote, RemoteNameSource::Config),
            ),
        }
    }

    fn origin_fallback_sync_remote(&self) -> Result<RemoteName, AppError> {
        let remote = RemoteName::new("origin", RemoteNameSource::OriginFallback)?;
        match self.remote_url(&remote)? {
            Some(_) => Ok(remote),
            None => Err(RemoteError::NoRemoteSelected.into()),
        }
    }

    fn selected_usable_remote(
        &self,
        remote: &str,
        source: RemoteNameSource,
    ) -> Result<RemoteName, AppError> {
        let remote = RemoteName::new(remote, source)?;
        match self.remote_url(&remote)? {
            Some(_) => Ok(remote),
            None => Err(RemoteError::UnusableRemote {
                remote: remote.to_string(),
                name_source: source,
                details: "remote does not exist or has no fetch URL".to_owned(),
            }
            .into()),
        }
    }

    fn remote_url(&self, remote: &RemoteName) -> Result<Option<String>, AppError> {
        let output = self
            .git_command()
            .arg("remote")
            .arg("get-url")
            .arg(remote.as_str())
            .output()?;

        if output.status.success() {
            let url = String::from_utf8(output.stdout)
                .map_err(|err| git_error("decoding remote URL", err))?
                .trim()
                .to_owned();
            Ok((!url.is_empty()).then_some(url))
        } else {
            Ok(None)
        }
    }

    fn required_config_value(&self, key: ReviewerConfigKey) -> Result<String, AppError> {
        match self.optional_config_value(key.name())? {
            Some(value) if !value.is_empty() => Ok(value),
            Some(_) | None => Err(key.missing_error()),
        }
    }

    fn optional_config_value(&self, key: &'static str) -> Result<Option<String>, AppError> {
        let output = self
            .git_command()
            .arg("config")
            .arg("get")
            .arg("--null")
            .arg(key)
            .output()?;

        if output.status.code() == Some(1) {
            return Ok(None);
        }
        if !output.status.success() {
            return Err(git_error(
                "reading git config",
                command_failure_details(&output),
            ));
        }

        let mut value = output.stdout;
        match value.pop() {
            Some(0) => {}
            Some(_) | None => {
                return Err(git_error(
                    "reading git config",
                    format!("expected NUL-terminated value for {key}"),
                ));
            }
        }

        String::from_utf8(value)
            .map(|value| Some(value.trim().to_owned()))
            .map_err(|err| AppError::NonUtf8GitConfig {
                key,
                details: err.to_string(),
            })
    }

    pub(crate) fn historical_blobs(
        &self,
        path: &RepoPath,
        current: &BlobOid,
    ) -> Result<Vec<BlobOid>, AppError> {
        let output = self.git_output("walking path history", |command| {
            command
                .arg("log")
                .arg("--follow")
                .arg("--raw")
                .arg("-z")
                .arg("--no-abbrev")
                .arg("--format=format:%x00commit%x00%H%x00")
                .arg("--")
                .arg(path.to_os_path_buf());
        })?;
        parse_follow_raw_history(&output, current)
    }

    pub(crate) fn history_changes(&self) -> Result<Vec<HistoryChange>, AppError> {
        let output = self.git_output("walking repository history", |command| {
            command
                .arg("log")
                .arg("--raw")
                .arg("-z")
                .arg("--no-abbrev")
                .arg("--find-renames")
                .arg("--format=format:%x00commit%x00%H%x00");
        })?;
        parse_raw_history_changes(&output)
    }

    pub(crate) fn diff_empty_to_head(&self, file: &TrackedFile) -> Result<(), AppError> {
        let empty_tree = gix::ObjectId::empty_tree(self.repo.object_hash()).to_string();
        self.stream_git_diff(|command| {
            command
                .arg("diff")
                .arg(empty_tree)
                .arg("HEAD")
                .arg("--")
                .arg(file.path.to_os_path_buf());
        })
    }

    pub(crate) fn diff_empty_to_worktree(&self, file: &TrackedFile) -> Result<(), AppError> {
        let empty_tree = gix::ObjectId::empty_tree(self.repo.object_hash()).to_string();
        self.stream_git_diff(|command| {
            command
                .arg("diff")
                .arg(empty_tree)
                .arg("--")
                .arg(file.path.to_os_path_buf());
        })
    }

    pub(crate) fn diff_blobs(&self, baseline: &BlobOid, current: &BlobOid) -> Result<(), AppError> {
        self.stream_git_diff(|command| {
            command
                .arg("diff")
                .arg(baseline.to_string())
                .arg(current.to_string());
        })
    }

    pub(crate) fn diff_blob_to_worktree(
        &self,
        baseline: &BlobOid,
        file: &TrackedFile,
    ) -> Result<(), AppError> {
        let baseline_index = self.synthetic_index_with_blob(file, baseline)?;
        self.stream_git_diff(|command| {
            command
                .env("GIT_INDEX_FILE", baseline_index.path())
                .arg("diff")
                .arg("--")
                .arg(file.path.to_os_path_buf());
        })
    }

    fn synthetic_index_with_blob(
        &self,
        file: &TrackedFile,
        blob: &BlobOid,
    ) -> Result<TempIndex, AppError> {
        let index = TempIndex::new()?;
        let mut input = Vec::new();
        input.extend_from_slice(
            format!("{} blob {}\t", file.mode.as_tree_entry_mode(), blob).as_bytes(),
        );
        input.extend_from_slice(file.path.to_string().as_bytes());
        input.push(0);

        let mut command = self.git_command();
        let mut child = command
            .env("GIT_INDEX_FILE", index.path())
            .arg("update-index")
            .arg("-z")
            .arg("--index-info")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()?;
        let Some(mut stdin) = child.stdin.take() else {
            return Err(git_error(
                "creating synthetic diff index",
                "failed to open git update-index stdin",
            ));
        };
        stdin.write_all(&input)?;
        drop(stdin);

        let output = child.wait_with_output()?;
        stdout_from_success("creating synthetic diff index", output)?;
        Ok(index)
    }

    fn stream_git_diff(&self, configure: impl FnOnce(&mut Command)) -> Result<(), AppError> {
        let mut command = self.git_command();
        configure(&mut command);
        let status = command.status()?;
        if status.success() {
            Ok(())
        } else {
            Err(git_error("rendering git diff", status))
        }
    }

    fn git_output(
        &self,
        operation: &'static str,
        configure: impl FnOnce(&mut Command),
    ) -> Result<Vec<u8>, AppError> {
        let mut command = self.git_command();
        configure(&mut command);
        let output = command.output()?;
        stdout_from_success(operation, output)
    }

    fn git_command(&self) -> Command {
        let mut command = Command::new("git");
        command
            .current_dir(&self.root)
            .env("GIT_LITERAL_PATHSPECS", "1")
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_PREFIX");
        command
    }

    fn lookup_file_in_tree(
        tree: &gix::Tree<'_>,
        path: &RepoPath,
    ) -> Result<Option<TrackedFile>, AppError> {
        let entry = tree
            .lookup_entry_by_path(path.to_os_path_buf())
            .map_err(|err| git_error("looking up path in tree", err))?;
        match entry {
            Some(entry) => {
                let mode = FileMode::new(entry.mode());
                if mode.is_submodule() {
                    return Err(AppError::PathIsSubmodule(path.clone()));
                }
                if mode.is_reviewable_file() {
                    Ok(Some(TrackedFile {
                        path: path.clone(),
                        blob: BlobOid::new(entry.object_id()),
                        mode,
                    }))
                } else {
                    Ok(None)
                }
            }
            None => Ok(None),
        }
    }
}

#[derive(Debug)]
struct TempIndex {
    dir: PathBuf,
    path: PathBuf,
}

impl TempIndex {
    fn new() -> Result<Self, AppError> {
        let base = env::temp_dir();
        let pid = std::process::id();
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |duration| duration.as_nanos());

        for attempt in 0..1000 {
            let dir = base.join(format!("git-vet-index-{pid}-{nanos}-{attempt}"));
            match fs::create_dir(&dir) {
                Ok(()) => {
                    let path = dir.join("index");
                    return Ok(Self { dir, path });
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error.into()),
            }
        }

        Err(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            "could not create a unique temporary index directory",
        )
        .into())
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempIndex {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ReviewerConfigKey {
    UserName,
    UserEmail,
}

impl ReviewerConfigKey {
    const fn name(self) -> &'static str {
        match self {
            Self::UserName => "user.name",
            Self::UserEmail => "user.email",
        }
    }

    const fn missing_error(self) -> AppError {
        match self {
            Self::UserName => AppError::MissingUserName,
            Self::UserEmail => AppError::MissingUserEmail,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RawFileMode {
    Missing,
    RegularBlob,
    ExecutableBlob,
    Symlink,
    Tree,
    Submodule,
}

impl RawFileMode {
    fn parse(mode: &str) -> Result<Self, AppError> {
        match mode {
            "000000" => Ok(Self::Missing),
            "100644" => Ok(Self::RegularBlob),
            "100755" => Ok(Self::ExecutableBlob),
            "120000" => Ok(Self::Symlink),
            "040000" => Ok(Self::Tree),
            "160000" => Ok(Self::Submodule),
            _ => Err(git_error(
                "parsing git log raw mode",
                format!("unexpected file mode {mode:?}"),
            )),
        }
    }

    const fn is_reviewable(self) -> bool {
        matches!(
            self,
            Self::RegularBlob | Self::ExecutableBlob | Self::Symlink
        )
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RawStatus {
    Added,
    Copied,
    Deleted,
    Modified,
    Renamed,
    TypeChanged,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum HistoryChangeStatus {
    Added,
    Copied,
    Deleted,
    Modified,
    Renamed,
    TypeChanged,
}

impl HistoryChangeStatus {
    const fn from_raw(status: RawStatus) -> Self {
        match status {
            RawStatus::Added => Self::Added,
            RawStatus::Copied => Self::Copied,
            RawStatus::Deleted => Self::Deleted,
            RawStatus::Modified => Self::Modified,
            RawStatus::Renamed => Self::Renamed,
            RawStatus::TypeChanged => Self::TypeChanged,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct HistoryChange {
    pub status: HistoryChangeStatus,
    pub before_path: RepoPath,
    pub after_path: Option<RepoPath>,
    pub before_blob: Option<BlobOid>,
}

impl RawStatus {
    fn parse(status: &str) -> Result<Self, AppError> {
        let Some((&kind, score)) = status.as_bytes().split_first() else {
            return Err(git_error("parsing git log raw status", "empty status"));
        };
        if !score.iter().all(u8::is_ascii_digit) {
            return Err(git_error(
                "parsing git log raw status",
                format!("unexpected status {status:?}"),
            ));
        }
        match kind {
            b'A' => Ok(Self::Added),
            b'C' => Ok(Self::Copied),
            b'D' => Ok(Self::Deleted),
            b'M' => Ok(Self::Modified),
            b'R' => Ok(Self::Renamed),
            b'T' => Ok(Self::TypeChanged),
            b'U' | b'X' | b'B' => Err(git_error(
                "parsing git log raw status",
                format!("unsupported status {status:?}"),
            )),
            _ => Err(git_error(
                "parsing git log raw status",
                format!("unexpected status {status:?}"),
            )),
        }
    }

    const fn has_destination_path(self) -> bool {
        matches!(self, Self::Copied | Self::Renamed)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct RawHeader {
    source_mode: RawFileMode,
    source_oid: gix::ObjectId,
    status: RawStatus,
}

fn parse_follow_raw_history(output: &[u8], current: &BlobOid) -> Result<Vec<BlobOid>, AppError> {
    let fields = output.split(|byte| *byte == b'\0').collect::<Vec<_>>();
    let mut index = 0;
    let mut history = Vec::new();

    while index < fields.len() {
        let field = trim_leading_lf(fields[index]);
        index += 1;

        if field.is_empty() {
            continue;
        }

        if field == b"commit" {
            let commit_oid = next_field(&fields, &mut index, "parsing git log commit marker")?;
            parse_object_id("parsing git log commit object id", commit_oid)?;
            continue;
        }

        let header = parse_raw_header(field)?;
        let source_path = next_field(&fields, &mut index, "parsing git log raw source path")?;
        parse_raw_path(source_path)?;
        if header.status.has_destination_path() {
            let destination_path =
                next_field(&fields, &mut index, "parsing git log raw destination path")?;
            parse_raw_path(destination_path)?;
        }

        if header.source_mode.is_reviewable() {
            let blob = BlobOid::new(header.source_oid);
            if blob != *current && history.last() != Some(&blob) {
                history.push(blob);
            }
        }
    }

    Ok(history)
}

fn parse_raw_history_changes(output: &[u8]) -> Result<Vec<HistoryChange>, AppError> {
    let fields = output.split(|byte| *byte == b'\0').collect::<Vec<_>>();
    let mut index = 0;
    let mut changes = Vec::new();

    while index < fields.len() {
        let field = trim_leading_lf(fields[index]);
        index += 1;

        if field.is_empty() {
            continue;
        }

        if field == b"commit" {
            let commit_oid = next_field(&fields, &mut index, "parsing git log commit marker")?;
            parse_object_id("parsing git log commit object id", commit_oid)?;
            continue;
        }

        let header = parse_raw_header(field)?;
        let source_path = next_field(&fields, &mut index, "parsing git log raw source path")?;
        let source_path = parse_raw_path(source_path)?;
        let destination_path = if header.status.has_destination_path() {
            let destination_path =
                next_field(&fields, &mut index, "parsing git log raw destination path")?;
            Some(parse_raw_path(destination_path)?)
        } else {
            None
        };

        let after_path = match header.status {
            RawStatus::Deleted => None,
            RawStatus::Copied | RawStatus::Renamed => destination_path,
            RawStatus::Added | RawStatus::Modified | RawStatus::TypeChanged => {
                Some(source_path.clone())
            }
        };
        let before_blob = header
            .source_mode
            .is_reviewable()
            .then_some(BlobOid::new(header.source_oid));

        changes.push(HistoryChange {
            status: HistoryChangeStatus::from_raw(header.status),
            before_path: source_path,
            after_path,
            before_blob,
        });
    }

    Ok(changes)
}

fn parse_raw_header(field: &[u8]) -> Result<RawHeader, AppError> {
    let line =
        std::str::from_utf8(field).map_err(|err| git_error("decoding git log raw header", err))?;
    let fields = line.strip_prefix(':').ok_or_else(|| {
        git_error(
            "parsing git log raw header",
            format!("expected raw diff header, got {line:?}"),
        )
    })?;
    let mut fields = fields.split(' ');
    let source_mode = fields.next();
    let destination_mode = fields.next();
    let source_oid = fields.next();
    let destination_oid = fields.next();
    let status = fields.next();
    let extra = fields.next();

    match (
        source_mode,
        destination_mode,
        source_oid,
        destination_oid,
        status,
        extra,
    ) {
        (
            Some(source_mode),
            Some(destination_mode),
            Some(source_oid),
            Some(destination_oid),
            Some(status),
            None,
        ) => {
            let source_mode = RawFileMode::parse(source_mode)?;
            RawFileMode::parse(destination_mode)?;
            let source_oid = parse_object_id(
                "parsing git log raw source object id",
                source_oid.as_bytes(),
            )?;
            parse_object_id(
                "parsing git log raw destination object id",
                destination_oid.as_bytes(),
            )?;
            let status = RawStatus::parse(status)?;
            Ok(RawHeader {
                source_mode,
                source_oid,
                status,
            })
        }
        _ => Err(git_error(
            "parsing git log raw header",
            format!("expected five raw header fields, got {line:?}"),
        )),
    }
}

fn parse_raw_path(path: &[u8]) -> Result<RepoPath, AppError> {
    repo_path_from_bstr(path.as_bstr()).map_err(AppError::from)
}

fn parse_object_id(operation: &'static str, oid: &[u8]) -> Result<gix::ObjectId, AppError> {
    gix::ObjectId::from_hex(oid).map_err(|err| git_error(operation, err))
}

fn next_field<'a>(
    fields: &[&'a [u8]],
    index: &mut usize,
    operation: &'static str,
) -> Result<&'a [u8], AppError> {
    let field = fields
        .get(*index)
        .ok_or_else(|| git_error(operation, "unexpected end of git log output"))?;
    *index += 1;
    Ok(field)
}

const fn trim_leading_lf(mut field: &[u8]) -> &[u8] {
    while let Some((b'\n', rest)) = field.split_first() {
        field = rest;
    }
    field
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
