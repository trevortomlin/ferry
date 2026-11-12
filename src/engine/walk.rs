// Turning a folder into a list of files to move.
//
// Runs on the engine thread because half of it is remote I/O. Results come back
// in batches so a large tree starts filling the queue immediately instead of
// after the whole walk, and so an accidental drag of a home directory can be
// seen going wrong rather than appearing to hang.
//
// Both walks are iterative. A recursive async function needs boxing, and a
// directory tree has no bound on its depth — least of all one that came off a
// server.

use std::path::{Path, PathBuf};

use crate::model::message::ExpandedItem;
use crate::model::path;
use crate::model::transfer::Direction;
use crate::remote::{Collect, Provider, VfsError};

/// Files per batch handed back to the UI.
const BATCH: usize = 200;

/// Refuses to expand a tree beyond this. A misdirected drag of a home
/// directory should say so rather than quietly queue a hundred thousand
/// transfers.
const MAX_FILES: usize = 50_000;

/// And beyond this depth, which also stops a symlink cycle on a server that
/// reports linked directories as directories.
const MAX_DEPTH: usize = 64;

pub enum Batch {
    Items(Vec<ExpandedItem>),
    Done,
}

/// Walks a remote folder, creating the mirroring local directories as it goes
/// so empty ones survive the round trip.
///
/// `emit` is called per batch and returns false to abandon the walk.
pub async fn remote_tree(
    provider: &mut Provider,
    root_remote: &str,
    root_local: &Path,
    mut emit: impl FnMut(Batch) -> bool,
) -> Result<(), VfsError> {
    let mut pending = vec![(root_remote.to_owned(), root_local.to_path_buf(), 0usize)];
    let mut batch = Vec::with_capacity(BATCH);
    let mut found = 0usize;

    while let Some((remote, local, depth)) = pending.pop() {
        if depth > MAX_DEPTH {
            return Err(VfsError::Failed(format!(
                "{remote} is nested more than {MAX_DEPTH} deep; refusing to go further."
            )));
        }

        std::fs::create_dir_all(&local).map_err(|error| {
            VfsError::Failed(format!("Could not create {}: {error}", local.display()))
        })?;

        let mut listing = Collect::default();
        provider.list(&remote, &mut listing).await?;

        for index in 0..listing.entries.len() {
            let name = listing.entries.name(index);
            let child_remote = path::join(&remote, name);
            let child_local = local.join(name);

            if listing.entries.kind(index).is_dir() {
                pending.push((child_remote, child_local, depth + 1));
                continue;
            }

            found += 1;
            if found > MAX_FILES {
                return Err(VfsError::Failed(format!(
                    "That folder holds more than {MAX_FILES} files. Transfer it in pieces."
                )));
            }
            batch.push(ExpandedItem {
                direction: Direction::Download,
                remote: child_remote,
                local: child_local,
                size: listing.entries.size(index),
            });

            if batch.len() >= BATCH && !emit(Batch::Items(std::mem::take(&mut batch))) {
                return Ok(());
            }
        }
    }

    if !batch.is_empty() && !emit(Batch::Items(batch)) {
        return Ok(());
    }
    emit(Batch::Done);
    Ok(())
}

/// Walks a local folder, creating the mirroring remote directories.
///
/// Symbolic links are skipped rather than followed: a link pointing at an
/// ancestor turns the walk into an infinite one, and uploading the contents of
/// something the user linked to is rarely what they meant.
pub async fn local_tree(
    provider: &mut Provider,
    root_local: &Path,
    root_remote: &str,
    mut emit: impl FnMut(Batch) -> bool,
) -> Result<(), VfsError> {
    let mut pending = vec![(root_local.to_path_buf(), root_remote.to_owned(), 0usize)];
    let mut batch = Vec::with_capacity(BATCH);
    let mut found = 0usize;

    while let Some((local, remote, depth)) = pending.pop() {
        if depth > MAX_DEPTH {
            return Err(VfsError::Failed(format!(
                "{} is nested more than {MAX_DEPTH} deep; refusing to go further.",
                local.display()
            )));
        }

        // Already there is fine; anything else is worth reporting, since every
        // file underneath would fail too.
        if let Err(error) = provider.create_dir(&remote).await
            && !already_exists(&error)
        {
            return Err(error);
        }

        let children = std::fs::read_dir(&local).map_err(|error| {
            VfsError::Failed(format!("Could not read {}: {error}", local.display()))
        })?;

        for child in children {
            let child = child.map_err(|error| {
                VfsError::Failed(format!("Could not read {}: {error}", local.display()))
            })?;
            let name = child.file_name().to_string_lossy().into_owned();
            let child_local = child.path();
            let child_remote = path::join(&remote, &name);

            // `symlink_metadata` rather than `metadata`, so a link is seen as a
            // link instead of as whatever it points at.
            let Ok(meta) = std::fs::symlink_metadata(&child_local) else {
                continue;
            };
            if meta.file_type().is_symlink() {
                continue;
            }

            if meta.is_dir() {
                pending.push((child_local, child_remote, depth + 1));
                continue;
            }

            found += 1;
            if found > MAX_FILES {
                return Err(VfsError::Failed(format!(
                    "That folder holds more than {MAX_FILES} files. Transfer it in pieces."
                )));
            }
            batch.push(ExpandedItem {
                direction: Direction::Upload,
                remote: child_remote,
                local: child_local,
                size: meta.len(),
            });

            if batch.len() >= BATCH && !emit(Batch::Items(std::mem::take(&mut batch))) {
                return Ok(());
            }
        }
    }

    if !batch.is_empty() && !emit(Batch::Items(batch)) {
        return Ok(());
    }
    emit(Batch::Done);
    Ok(())
}

/// Creating a directory that is already there is not a failure worth stopping
/// for. Servers disagree about how to say so, hence the text match.
fn already_exists(error: &VfsError) -> bool {
    let rendered = error.to_string();
    rendered.contains("exist") || rendered.contains("Failure")
}

/// Where a folder should land locally: inside the chosen directory, under its
/// own name, the way every file manager does it.
pub fn destination_for(chosen: &Path, remote_folder: &str) -> PathBuf {
    chosen.join(path::file_name(remote_folder))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_downloaded_folder_lands_under_its_own_name() {
        assert_eq!(
            destination_for(Path::new("/home/ferry/Downloads"), "/srv/reports"),
            Path::new("/home/ferry/Downloads/reports")
        );
    }

    #[test]
    fn already_exists_recognises_the_usual_wording() {
        assert!(already_exists(&VfsError::Failed(
            "/a: the file already exists".to_owned()
        )));
        assert!(!already_exists(&VfsError::PermissionDenied(
            "/a".to_owned()
        )));
    }
}
