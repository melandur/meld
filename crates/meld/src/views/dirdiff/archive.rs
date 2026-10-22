//! Archives compared as folders (`_mount_archives_and_open()` in
//! `meldwindow.py`, the archive parts of `dirdiff.py`).
//!
//! An archive given as a folder is mounted with GVfs, or extracted into
//! a temporary folder without it, on a background thread
//! ([`meld_files::open_archive`]); the pane then compares that folder
//! while its labels show the archive. The folder is unmounted or deleted
//! when the pane changes or the comparison closes (dropping the
//! [`ArchiveFolder`] does that).

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use meld_files::ArchiveFolder;

/// A pane showing an archive.
#[derive(Debug)]
pub struct ArchivePane {
    /// The archive file.
    pub archive: PathBuf,
    /// Distinguishes this opening from earlier ones of the same pane.
    pub generation: u64,
    /// The opened folder, once ready.
    pub folder: Option<ArchiveFolder>,
}

impl ArchivePane {
    pub fn opening(&self) -> bool {
        self.folder.is_none()
    }
}

/// Hands an opened archive folder through a message. If the message is
/// dropped (the tab closed first), so is the folder, which cleans it up.
#[derive(Clone)]
pub struct Opened(Arc<Mutex<Option<ArchiveFolder>>>);

impl Opened {
    pub fn new(folder: ArchiveFolder) -> Opened {
        Opened(Arc::new(Mutex::new(Some(folder))))
    }

    pub fn take(&self) -> Option<ArchiveFolder> {
        self.0.lock().ok()?.take()
    }
}

impl std::fmt::Debug for Opened {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Opened(..)")
    }
}

/// Opens `archive` as a folder (blocking; run it in the background).
pub fn open(archive: &Path) -> Result<Opened, String> {
    meld_files::open_archive(archive).map(Opened::new)
}

/// Whether every existing path is an archive (so that comparing them
/// compares their contents as folders).
pub fn all_archives(paths: &[Option<&Path>]) -> bool {
    let present: Vec<&Path> = paths.iter().flatten().copied().collect();
    !present.is_empty() && present.iter().all(|p| meld_files::is_archive(p))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn archive_rows() {
        let dir = tempfile::tempdir().unwrap();
        let zip = dir.path().join("a.zip");
        std::fs::write(&zip, b"PK\x03\x04rest").unwrap();
        let text = dir.path().join("a.txt");
        std::fs::write(&text, b"hello").unwrap();
        assert!(all_archives(&[Some(&zip), None, Some(&zip)]));
        assert!(!all_archives(&[Some(&zip), Some(&text)]));
        assert!(!all_archives(&[None, None]));
    }
}
