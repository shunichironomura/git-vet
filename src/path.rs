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

/// A repository-relative status scope supplied by the user.
///
/// Unlike `RepoPath`, this may be empty, representing the repository root.
/// It matches tracked files whose paths are exactly equal to the scope or live
/// below it.
#[derive(Clone, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub struct RepoPathScope {
    components: Vec<RepoPathComponent>,
}

impl RepoPath {
    /// Parses a Git path into a repository-relative path.
    ///
    /// Git paths always use `/` as the separator and must already be relative
    /// to the repository root.
    pub(crate) fn from_git_path(path: &str) -> Result<Self, PathError> {
        let components = parse_git_path_components(path)?;
        Ok(Self { components })
    }

    /// Converts this repository path into an OS-native relative path.
    pub(crate) fn to_os_path_buf(&self) -> PathBuf {
        self.components
            .iter()
            .map(RepoPathComponent::as_str)
            .collect()
    }

    /// Renders this repository path using Git's `/` component separator.
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

impl RepoPathScope {
    /// Returns true when this scope contains the provided repository file path.
    pub(crate) fn contains_file(&self, path: &RepoPath) -> bool {
        self.components.len() <= path.components.len()
            && self
                .components
                .iter()
                .zip(path.components.iter())
                .all(|(scope, path)| scope == path)
    }

    fn to_git_path(&self) -> String {
        if self.components.is_empty() {
            ".".to_owned()
        } else {
            self.components
                .iter()
                .map(RepoPathComponent::as_str)
                .collect::<Vec<_>>()
                .join("/")
        }
    }
}

impl fmt::Display for RepoPathScope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_git_path())
    }
}

/// A single validated UTF-8 repository path component.
#[derive(Clone, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
struct RepoPathComponent(String);

impl RepoPathComponent {
    /// Parses one textual path component and rejects structural components.
    fn parse(component: &str) -> Result<Self, RepoPathComponentError> {
        match component {
            "" => Err(RepoPathComponentError::Empty),
            "." => Err(RepoPathComponentError::CurrentDir),
            ".." => Err(RepoPathComponentError::ParentDir),
            value if value.contains('\0') => Err(RepoPathComponentError::NulByte),
            value => Ok(Self(value.to_owned())),
        }
    }

    /// Converts one OS path component into a validated repository component.
    fn from_os_component(component: &std::ffi::OsStr) -> Result<Self, RepoPathComponentError> {
        let component = component.to_str().ok_or(RepoPathComponentError::NonUtf8)?;
        Self::parse(component)
    }

    /// Returns the component as a UTF-8 string slice.
    fn as_str(&self) -> &str {
        &self.0
    }
}

/// Validation errors for a single repository path component.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RepoPathComponentError {
    /// The OS component is not valid UTF-8.
    NonUtf8,
    /// The component is empty.
    Empty,
    /// The component is `.`.
    CurrentDir,
    /// The component is `..`.
    ParentDir,
    /// The component contains a NUL byte.
    NulByte,
}

impl RepoPathComponentError {
    /// Promotes a component-level validation error to a path-level error.
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

/// Errors produced while converting external path representations into typed paths.
#[derive(Debug, Error)]
pub(crate) enum PathError {
    /// The path or component is not valid UTF-8.
    #[error("path is not valid UTF-8")]
    NonUtf8,
    /// The path escapes, or is not relative to, the repository root.
    #[error("path escapes the repository root")]
    OutsideRepo,
    /// An absolute path was required but a relative path was provided.
    #[error("path must be absolute")]
    NonAbsolute,
    /// The path has no file components.
    #[error("empty paths are not valid tracked files")]
    EmptyPath,
    /// A Git path contains an empty component.
    #[error("repo path contains an empty component")]
    EmptyComponent,
    /// A Git path contains a `.` component.
    #[error("repo path contains a current-directory component")]
    CurrentDirComponent,
    /// The path contains a NUL byte.
    #[error("repo path contains a NUL byte")]
    NulByte,
}

/// Computes the current working directory prefix relative to the repository root.
///
/// Both inputs must be absolute paths. `.` components are removed and `..`
/// components are applied lexically before checking containment.
pub(crate) fn prefix_from_cwd(root: &Path, cwd: &Path) -> Result<PathBuf, PathError> {
    let root = normalize_absolute_lexically(root)?;
    let cwd = normalize_absolute_lexically(cwd)?;
    cwd.strip_prefix(&root)
        .map(Path::to_path_buf)
        .map_err(|_| PathError::OutsideRepo)
}

/// Converts a Git byte-string path into a typed repository path.
pub(crate) fn repo_path_from_bstr(path: &gix::bstr::BStr) -> Result<RepoPath, PathError> {
    let path = path.to_str().map_err(|_| PathError::NonUtf8)?;
    RepoPath::from_git_path(path)
}

/// Lexically normalizes an absolute path without touching the filesystem.
///
/// This removes `.` components and applies `..` components with `PathBuf::pop`.
/// It does not resolve symlinks or require the path to exist.
///
/// TODO: Replace this implementation with `Path::normalize_lexically()` once
/// that standard-library API is stabilized.
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

/// Converts an OS-native relative path into a typed repository path.
///
/// The input must be relative to the repository root and resolve to a non-empty
/// tracked-file path. Parent-directory, root, prefix, non-UTF-8, and NUL-byte
/// components are rejected.
pub(crate) fn repo_path_from_relative(path: &Path) -> Result<RepoPath, PathError> {
    let components = repo_path_components_from_relative(path)?;

    if components.is_empty() {
        Err(PathError::EmptyPath)
    } else {
        Ok(RepoPath { components })
    }
}

/// Converts an OS-native relative path into a repository-relative status scope.
pub(crate) fn repo_path_scope_from_relative(path: &Path) -> Result<RepoPathScope, PathError> {
    repo_path_components_from_relative(path).map(|components| RepoPathScope { components })
}

fn repo_path_components_from_relative(path: &Path) -> Result<Vec<RepoPathComponent>, PathError> {
    if !path.is_relative() {
        return Err(PathError::OutsideRepo);
    }

    path.components()
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
        .collect()
}

/// Splits and validates a Git path into repository path components.
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
    fn repo_path_from_relative_rejects_absolute_paths() {
        assert!(matches!(
            repo_path_from_relative(Path::new("/src/lib.rs")),
            Err(PathError::OutsideRepo)
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
