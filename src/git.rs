use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

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

    pub(crate) fn vetter(&self) -> Result<Vetter, AppError> {
        let config = self.repo.config_snapshot();
        let name = config
            .string("user.name")
            .ok_or(AppError::MissingUserName)?;
        let email = config
            .string("user.email")
            .ok_or(AppError::MissingUserEmail)?;
        let name = name
            .to_str()
            .map_err(|err| AppError::NonUtf8Path(err.to_string()))?
            .trim()
            .to_owned();
        let email = email
            .to_str()
            .map_err(|err| AppError::NonUtf8Path(err.to_string()))?
            .trim()
            .to_owned();
        match (name.is_empty(), email.is_empty()) {
            (true, _) => Err(AppError::MissingUserName),
            (_, true) => Err(AppError::MissingUserEmail),
            (false, false) => Ok(Vetter::new(name, email)),
        }
    }

    pub(crate) fn historical_blobs(
        &self,
        path: &RepoPath,
        current: &BlobOid,
    ) -> Result<Vec<BlobOid>, AppError> {
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
            if let Some(file) = Self::lookup_file_in_tree(&tree, &followed_path)? {
                if file.blob != *current && previous_blob != Some(file.blob) {
                    history.push(file.blob);
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
                    return repo_path_from_bstr(source_location.as_bstr())
                        .map(Some)
                        .map_err(Into::into);
                }
                _ => {}
            }
        }
        Ok(None)
    }
}
