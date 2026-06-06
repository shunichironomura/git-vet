use std::fmt;

use gix::objs::tree::EntryMode;
use serde::{Serialize, Serializer};

use crate::path::RepoPath;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub struct BlobOid(gix::ObjectId);

impl BlobOid {
    pub(crate) const fn new(oid: gix::ObjectId) -> Self {
        Self(oid)
    }
}

impl Serialize for BlobOid {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

impl fmt::Display for BlobOid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub struct CommitOid(gix::ObjectId);

impl CommitOid {
    pub(crate) const fn new(oid: gix::ObjectId) -> Self {
        Self(oid)
    }
}

impl fmt::Display for CommitOid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FileMode(EntryMode);

impl FileMode {
    pub(crate) const fn new(mode: EntryMode) -> Self {
        Self(mode)
    }

    pub(crate) const fn is_reviewable_file(self) -> bool {
        self.0.is_blob_or_symlink()
    }

    pub(crate) const fn is_submodule(self) -> bool {
        self.0.is_commit()
    }
}

#[derive(Clone, Debug)]
pub struct TrackedFile {
    pub(crate) path: RepoPath,
    pub(crate) blob: BlobOid,
}
