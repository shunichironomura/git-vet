use std::fmt;

use gix::bstr::ByteSlice;
use serde::{Serialize, Serializer};
use thiserror::Error;

const NOTES_REF_PREFIX: &str = "refs/notes/vet";
pub(crate) const DEFAULT_REVIEW_CHANNEL: &str = "default";

/// A review channel whose notes ref has been validated as a Git ref name.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ReviewChannel {
    name: String,
    notes_ref: NotesRef,
}

impl ReviewChannel {
    pub(crate) fn new(input: &str) -> Result<Self, ChannelError> {
        if input.is_empty() {
            return Err(ChannelError::Empty);
        }
        if input.contains('/') {
            return Err(ChannelError::ContainsSlash);
        }

        let notes_ref_name = format!("{NOTES_REF_PREFIX}/{input}");
        validate_notes_ref(&notes_ref_name)?;

        Ok(Self {
            name: input.to_owned(),
            notes_ref: NotesRef {
                name: notes_ref_name,
            },
        })
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.name
    }

    pub(crate) const fn notes_ref(&self) -> &NotesRef {
        &self.notes_ref
    }
}

fn validate_notes_ref(notes_ref_name: &str) -> Result<(), ChannelError> {
    match gix::validate::reference::name(notes_ref_name.as_bytes().as_bstr()) {
        Ok(_) => Ok(()),
        Err(error) => Err(ChannelError::InvalidNotesRef {
            notes_ref: notes_ref_name.to_owned(),
            validation_error: error.to_string(),
        }),
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ChannelTransferKind {
    Copy,
    Move,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ChannelTransfer {
    kind: ChannelTransferKind,
    source: ReviewChannel,
    destination: ReviewChannel,
}

impl ChannelTransfer {
    pub(crate) fn new(
        kind: ChannelTransferKind,
        source: ReviewChannel,
        destination: ReviewChannel,
    ) -> Result<Self, ChannelTransferError> {
        if source == destination {
            return Err(ChannelTransferError::SameChannel);
        }

        Ok(Self {
            kind,
            source,
            destination,
        })
    }

    pub(crate) const fn kind(&self) -> ChannelTransferKind {
        self.kind
    }

    pub(crate) const fn source(&self) -> &ReviewChannel {
        &self.source
    }

    pub(crate) const fn destination(&self) -> &ReviewChannel {
        &self.destination
    }
}

#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub(crate) enum ChannelTransferError {
    #[error("source and destination channels must differ")]
    SameChannel,
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
pub(crate) enum ChannelError {
    #[error("channel name must not be empty")]
    Empty,
    #[error("channel name must not contain '/'")]
    ContainsSlash,
    #[error("notes ref {notes_ref:?} is not a valid Git ref name: {validation_error}")]
    InvalidNotesRef {
        notes_ref: String,
        validation_error: String,
    },
}
