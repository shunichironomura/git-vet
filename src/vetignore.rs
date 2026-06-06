use std::path::Path;

use ignore::gitignore::{Gitignore, GitignoreBuilder};

use crate::error::AppError;
use crate::path::RepoPath;

#[derive(Debug)]
pub struct Vetignore {
    matcher: Gitignore,
}

impl Vetignore {
    pub(crate) fn load(root: &Path) -> Result<Self, AppError> {
        let path = root.join(".vetignore");
        let mut builder = GitignoreBuilder::new(root);
        if let Some(error) = path.exists().then(|| builder.add(&path)).flatten() {
            return Err(AppError::Vetignore(error.to_string()));
        }
        let matcher = builder
            .build()
            .map_err(|error| AppError::Vetignore(error.to_string()))?;
        Ok(Self { matcher })
    }

    pub(crate) fn is_ignored(&self, path: &RepoPath) -> bool {
        self.matcher
            .matched_path_or_any_parents(path.to_path_buf(), false)
            .is_ignore()
    }
}
