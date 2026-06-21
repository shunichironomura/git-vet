use std::fmt;
use std::marker::PhantomData;

use serde::{Serialize, Serializer};
use thiserror::Error;

use crate::git_ref_format::StrictGitRefName;

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
pub(crate) enum Unvalidated {}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Validated {}

pub(crate) type ValidatedReviewChannelCandidate = ReviewChannelCandidate<Validated>;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ReviewChannelCandidate<State = Unvalidated> {
    name: String,
    notes_ref_name: String,
    _state: PhantomData<State>,
}

impl ReviewChannelCandidate<Unvalidated> {
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
            _state: PhantomData,
        })
    }

    pub(crate) fn into_validated(
        self,
        checked_ref: StrictGitRefName,
    ) -> Result<ValidatedReviewChannelCandidate, ChannelError> {
        let Self {
            name,
            notes_ref_name,
            _state,
        } = self;

        let checked_ref_name = checked_ref.into_string();
        if checked_ref_name != notes_ref_name {
            return Err(ChannelError {
                channel: name,
                details: format!(
                    "validated Git ref {checked_ref_name:?} does not match channel notes ref {notes_ref_name:?}"
                ),
            });
        }

        Ok(ReviewChannelCandidate {
            name,
            notes_ref_name,
            _state: PhantomData,
        })
    }
}

impl<State> ReviewChannelCandidate<State> {
    pub(crate) fn notes_ref_name(&self) -> &str {
        &self.notes_ref_name
    }
}

impl ReviewChannel {
    pub(crate) fn from_validated_candidate(candidate: ValidatedReviewChannelCandidate) -> Self {
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
