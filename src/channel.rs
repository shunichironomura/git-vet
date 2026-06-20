use std::fmt;

use serde::{Serialize, Serializer};
use thiserror::Error;

const NOTES_REF_PREFIX: &str = "refs/notes/vet";
pub(crate) const DEFAULT_REVIEW_CHANNEL: &str = "default";

/// A review channel whose notes ref has been validated by `git check-ref-format`
/// without normalization.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ReviewChannel {
    name: String,
    notes_ref: NotesRef,
}

impl ReviewChannel {
    pub(crate) fn as_str(&self) -> &str {
        &self.name
    }

    pub(crate) const fn notes_ref(&self) -> &NotesRef {
        &self.notes_ref
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ReviewChannelCandidate {
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
        if input.contains('/') {
            return Err(ChannelError {
                channel: input.to_owned(),
                details: "channel name must not contain '/'".to_owned(),
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
}

pub(crate) trait ValidatedReviewChannelCandidate {
    fn into_candidate(self) -> ReviewChannelCandidate;
}

impl ReviewChannel {
    pub(crate) fn from_validated_candidate(
        candidate: impl ValidatedReviewChannelCandidate,
    ) -> Self {
        let candidate = candidate.into_candidate();
        Self {
            name: candidate.name,
            notes_ref: NotesRef {
                name: candidate.notes_ref_name,
            },
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
pub(crate) struct NotesRef {
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
