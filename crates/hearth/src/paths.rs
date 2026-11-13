//! The data folder. It is the only thing the command line configures.

use std::{
    io,
    path::{Path, PathBuf},
};

#[derive(Debug, Clone)]
pub struct Paths {
    root: PathBuf,
}

impl Paths {
    /// Makes the folder and what belongs inside it.
    pub fn create(root: &Path) -> io::Result<Self> {
        let root = std::path::absolute(root)?;
        for dir in ["", "tls", "art", "saves", "worlds", "backups", "logs", "emulators"] {
            std::fs::create_dir_all(root.join(dir))?;
        }
        Ok(Self { root })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn db(&self) -> PathBuf {
        self.root.join("hearth.db")
    }

    pub fn dir(&self, name: &str) -> PathBuf {
        self.root.join(name)
    }
}
