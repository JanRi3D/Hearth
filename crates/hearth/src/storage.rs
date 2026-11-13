//! Revisions of a folder of files: the saves of one game for one account, or one shared
//! world. Clients write into `upload/` over WebDAV; a commit turns that into `rev-<n>/`.

use std::{
    io,
    path::{Path, PathBuf},
};

use crate::{library::manifest, tls::hex};

/// What a folder holds.
#[derive(Debug, Clone, PartialEq)]
pub struct Summary {
    pub bytes: u64,
    pub files: u32,
    /// BLAKE3 over every path, size and file content. Equal hashes mean equal folders.
    pub hash: String,
}

#[derive(Debug, Clone)]
pub struct Store {
    base: PathBuf,
}

impl Store {
    pub fn new(base: PathBuf) -> Self {
        Self { base }
    }

    pub fn upload(&self) -> PathBuf {
        self.base.join("upload")
    }

    pub fn revision(&self, number: u32) -> PathBuf {
        self.base.join(format!("rev-{number}"))
    }

    /// Turns the upload area into revision `number`. `None` when nothing was uploaded.
    /// Afterwards the upload area holds the same files again, as hard links that take no
    /// space, so the next sync from a client only sends what changed.
    pub fn commit(&self, number: u32) -> io::Result<Option<Summary>> {
        let upload = self.upload();
        if !upload.is_dir() {
            return Ok(None);
        }
        // What an interrupted upload left behind is not part of the saves.
        for (relative, _, _) in manifest::list_files(&upload)? {
            if is_partial(&relative) {
                let _ = std::fs::remove_file(upload.join(&relative));
            }
        }
        let summary = summarize(&upload)?;
        if summary.files == 0 {
            return Ok(None);
        }
        let revision = self.revision(number);
        if revision.exists() {
            std::fs::remove_dir_all(&revision)?;
        }
        std::fs::rename(&upload, &revision)?;
        self.refill_upload(number);
        Ok(Some(summary))
    }

    /// Makes revision `to` a copy of revision `from`.
    pub fn copy_revision(&self, from: u32, to: u32) -> io::Result<Summary> {
        let (source, target) = (self.revision(from), self.revision(to));
        if target.exists() {
            std::fs::remove_dir_all(&target)?;
        }
        mirror(&source, &target, |from, to| std::fs::copy(from, to).map(|_| ()))?;
        self.refill_upload(to);
        summarize(&target)
    }

    /// Puts the files of a revision into the upload area as hard links. Best effort: where
    /// links are not possible the area stays empty and the next sync uploads everything.
    pub fn refill_upload(&self, number: u32) {
        let upload = self.upload();
        let _ = std::fs::remove_dir_all(&upload);
        if mirror(&self.revision(number), &upload, |from, to| std::fs::hard_link(from, to)).is_err() {
            let _ = std::fs::remove_dir_all(&upload);
        }
    }

    pub fn clear_upload(&self) {
        let _ = std::fs::remove_dir_all(self.upload());
    }

    pub fn remove_revision(&self, number: u32) {
        let _ = std::fs::remove_dir_all(self.revision(number));
    }

    /// Deletes everything this store holds.
    pub fn remove_all(&self) {
        let _ = std::fs::remove_dir_all(&self.base);
    }
}

/// Recreates the tree under `from` at `to`, placing each file with `place`.
fn mirror(from: &Path, to: &Path, place: impl Fn(&Path, &Path) -> io::Result<()>) -> io::Result<()> {
    std::fs::create_dir_all(to)?;
    for (relative, _, _) in manifest::list_files(from)? {
        let (source, target) = (from.join(&relative), to.join(&relative));
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
        place(&source, &target)?;
    }
    Ok(())
}

pub fn summarize(dir: &Path) -> io::Result<Summary> {
    let mut all = blake3::Hasher::new();
    let (mut bytes, mut files) = (0u64, 0u32);
    for (relative, size, _) in manifest::list_files(dir)? {
        // Partly written uploads are not part of the folder's content.
        if is_partial(&relative) {
            continue;
        }
        let (hash, _) = manifest::hash_file(&dir.join(&relative))?;
        all.update(relative.as_bytes());
        all.update(&[0]);
        all.update(&size.to_le_bytes());
        all.update(&hash);
        bytes += size;
        files += 1;
    }
    Ok(Summary { bytes, files, hash: hex(all.finalize().as_bytes()) })
}

/// Total size of the files under `dir`. 0 when it does not exist.
pub fn size_of(dir: &Path) -> u64 {
    manifest::list_files(dir).map(|files| files.iter().map(|(_, size, _)| size).sum()).unwrap_or(0)
}

/// The name an upload is written under until it is complete.
pub fn partial_name(name: &str) -> String {
    format!(".{name}.hearth-part")
}

fn is_partial(relative: &str) -> bool {
    relative.rsplit('/').next().is_some_and(|name| name.starts_with('.') && name.ends_with(".hearth-part"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_upload_becomes_a_revision_and_the_next_upload_starts_from_it() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(dir.path().join("saves/1/7"));
        assert_eq!(store.commit(1).unwrap(), None, "nothing uploaded, nothing to commit");

        std::fs::create_dir_all(store.upload().join("slot1")).unwrap();
        std::fs::write(store.upload().join("slot1/world.sav"), b"day 12").unwrap();
        std::fs::write(store.upload().join("options.ini"), b"volume=7").unwrap();
        std::fs::write(store.upload().join(partial_name("half.sav")), b"not done").unwrap();
        let first = store.commit(1).unwrap().unwrap();
        assert_eq!((first.bytes, first.files), (14, 2), "a partly written file is not part of the saves");
        assert_eq!(std::fs::read(store.revision(1).join("slot1/world.sav")).unwrap(), b"day 12");
        assert_eq!(std::fs::read(store.upload().join("slot1/world.sav")).unwrap(), b"day 12", "the upload area starts from the revision");

        // Committing the same files again gives the same fingerprint, which is how "nothing changed" is told.
        assert_eq!(store.commit(2).unwrap().unwrap().hash, first.hash);

        // A change in the upload area must not reach back into the revision it was linked from.
        std::fs::remove_file(store.upload().join("slot1/world.sav")).unwrap();
        std::fs::write(store.upload().join("slot1/world.sav"), b"day 13!").unwrap();
        let third = store.commit(3).unwrap().unwrap();
        assert_ne!(third.hash, first.hash);
        assert_eq!(std::fs::read(store.revision(2).join("slot1/world.sav")).unwrap(), b"day 12");

        let restored = store.copy_revision(1, 4).unwrap();
        assert_eq!(restored.hash, first.hash);
        store.remove_revision(1);
        assert!(!store.revision(1).exists() && store.revision(4).exists());
        assert_eq!(size_of(&store.revision(4)), 14);
    }
}
