use std::env;
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};

use thiserror::Error;

const MAN_PAGE: &str = include_str!("../man/git-vet.1");
const MAN_PAGE_FILE_NAME: &str = "git-vet.1";

#[derive(Debug, Error)]
pub(crate) enum ManPageInstallError {
    #[error(
        "cannot determine the user man directory because neither XDG_DATA_HOME nor HOME is set; pass --man-dir explicitly"
    )]
    MissingHomeDirectory,
    #[error("XDG_DATA_HOME must be an absolute path, got {0:?}; pass --man-dir explicitly")]
    RelativeXdgDataHome(PathBuf),
    #[error("failed to create man directory {path}: {source}")]
    CreateDirectory {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("failed to install man page at {path}: {source}")]
    Write {
        path: PathBuf,
        source: std::io::Error,
    },
}

pub(crate) fn install(man_dir: Option<&Path>) -> Result<PathBuf, ManPageInstallError> {
    let man_dir = match man_dir {
        Some(path) => path.to_owned(),
        None => default_man_dir(
            env::var_os("XDG_DATA_HOME").as_deref(),
            env::var_os("HOME").as_deref(),
        )?,
    };

    fs::create_dir_all(&man_dir).map_err(|source| ManPageInstallError::CreateDirectory {
        path: man_dir.clone(),
        source,
    })?;
    let destination = man_dir.join(MAN_PAGE_FILE_NAME);
    fs::write(&destination, MAN_PAGE).map_err(|source| ManPageInstallError::Write {
        path: destination.clone(),
        source,
    })?;
    Ok(destination)
}

fn default_man_dir(
    xdg_data_home: Option<&OsStr>,
    home: Option<&OsStr>,
) -> Result<PathBuf, ManPageInstallError> {
    xdg_data_home.map_or_else(
        || {
            home.map(PathBuf::from)
                .map(|path| path.join(".local/share/man/man1"))
                .ok_or(ManPageInstallError::MissingHomeDirectory)
        },
        |value| {
            let path = PathBuf::from(value);
            if path.is_absolute() {
                Ok(path.join("man/man1"))
            } else {
                Err(ManPageInstallError::RelativeXdgDataHome(path))
            }
        },
    )
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;

    use super::{ManPageInstallError, default_man_dir};

    #[test]
    fn xdg_data_home_selects_its_man1_directory() {
        let data_home = std::env::temp_dir().join("git-vet-data");
        assert_eq!(
            default_man_dir(Some(data_home.as_os_str()), None)
                .expect("absolute XDG_DATA_HOME is valid"),
            data_home.join("man/man1")
        );
    }

    #[test]
    fn home_selects_the_conventional_user_man1_directory() {
        let home = std::env::temp_dir().join("git-vet-home");
        assert_eq!(
            default_man_dir(None, Some(home.as_os_str())).expect("HOME selects a directory"),
            home.join(".local/share/man/man1")
        );
    }

    #[test]
    fn relative_xdg_data_home_is_rejected() {
        assert!(matches!(
            default_man_dir(Some(OsStr::new("relative")), Some(OsStr::new("/home/user"))),
            Err(ManPageInstallError::RelativeXdgDataHome(path)) if path == *"relative"
        ));
    }
}
