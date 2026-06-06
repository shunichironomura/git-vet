use std::fmt;

use serde::{Serialize, Serializer};
use thiserror::Error;

const NOTES_REF_PREFIX: &str = "refs/notes/vet";
pub const DEFAULT_REVIEW_CHANNEL: &str = "default";

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReviewChannel {
    name: String,
    notes_ref: NotesRef,
}

impl ReviewChannel {
    pub(crate) const fn notes_ref(&self) -> &NotesRef {
        &self.notes_ref
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReviewChannelCandidate {
    name: String,
    notes_ref_name: String,
}

impl ReviewChannelCandidate {
    pub(crate) fn new(input: &str) -> Result<Self, ChannelError> {
        if input.is_empty() {
            return Err(ChannelError {
                channel: input.to_owned(),
                details: "channel name must not be empty".to_owned(),
            });
        }

        Ok(Self {
            name: input.to_owned(),
            notes_ref_name: format!("{NOTES_REF_PREFIX}/{input}"),
        })
    }

    pub(crate) fn notes_ref_name(&self) -> &str {
        &self.notes_ref_name
    }

    /// Convert only after the caller has validated `notes_ref_name()` with
    /// `git check-ref-format` without normalization.
    pub(crate) fn into_channel_after_git_check_ref_format(self) -> ReviewChannel {
        ReviewChannel {
            name: self.name,
            notes_ref: NotesRef {
                name: self.notes_ref_name,
            },
        }
    }

    pub(crate) fn channel_error(&self, details: String) -> ChannelError {
        ChannelError {
            channel: self.name.clone(),
            details,
        }
    }
}

impl Serialize for ReviewChannel {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.name)
    }
}

impl fmt::Display for ReviewChannel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.name.fmt(f)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NotesRef {
    name: String,
}

impl NotesRef {
    pub(crate) fn as_str(&self) -> &str {
        &self.name
    }
}

impl fmt::Display for NotesRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.name.fmt(f)
    }
}

#[derive(Clone, Debug, Eq, Error, PartialEq)]
#[error("invalid review channel {channel:?}: {details}")]
pub struct ChannelError {
    pub(crate) channel: String,
    pub(crate) details: String,
}
