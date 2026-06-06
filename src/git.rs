use std::collections::BTreeSet;
use std::env;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use gix::bstr::ByteSlice;

use crate::error::{AppError, git_error};
use crate::git_types::{BlobOid, CommitOid, FileMode, TrackedFile};
use crate::path::{
    RepoPath, normalize_lexically, prefix_from_cwd, repo_path_from_bstr, repo_path_from_relative,
};
use crate::review::Vetter;

pub struct Git {
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
        let normalized = normalize_lexically(&joined);
        let root = normalize_lexically(&self.root);
        let relative = normalized
            .strip_prefix(&root)
            .map_err(|_| AppError::PathOutsideRepo(input.display().to_string()))?;
        let path = repo_path_from_relative(relative)?;
        RepoPath::from_git_path(&path).map_err(AppError::from)
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
            .arg(path.to_path_buf())
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

    fn required_config_value(&self, key: ReviewerConfigKey) -> Result<String, AppError> {
        let output = self
            .git_command()
            .arg("config")
            .arg("get")
            .arg("--null")
            .arg(key.name())
            .output()?;

        if output.status.code() == Some(1) {
            return Err(key.missing_error());
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
                    format!("expected NUL-terminated value for {}", key.name()),
                ));
            }
        }

        let value = String::from_utf8(value).map_err(|err| AppError::NonUtf8GitConfig {
            key: key.name(),
            details: err.to_string(),
        })?;
        let value = value.trim().to_owned();
        if value.is_empty() {
            Err(key.missing_error())
        } else {
            Ok(value)
        }
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
                .arg(path.to_path_buf());
        })?;
        parse_follow_raw_history(&output, current)
    }

    pub(crate) fn diff_empty_to_head(&self, file: &TrackedFile) -> Result<(), AppError> {
        let empty_tree = gix::ObjectId::empty_tree(self.repo.object_hash()).to_string();
        self.stream_git_diff(|command| {
            command
                .arg("diff")
                .arg(empty_tree)
                .arg("HEAD")
                .arg("--")
                .arg(file.path.to_path_buf());
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
            .lookup_entry_by_path(path.to_path_buf())
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
                    }))
                } else {
                    Ok(None)
                }
            }
            None => Ok(None),
        }
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

fn parse_raw_path(path: &[u8]) -> Result<(), AppError> {
    repo_path_from_bstr(path.as_bstr())?;
    Ok(())
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
