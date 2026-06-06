use std::collections::HashMap;

use crate::git_types::{BlobOid, CommitOid, FileMode};
use crate::path::RepoPath;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ReviewRecord {
    pub(crate) reviewed_at: String,
    pub(crate) reviewer: String,
    pub(crate) commit: CommitOid,
    pub(crate) path: RepoPath,
}

impl ReviewRecord {
    fn render(&self) -> String {
        format!(
            "reviewed-at={} reviewer={} commit={} path={}",
            self.reviewed_at, self.reviewer, self.commit, self.path
        )
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
            .max_by(|left, right| left.reviewed_at.cmp(&right.reviewed_at))
            .map(|record| ReviewMetadata {
                last_reviewed_at: record.reviewed_at.clone(),
                reviewer: record.reviewer.clone(),
            })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ReviewMetadata {
    pub(crate) last_reviewed_at: String,
    pub(crate) reviewer: String,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct ReviewedSet {
    pub(crate) by_blob: HashMap<BlobOid, ReviewInfo>,
}

impl ReviewedSet {
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
    Stale {
        baseline: BlobOid,
        baseline_mode: FileMode,
    },
    New,
}

impl ReviewState {
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Self::Vetted => "vetted",
            Self::Stale { .. } => "stale",
            Self::New => "new",
        }
    }

    pub(crate) fn baseline(&self) -> Option<&BlobOid> {
        match self {
            Self::Stale { baseline, .. } => Some(baseline),
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

pub(crate) fn append_record(existing: Option<&str>, new_record: &ReviewRecord) -> String {
    let already_recorded = existing
        .map(parse_note_records)
        .unwrap_or_default()
        .iter()
        .any(|record| {
            record.reviewer == new_record.reviewer
                && record.commit == new_record.commit
                && record.path == new_record.path
        });

    let mut lines = existing
        .into_iter()
        .flat_map(str::lines)
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(ToOwned::to_owned)
        .chain((!already_recorded).then(|| new_record.render()))
        .collect::<Vec<_>>();
    lines.sort();
    lines.dedup();

    match lines.is_empty() {
        true => String::new(),
        false => format!("{}\n", lines.join("\n")),
    }
}

pub(crate) fn parse_note_records(body: &str) -> Vec<ReviewRecord> {
    body.lines().filter_map(parse_note_record).collect()
}

fn parse_note_record(line: &str) -> Option<ReviewRecord> {
    let fields = line
        .split_whitespace()
        .filter_map(|field| field.split_once('='))
        .collect::<HashMap<_, _>>();

    Some(ReviewRecord {
        reviewed_at: fields.get("reviewed-at")?.to_string(),
        reviewer: fields.get("reviewer")?.to_string(),
        commit: CommitOid::new(gix::ObjectId::from_hex(fields.get("commit")?.as_bytes()).ok()?),
        path: RepoPath::from_git_path(fields.get("path")?).ok()?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn oid(hex: &str) -> gix::ObjectId {
        gix::ObjectId::from_hex(hex.as_bytes()).unwrap()
    }

    #[test]
    fn append_record_sorts_and_deduplicates_records() {
        let record = ReviewRecord {
            reviewed_at: "2026-06-06T00:00:00Z".to_owned(),
            reviewer: "reviewer@example.com".to_owned(),
            commit: CommitOid::new(oid("0123456789012345678901234567890123456789")),
            path: RepoPath::from_git_path("src/main.rs").unwrap(),
        };
        let existing = "reviewed-at=2026-06-06T00:00:00Z reviewer=reviewer@example.com commit=0123456789012345678901234567890123456789 path=src/main.rs\n";

        assert_eq!(append_record(Some(existing), &record), existing);
    }
}
