use std::path::{Path, PathBuf};

use ignore::gitignore::{Gitignore, GitignoreBuilder};

use crate::channel::ReviewChannel;
use crate::error::AppError;
use crate::path::RepoPath;

#[derive(Debug)]
pub(crate) struct Vetignore {
    matcher: Gitignore,
}

impl Vetignore {
    pub(crate) fn load(root: &Path, channel: &ReviewChannel) -> Result<Self, AppError> {
        let mut builder = GitignoreBuilder::new(root);
        add_existing_ignore_file(&mut builder, &root.join(".vetignore"))?;
        add_channel_ignore_file_if_present(&mut builder, &channel_ignore_path(root, channel))?;
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

fn add_existing_ignore_file(builder: &mut GitignoreBuilder, path: &Path) -> Result<(), AppError> {
    if let Some(error) = path.exists().then(|| builder.add(path)).flatten() {
        return Err(AppError::Vetignore(error.to_string()));
    }
    Ok(())
}

fn add_channel_ignore_file_if_present(
    builder: &mut GitignoreBuilder,
    path: &Path,
) -> Result<(), AppError> {
    if let Some(error) = path.is_file().then(|| builder.add(path)).flatten() {
        return Err(AppError::Vetignore(error.to_string()));
    }
    Ok(())
}

fn channel_ignore_path(root: &Path, channel: &ReviewChannel) -> PathBuf {
    root.join(format!(".vetignore.{}", channel.as_str()))
}
