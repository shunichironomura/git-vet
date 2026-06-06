use std::fmt;
use std::str::FromStr;

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

impl FromStr for ReviewChannel {
    type Err = ChannelError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        if input.is_empty() {
            return Err(ChannelError {
                channel: input.to_owned(),
                details: "channel name must not be empty".to_owned(),
            });
        }

        let ref_name = format!("{NOTES_REF_PREFIX}/{input}");
        let notes_ref = NotesRef::new(ref_name).map_err(|details| ChannelError {
            channel: input.to_owned(),
            details,
        })?;

        Ok(Self {
            name: input.to_owned(),
            notes_ref,
        })
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
    fn new(name: String) -> Result<Self, String> {
        gix::refs::FullName::try_from(name.clone()).map_err(|error| error.to_string())?;
        Ok(Self { name })
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
