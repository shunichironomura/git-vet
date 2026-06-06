use gix::bstr::ByteSlice;
use gix::objs::tree::EntryKind;

use crate::channel::NotesRef;
use crate::error::{AppError, git_error};
use crate::git::Git;
use crate::git_types::BlobOid;
use crate::review::{ReviewInfo, ReviewedSet, parse_note_records};

const NOTES_MERGE_STRATEGY_KEY: &str = "notes.mergeStrategy";
const NOTES_MERGE_STRATEGY: &str = "cat_sort_uniq";

pub(crate) trait NotesStore {
    fn list_reviewed(&self) -> Result<ReviewedSet, AppError>;
    fn note_body(&self, oid: &BlobOid) -> Result<Option<String>, AppError>;
    fn write_note_body(&self, oid: &BlobOid, body: &str) -> Result<(), AppError>;
    fn prune(&self) -> Result<(), AppError>;
}

#[derive(Clone, Debug)]
struct NoteEntry {
    annotated: BlobOid,
    note_blob: gix::ObjectId,
    path: String,
}

#[derive(Clone)]
pub(crate) struct GixNotesStore<'git> {
    git: &'git Git,
    notes_ref: NotesRef,
}

impl<'git> GixNotesStore<'git> {
    pub(crate) fn new(git: &'git Git, notes_ref: NotesRef) -> Self {
        Self { git, notes_ref }
    }

    fn configure_merge_strategy(&self) -> Result<(), AppError> {
        let config_path = self.git.repo.common_dir().join("config");
        let mut config = match config_path.exists() {
            true => gix_config::File::from_path_no_includes(
                config_path.clone(),
                gix_config::Source::Local,
            )
            .map_err(|err| git_error("reading repository config", err))?,
            false => gix_config::File::default(),
        };
        config
            .set_raw_value(NOTES_MERGE_STRATEGY_KEY, NOTES_MERGE_STRATEGY)
            .map_err(|err| git_error("updating repository config", err))?;
        std::fs::write(config_path, config.to_bstring())?;
        Ok(())
    }

    fn note_entries(&self) -> Result<Vec<NoteEntry>, AppError> {
        let Some(tree) = self.notes_tree()? else {
            return Ok(Vec::new());
        };
        tree.traverse()
            .breadthfirst
            .files()
            .map_err(|err| git_error("walking notes tree", err))?
            .into_iter()
            .filter(|entry| entry.mode.is_blob())
            .filter_map(|entry| self.note_entry_from_tree_record(entry).transpose())
            .collect()
    }

    fn note_entry_from_tree_record(
        &self,
        entry: gix::traverse::tree::recorder::Entry,
    ) -> Result<Option<NoteEntry>, AppError> {
        let note_path = entry
            .filepath
            .to_str()
            .map_err(|err| AppError::NonUtf8Path(err.to_string()))?
            .to_owned();
        let hex = note_path.replace('/', "");
        if hex.len() != self.git.repo.object_hash().len_in_hex() {
            return Ok(None);
        }
        let annotated = match gix::ObjectId::from_hex(hex.as_bytes()) {
            Ok(oid) => BlobOid::new(oid),
            Err(_) => return Ok(None),
        };
        Ok(Some(NoteEntry {
            annotated,
            note_blob: entry.oid,
            path: note_path,
        }))
    }

    fn note_entry(&self, oid: &BlobOid) -> Result<Option<NoteEntry>, AppError> {
        self.note_entries()
            .map(|entries| entries.into_iter().find(|entry| entry.annotated == *oid))
    }

    fn note_path(&self, oid: &BlobOid) -> Result<String, AppError> {
        self.note_entry(oid)
            .map(|entry| entry.map(|entry| entry.path))
            .map(|path| path.unwrap_or_else(|| oid.to_string()))
    }

