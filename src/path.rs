use std::fmt;
use std::path::{Component, Path, PathBuf};

use gix::bstr::ByteSlice;
use serde::{Serialize, Serializer};
use thiserror::Error;

#[derive(Clone, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub struct RepoPath {
    components: Vec<RepoPathComponent>,
}

impl RepoPath {
    pub(crate) fn from_git_path(path: &str) -> Result<Self, PathError> {
        let components = parse_git_path_components(path)?;
        Ok(Self { components })
    }

    pub(crate) fn to_path_buf(&self) -> PathBuf {
        self.components
            .iter()
            .map(RepoPathComponent::as_str)
            .collect()
    }

    fn to_git_path(&self) -> String {
        self.components
            .iter()
            .map(RepoPathComponent::as_str)
            .collect::<Vec<_>>()
            .join("/")
    }
}

impl fmt::Display for RepoPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_git_path())
    }
}

impl Serialize for RepoPath {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.to_git_path())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
struct RepoPathComponent(String);

impl RepoPathComponent {
    fn parse(path: &str, component: &str) -> Result<Self, PathError> {
        match component {
            "" => Err(PathError::InvalidRepoPath {
                path: path.to_owned(),
                details: "path contains an empty component",
            }),
            "." => Err(PathError::InvalidRepoPath {
                path: path.to_owned(),
                details: "path contains a current-directory component",
            }),
            ".." => Err(PathError::PathOutsideRepo(path.to_owned())),
            value if value.contains('\0') => Err(PathError::InvalidRepoPath {
                path: path.to_owned(),
                details: "path contains a NUL byte",
            }),
            value => Ok(Self(value.to_owned())),
        }
    }

    fn from_os_component(path: &Path, component: &std::ffi::OsStr) -> Result<Self, PathError> {
        let component = component
            .to_str()
            .ok_or_else(|| PathError::NonUtf8Path(path.display().to_string()))?;
        Self::parse(&path.display().to_string(), component)
    }

    fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Error)]
pub enum PathError {
    #[error("path is not valid UTF-8: {0}")]
    NonUtf8Path(String),
    #[error("path escapes the repository root: {0}")]
    PathOutsideRepo(String),
    #[error("empty paths are not valid tracked files")]
    EmptyPath,
    #[error("repo path is invalid: {path}: {details}")]
    InvalidRepoPath { path: String, details: &'static str },
}

pub fn prefix_from_cwd(root: &Path, cwd: &Path) -> Result<PathBuf, PathError> {
    let root = normalize_lexically(root);
    let cwd = normalize_lexically(cwd);
    cwd.strip_prefix(&root)
        .map(Path::to_path_buf)
        .map_err(|_| PathError::PathOutsideRepo(cwd.display().to_string()))
}

pub fn repo_path_from_bstr(path: &gix::bstr::BStr) -> Result<RepoPath, PathError> {
    let path = path
        .to_str()
        .map_err(|err| PathError::NonUtf8Path(err.to_string()))?;
    RepoPath::from_git_path(path)
}

pub fn normalize_lexically(path: &Path) -> PathBuf {
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

pub fn repo_path_from_relative(path: &Path) -> Result<RepoPath, PathError> {
    let components = path
        .components()
        .filter_map(|component| match component {
            Component::Normal(part) => Some(RepoPathComponent::from_os_component(path, part)),
            Component::CurDir => None,
            Component::Prefix(_) | Component::RootDir | Component::ParentDir => {
                Some(Err(PathError::PathOutsideRepo(path.display().to_string())))
            }
        })
        .collect::<Result<Vec<_>, _>>()?;

    if components.is_empty() {
        Err(PathError::EmptyPath)
    } else {
        Ok(RepoPath { components })
    }
}

fn parse_git_path_components(path: &str) -> Result<Vec<RepoPathComponent>, PathError> {
    if path.is_empty() {
        return Err(PathError::EmptyPath);
    }

    path.split('/')
        .map(|component| RepoPathComponent::parse(path, component))
        .collect()
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

    #[test]
    fn repo_path_from_git_path_rejects_structurally_invalid_paths() {
        for path in [
            "/src/lib.rs",
            "src//lib.rs",
            "src/./lib.rs",
            "src/../lib.rs",
            "src/lib.rs/",
        ] {
            assert!(
                RepoPath::from_git_path(path).is_err(),
                "expected {path:?} to be rejected"
            );
        }
    }

    #[test]
    fn repo_path_round_trips_git_path_and_os_path() -> Result<(), PathError> {
        let path = RepoPath::from_git_path("src/lib.rs")?;

        assert_eq!(path.to_string(), "src/lib.rs");
        assert_eq!(path.to_path_buf(), PathBuf::from("src").join("lib.rs"));
        Ok(())
    }
}
