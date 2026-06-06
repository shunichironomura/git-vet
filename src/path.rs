use std::fmt;
use std::path::{Component, Path, PathBuf};

use gix::bstr::ByteSlice;
use thiserror::Error;

#[derive(Clone, Debug, Eq, PartialEq, Hash, Ord, PartialOrd, serde::Serialize)]
#[serde(transparent)]
pub struct RepoPath(String);

impl RepoPath {
    pub(crate) fn from_git_path(path: &str) -> Result<Self, PathError> {
        if path.is_empty() {
            return Err(PathError::EmptyPath);
        }
        Ok(Self(path.to_owned()))
    }

    pub(crate) fn as_bstr(&self) -> &gix::bstr::BStr {
        self.0.as_bytes().as_bstr()
    }

    pub(crate) fn to_path_buf(&self) -> PathBuf {
        self.0.split('/').collect()
    }
}

impl fmt::Display for RepoPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

#[derive(Debug, Error)]
pub(crate) enum PathError {
    #[error("path is not valid UTF-8: {0}")]
    NonUtf8Path(String),
    #[error("path escapes the repository root: {0}")]
    PathOutsideRepo(String),
    #[error("empty paths are not valid tracked files")]
    EmptyPath,
}

pub(crate) fn prefix_from_cwd(root: &Path, cwd: &Path) -> Result<PathBuf, PathError> {
    let root = normalize_lexically(root);
    let cwd = normalize_lexically(cwd);
    cwd.strip_prefix(&root)
        .map(Path::to_path_buf)
        .map_err(|_| PathError::PathOutsideRepo(cwd.display().to_string()))
}

pub(crate) fn repo_path_from_bstr(path: &gix::bstr::BStr) -> Result<RepoPath, PathError> {
    let path = path
        .to_str()
        .map_err(|err| PathError::NonUtf8Path(err.to_string()))?;
    RepoPath::from_git_path(path)
}

pub(crate) fn normalize_lexically(path: &Path) -> PathBuf {
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

pub(crate) fn repo_path_from_relative(path: &Path) -> Result<String, PathError> {
    let parts = path
        .components()
        .map(|component| match component {
            Component::Normal(part) => part
                .to_str()
                .map(ToOwned::to_owned)
                .ok_or_else(|| PathError::NonUtf8Path(path.display().to_string())),
            Component::CurDir => Ok(String::new()),
            Component::Prefix(_) | Component::RootDir | Component::ParentDir => {
                Err(PathError::PathOutsideRepo(path.display().to_string()))
            }
        })
        .filter(|part| !matches!(part, Ok(value) if value.is_empty()))
        .collect::<Result<Vec<_>, _>>()?;

    match parts.is_empty() {
        true => Err(PathError::EmptyPath),
        false => Ok(parts.join("/")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repo_path_from_relative_rejects_empty_paths() {
        assert!(matches!(
            repo_path_from_relative(Path::new("")),
            Err(PathError::EmptyPath)
        ));
    }
}