    fn notes_tree(&self) -> Result<Option<gix::Tree<'_>>, AppError> {
        let reference = self
            .git
            .repo
            .try_find_reference(self.notes_ref.as_str())
            .map_err(|err| git_error("finding notes ref", err))?;
        match reference {
            Some(mut reference) => reference
                .peel_to_tree()
                .map(Some)
                .map_err(|err| git_error("reading notes tree", err)),
            None => Ok(None),
        }
    }

    fn notes_tree_id(&self) -> Result<Option<gix::ObjectId>, AppError> {
        self.notes_tree()
            .map(|tree| tree.map(|tree| tree.id().detach()))
    }

    fn notes_parent_commit(&self) -> Result<Option<gix::ObjectId>, AppError> {
        let Some(mut reference) = self
            .git
            .repo
            .try_find_reference(self.notes_ref.as_str())
            .map_err(|err| git_error("finding notes ref", err))?
        else {
            return Ok(None);
        };
        let target = reference
            .follow_to_object()
            .map_err(|err| git_error("resolving notes ref", err))?
            .detach();
        let object = self
            .git
            .repo
            .find_object(target)
            .map_err(|err| git_error("reading notes ref target", err))?;
        match object.kind {
            gix::objs::Kind::Commit => Ok(Some(target)),
            gix::objs::Kind::Tree => Err(AppError::InvalidNotesRefTarget { actual: "tree" }),
            gix::objs::Kind::Blob => Err(AppError::InvalidNotesRefTarget { actual: "blob" }),
            gix::objs::Kind::Tag => Err(AppError::InvalidNotesRefTarget { actual: "tag" }),
        }
    }

    fn commit_notes_tree(&self, tree_id: gix::ObjectId) -> Result<(), AppError> {
        let parent = self.notes_parent_commit()?;
        let parents = parent.into_iter().collect::<Vec<_>>();
        self.git
            .repo
            .commit(
                self.notes_ref.full_name(),
                "git-vet notes",
                tree_id,
                parents,
            )
            .map(|_| ())
            .map_err(|err| git_error("committing notes tree", err))
    }

    fn rewrite_notes_tree(
        &self,
        edit: impl FnOnce(&mut gix::object::tree::Editor<'_>) -> Result<(), AppError>,
    ) -> Result<(), AppError> {
        let base_tree = self
            .notes_tree_id()?
            .unwrap_or_else(|| gix::ObjectId::empty_tree(self.git.repo.object_hash()));
        let mut editor = self
            .git
            .repo
            .edit_tree(base_tree)
            .map_err(|err| git_error("editing notes tree", err))?;
        edit(&mut editor)?;
        let new_tree = editor
            .write()
            .map_err(|err| git_error("writing notes tree", err))?
            .detach();
        if new_tree != base_tree {
            self.commit_notes_tree(new_tree)?;
        }
        Ok(())
    }
}

impl NotesStore for GixNotesStore<'_> {
    fn list_reviewed(&self) -> Result<ReviewedSet, AppError> {
        self.note_entries()?
            .into_iter()
            .try_fold(ReviewedSet::default(), |mut reviewed, entry| {
                let mut body = self
                    .git
                    .repo
                    .find_blob(entry.note_blob)
                    .map_err(|err| git_error("reading note body", err))?;
                let body = String::from_utf8(body.take_data())
                    .map_err(|err| AppError::NonUtf8Path(err.to_string()))?;
                let records = parse_note_records(&body);
                reviewed
                    .by_blob
                    .insert(entry.annotated, ReviewInfo { records });
                Ok(reviewed)
            })
    }

    fn note_body(&self, oid: &BlobOid) -> Result<Option<String>, AppError> {
        let Some(entry) = self.note_entry(oid)? else {
            return Ok(None);
        };
        let mut body = self
            .git
            .repo
            .find_blob(entry.note_blob)
            .map_err(|err| git_error("reading note body", err))?;
        String::from_utf8(body.take_data())
            .map(Some)
            .map_err(|err| AppError::NonUtf8Path(err.to_string()))
    }

    fn write_note_body(&self, oid: &BlobOid, body: &str) -> Result<(), AppError> {
        self.configure_merge_strategy()?;
        let note_path = self.note_path(oid)?;
        let note_blob = self
            .git
            .repo
            .write_blob(body.as_bytes())
            .map_err(|err| git_error("writing note blob", err))?
            .detach();
        self.rewrite_notes_tree(|editor| {
            editor
                .upsert(&note_path, EntryKind::Blob, note_blob)
                .map_err(|err| git_error("updating note entry", err))?;
            Ok(())
        })
    }

    fn prune(&self) -> Result<(), AppError> {
        let entries = self.note_entries()?;
        let stale_paths = entries
            .into_iter()
            .filter(|entry| !self.git.repo.has_object(entry.annotated.as_object_id()))
            .map(|entry| entry.path)
            .collect::<Vec<_>>();
        if stale_paths.is_empty() {
            return Ok(());
        }
        self.rewrite_notes_tree(|editor| {
            stale_paths.iter().try_for_each(|path| {
                editor
                    .remove(path)
                    .map_err(|err| git_error("removing stale note", err))?;
                println!("Removing note for object {path}");
                Ok(())
            })
        })
    }
}
