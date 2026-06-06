use std::env;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use gix::bstr::ByteSlice;
use gix::diff::blob::platform::prepare_diff::Operation;
use gix::diff::blob::unified_diff::{ConsumeBinaryHunk, ContextSize};
use gix::diff::blob::{ResourceKind, UnifiedDiff};
use gix::objs::tree::EntryKind;

use crate::error::{AppError, git_error};
use crate::git_types::{BlobOid, CommitOid, FileMode, HistoricalBlob, TrackedFile};
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
            if let Some(file) = Self::lookup_file_in_tree(&tree, &followed_path)? {
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

    pub(crate) fn diff_empty_to_head(&self, file: &TrackedFile) -> Result<String, AppError> {
        self.render_blob_diff(None, file)
    }

    pub(crate) fn diff_blobs_with_path_label(
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
        let old_label =
            baseline.map_or_else(|| "/dev/null".to_owned(), |_| format!("a/{}", current.path));
        let new_label = format!("b/{}", current.path);
        let old_blob = baseline.map(|baseline| baseline.blob);
        let old_mode = baseline.map(|baseline| baseline.mode);
        let old_id = old_blob.map_or_else(
            || gix::ObjectId::null(self.repo.object_hash()),
            |oid| oid.as_object_id(),
        );
        let old_kind = old_mode.map_or(EntryKind::Blob, FileMode::kind);
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

        let mut output = self.diff_header(baseline, current)?;

        let prepared = cache
            .prepare_diff()
            .map_err(|err| git_error("preparing blob diff", err))?;
        match prepared.operation {
            Operation::InternalDiff { algorithm } => {
                writeln!(&mut output, "--- {old_label}\n+++ {new_label}")
                    .map_err(|err| git_error("rendering diff header", err))?;
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
                writeln!(
                    &mut output,
                    "Binary files {old_label} and {new_label} differ"
                )
                .map_err(|err| git_error("rendering binary diff", err))?;
            }
            Operation::ExternalCommand { .. } => {
                return Err(git_error(
                    "rendering blob diff",
                    "external diffs are disabled but gix selected an external command",
                ));
            }
        }

        Ok(output)
    }

    fn diff_header(
        &self,
        baseline: Option<&HistoricalBlob>,
        current: &TrackedFile,
    ) -> Result<String, AppError> {
        let mut output = String::new();
        writeln!(
            &mut output,
            "diff --git a/{path} b/{path}",
            path = current.path
        )
        .map_err(|err| git_error("rendering diff header", err))?;
        match baseline {
            Some(baseline) if baseline.mode != current.mode => {
                writeln!(&mut output, "old mode {}", baseline.mode.as_octal())
                    .map_err(|err| git_error("rendering diff header", err))?;
                writeln!(&mut output, "new mode {}", current.mode.as_octal())
                    .map_err(|err| git_error("rendering diff header", err))?;
                writeln!(
                    &mut output,
                    "index {}..{}",
                    baseline.blob.short(),
                    current.blob.short()
                )
                .map_err(|err| git_error("rendering diff header", err))?;
            }
            Some(baseline) => {
                writeln!(
                    &mut output,
                    "index {}..{} {}",
                    baseline.blob.short(),
                    current.blob.short(),
                    current.mode.as_octal()
                )
                .map_err(|err| git_error("rendering diff header", err))?;
            }
            None => {
                writeln!(&mut output, "new file mode {}", current.mode.as_octal())
                    .map_err(|err| git_error("rendering diff header", err))?;
                writeln!(
                    &mut output,
                    "index {}..{}",
                    zero_oid(self.repo.object_hash()),
                    current.blob.short()
                )
                .map_err(|err| git_error("rendering diff header", err))?;
            }
        }
        Ok(output)
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
                        mode,
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

fn zero_oid(kind: gix::hash::Kind) -> String {
    "0".repeat(kind.len_in_hex())
}
