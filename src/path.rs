use std::fmt;
use std::path::{Component, Path, PathBuf};

use gix::bstr::ByteSlice;
use serde::{Serialize, Serializer};
use thiserror::Error;

/// A non-empty UTF-8 path to a tracked file relative to the repository root.
///
/// `RepoPath` is normalized into validated components: it never contains an
/// absolute prefix, empty component, `.`, `..`, or NUL byte. It renders and
/// serializes as a Git path, using `/` as the component separator.
#[derive(Clone, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub struct RepoPath {
    components: Vec<RepoPathComponent>,
}

impl RepoPath {
    pub(crate) fn from_git_path(path: &str) -> Result<Self, PathError> {
        let components = parse_git_path_components(path)?;
        Ok(Self { components })
    }

    pub(crate) fn to_os_path_buf(&self) -> PathBuf {
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
    fn parse(component: &str) -> Result<Self, RepoPathComponentError> {
        match component {
            "" => Err(RepoPathComponentError::Empty),
            "." => Err(RepoPathComponentError::CurrentDir),
            ".." => Err(RepoPathComponentError::ParentDir),
            value if value.contains('\0') => Err(RepoPathComponentError::NulByte),
            value => Ok(Self(value.to_owned())),
        }
    }

    fn from_os_component(component: &std::ffi::OsStr) -> Result<Self, RepoPathComponentError> {
        let component = component.to_str().ok_or(RepoPathComponentError::NonUtf8)?;
        Self::parse(component)
    }

    fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RepoPathComponentError {
    NonUtf8,
    Empty,
    CurrentDir,
    ParentDir,
    NulByte,
}

impl RepoPathComponentError {
    const fn into_path_error(self) -> PathError {
        match self {
            Self::NonUtf8 => PathError::NonUtf8,
            Self::Empty => PathError::EmptyComponent,
            Self::CurrentDir => PathError::CurrentDirComponent,
            Self::ParentDir => PathError::OutsideRepo,
            Self::NulByte => PathError::NulByte,
        }
    }
}

#[derive(Debug, Error)]
pub(crate) enum PathError {
    #[error("path is not valid UTF-8")]
    NonUtf8,
    #[error("path escapes the repository root")]
    OutsideRepo,
    #[error("path must be absolute")]
    NonAbsolute,
    #[error("empty paths are not valid tracked files")]
    EmptyPath,
    #[error("repo path contains an empty component")]
    EmptyComponent,
    #[error("repo path contains a current-directory component")]
    CurrentDirComponent,
    #[error("repo path contains a NUL byte")]
    NulByte,
}

pub(crate) fn prefix_from_cwd(root: &Path, cwd: &Path) -> Result<PathBuf, PathError> {
    let root = normalize_absolute_lexically(root)?;
    let cwd = normalize_absolute_lexically(cwd)?;
    cwd.strip_prefix(&root)
        .map(Path::to_path_buf)
        .map_err(|_| PathError::OutsideRepo)
}

pub(crate) fn repo_path_from_bstr(path: &gix::bstr::BStr) -> Result<RepoPath, PathError> {
    let path = path.to_str().map_err(|_| PathError::NonUtf8)?;
    RepoPath::from_git_path(path)
}

pub(crate) fn normalize_absolute_lexically(path: &Path) -> Result<PathBuf, PathError> {
    if !path.is_absolute() {
        return Err(PathError::NonAbsolute);
    }

    Ok(path
        .components()
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
        }))
}

pub(crate) fn repo_path_from_relative(path: &Path) -> Result<RepoPath, PathError> {
    let components = path
        .components()
        .filter_map(|component| match component {
            Component::Normal(part) => Some(
                RepoPathComponent::from_os_component(part)
                    .map_err(RepoPathComponentError::into_path_error),
            ),
            Component::CurDir => None,
            Component::Prefix(_) | Component::RootDir | Component::ParentDir => {
                Some(Err(PathError::OutsideRepo))
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
        .map(|component| {
            RepoPathComponent::parse(component).map_err(RepoPathComponentError::into_path_error)
        })
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
    fn repo_path_from_git_path_classifies_structural_errors() {
        assert!(matches!(
            RepoPath::from_git_path(""),
            Err(PathError::EmptyPath)
        ));
        for path in ["/src/lib.rs", "src//lib.rs", "src/lib.rs/"] {
            assert!(matches!(
                RepoPath::from_git_path(path),
                Err(PathError::EmptyComponent),
            ));
        }
        assert!(matches!(
            RepoPath::from_git_path("src/./lib.rs"),
            Err(PathError::CurrentDirComponent)
        ));
        assert!(matches!(
            RepoPath::from_git_path("src/../lib.rs"),
            Err(PathError::OutsideRepo)
        ));
        assert!(matches!(
            RepoPath::from_git_path("src/\0/lib.rs"),
            Err(PathError::NulByte)
        ));
    }

    #[test]
    fn repo_path_round_trips_git_path_and_os_path() -> Result<(), PathError> {
        let path = RepoPath::from_git_path("src/lib.rs")?;

        assert_eq!(path.to_string(), "src/lib.rs");
        assert_eq!(path.to_os_path_buf(), PathBuf::from("src").join("lib.rs"));
        Ok(())
    }

    #[test]
    fn normalize_absolute_lexically_rejects_relative_paths() {
        assert!(matches!(
            normalize_absolute_lexically(Path::new("src/../lib.rs")),
            Err(PathError::NonAbsolute)
        ));
    }
}
