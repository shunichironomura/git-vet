use std::fmt;

use thiserror::Error;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RemoteName(String);

impl RemoteName {
    pub(crate) fn new(input: &str, name_source: RemoteNameSource) -> Result<Self, RemoteError> {
        if input.is_empty() {
            Err(RemoteError::EmptyRemoteName { name_source })
        } else {
            Ok(Self(input.to_owned()))
        }
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for RemoteName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RemoteNameSource {
    Cli,
    Config,
    OriginFallback,
}

impl fmt::Display for RemoteNameSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Cli => f.write_str("--remote"),
            Self::Config => f.write_str("vet.syncRemote"),
            Self::OriginFallback => f.write_str("origin fallback"),
        }
    }
}

#[derive(Debug, Error, Eq, PartialEq)]
pub enum RemoteError {
    #[error("remote name from {name_source} must not be empty")]
    EmptyRemoteName { name_source: RemoteNameSource },
    #[error("no remote selected; pass --remote, set vet.syncRemote, or configure origin")]
    NoRemoteSelected,
    #[error("selected remote {remote:?} from {name_source} is not usable: {details}")]
    UnusableRemote {
        remote: String,
        name_source: RemoteNameSource,
        details: String,
    },
}
