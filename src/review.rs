use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::git_types::{BlobOid, CommitOid};
use crate::path::RepoPath;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct Vetter {
    pub(crate) name: String,
    pub(crate) email: String,
}

impl Vetter {
    pub(crate) const fn new(name: String, email: String) -> Self {
        Self { name, email }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ReviewRecord {
    pub(crate) vetted_at: String,
    pub(crate) vetted_by: Vetter,
    pub(crate) commit: CommitOid,
    pub(crate) path: RepoPath,
}

impl ReviewRecord {
    fn render(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(&NoteRecord::from(self))
    }
}

#[derive(Clone, Debug, Default)]
pub(crate) struct ReviewInfo {
    pub(crate) records: Vec<ReviewRecord>,
}

impl ReviewInfo {
    fn latest_metadata(&self) -> Option<ReviewMetadata> {
        self.records
            .iter()
            .max_by(|left, right| left.vetted_at.cmp(&right.vetted_at))
            .map(|record| ReviewMetadata {
                last_vetted_at: record.vetted_at.clone(),
                vetted_by: record.vetted_by.clone(),
            })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ReviewMetadata {
    pub(crate) last_vetted_at: String,
    pub(crate) vetted_by: Vetter,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct ReviewedSet {
    pub(crate) by_blob: HashMap<BlobOid, ReviewInfo>,
}

impl ReviewedSet {
    pub(crate) fn is_empty(&self) -> bool {
        self.by_blob.is_empty()
    }

    pub(crate) fn contains(&self, oid: &BlobOid) -> bool {
        self.by_blob.contains_key(oid)
    }

    pub(crate) fn metadata(&self, oid: &BlobOid) -> Option<ReviewMetadata> {
        self.by_blob.get(oid).and_then(ReviewInfo::latest_metadata)
    }
}

#[derive(Clone, Debug)]
pub(crate) enum ReviewState {
    Vetted,
    Stale { baseline: BlobOid },
    New,
}

impl ReviewState {
    pub(crate) const fn as_str(&self) -> &'static str {
        match self {
            Self::Vetted => "vetted",
            Self::Stale { .. } => "stale",
            Self::New => "new",
        }
    }

    pub(crate) const fn baseline(&self) -> Option<&BlobOid> {
        match self {
            Self::Stale { baseline } => Some(baseline),
            Self::Vetted | Self::New => None,
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct ClassifiedFile {
    pub(crate) path: RepoPath,
    pub(crate) state: ReviewState,
    pub(crate) blob: BlobOid,
    pub(crate) metadata: Option<ReviewMetadata>,
}

pub(crate) fn append_record(
    existing: Option<&str>,
    new_record: &ReviewRecord,
) -> Result<String, serde_json::Error> {
    let already_recorded = existing
        .map(parse_note_records)
        .unwrap_or_default()
        .iter()
        .any(|record| {
            record.vetted_by == new_record.vetted_by
                && record.commit == new_record.commit
                && record.path == new_record.path
        });

    let mut lines = existing
        .into_iter()
        .flat_map(str::lines)
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();
    if !already_recorded {
        lines.push(new_record.render()?);
    }
    lines.sort();
    lines.dedup();

    if lines.is_empty() {
        Ok(String::new())
    } else {
        Ok(format!("{}\n", lines.join("\n")))
    }
}

pub(crate) fn parse_note_records(body: &str) -> Vec<ReviewRecord> {
    body.lines().filter_map(parse_note_record).collect()
}

fn parse_note_record(line: &str) -> Option<ReviewRecord> {
    serde_json::from_str::<NoteRecord>(line)
        .ok()
        .and_then(NoteRecord::into_review_record)
}

#[derive(Deserialize, Serialize)]
struct NoteRecord {
    vetted_at: String,
    vetted_by: Vetter,
    commit: String,
    path: String,
}

impl From<&ReviewRecord> for NoteRecord {
    fn from(record: &ReviewRecord) -> Self {
        Self {
            vetted_at: record.vetted_at.clone(),
            vetted_by: record.vetted_by.clone(),
            commit: record.commit.to_string(),
            path: record.path.to_string(),
        }
    }
}

impl NoteRecord {
    fn into_review_record(self) -> Option<ReviewRecord> {
        Some(ReviewRecord {
            vetted_at: self.vetted_at,
            vetted_by: self.vetted_by,
            commit: CommitOid::new(gix::ObjectId::from_hex(self.commit.as_bytes()).ok()?),
            path: RepoPath::from_git_path(&self.path).ok()?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn oid(hex: &str) -> Result<gix::ObjectId, Box<dyn std::error::Error>> {
        gix::ObjectId::from_hex(hex.as_bytes()).map_err(Into::into)
    }

    #[test]
    fn append_record_sorts_and_deduplicates_records() -> Result<(), Box<dyn std::error::Error>> {
        let record = ReviewRecord {
            vetted_at: "2026-06-06T00:00:00Z".to_owned(),
            vetted_by: Vetter::new("Reviewer".to_owned(), "reviewer@example.com".to_owned()),
            commit: CommitOid::new(oid("0123456789012345678901234567890123456789")?),
            path: RepoPath::from_git_path("src/main.rs")?,
        };
        let existing = "{\"vetted_at\":\"2026-06-06T00:00:00Z\",\"vetted_by\":{\"name\":\"Reviewer\",\"email\":\"reviewer@example.com\"},\"commit\":\"0123456789012345678901234567890123456789\",\"path\":\"src/main.rs\"}\n";

        assert_eq!(append_record(Some(existing), &record)?, existing);
        Ok(())
    }
}
