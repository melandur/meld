//! File operations performed by the folder comparison view
//!
//! Ports `copy2()` and `copytree()` from `meld/misc.py` and the copy
//! logic of `DirDiff.copy_selected()`. Deleting goes through
//! [`crate::trash::trash_or_confirm`].

use std::fs::{self, FileTimes};
use std::io;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use thiserror::Error;

/// Errors from copying files between panes
#[derive(Debug, Error)]
pub enum FileOpError {
    /// The destination folder exists; ask "Replace folder?" and retry
    /// with `replace_folders` if the user agrees. Replacing copies the
    /// source tree over the existing folder, as Meld does.
    #[error("Another folder with the same name already exists in “{}”", .path.parent().unwrap_or(.path).display())]
    FolderExists {
        /// The existing destination folder
        path: PathBuf,
    },
    /// Copying failed
    #[error("Couldn’t copy {}\nto {}.\n\n{source}", .src.display(), .dst.display())]
    Copy {
        /// Source path
        src: PathBuf,
        /// Destination path
        dst: PathBuf,
        /// The underlying error
        source: io::Error,
    },
}

/// What [`copy_entry`] copied
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CopyOutcome {
    /// A file (or file symlink) was copied; refresh the row
    File,
    /// A folder tree was copied; rescan the row
    Folder,
    /// The source is neither a file nor a folder, so nothing was done
    Nothing,
}

fn ignorable(err: &io::Error, codes: &[i32]) -> bool {
    err.raw_os_error().is_some_and(|code| codes.contains(&code))
}

/// Copy permission bits and access/modification times, like
/// `shutil.copystat()`
pub fn copystat(src: &Path, dst: &Path) -> io::Result<()> {
    let meta = fs::metadata(src)?;
    let times = FileTimes::new()
        .set_accessed(meta.accessed()?)
        .set_modified(meta.modified()?);
    // Explicit times only need ownership, so a read-only handle works
    fs::File::open(dst)?.set_times(times)?;
    fs::set_permissions(dst, meta.permissions())
}

/// Copy a file like `shutil.copy2()`, but ignore permission errors when
/// copying metadata, and copy symlinks as links
///
/// If `dst` is a folder, the file is copied into it.
pub fn copy2(src: &Path, dst: &Path) -> io::Result<()> {
    let dst = if dst.is_dir() {
        dst.join(
            src.file_name()
                .ok_or_else(|| io::Error::other("Not a file"))?,
        )
    } else {
        dst.to_path_buf()
    };

    let is_link = fs::symlink_metadata(src)?.file_type().is_symlink();
    if is_link && src.is_file() {
        if fs::symlink_metadata(&dst).is_ok() {
            fs::remove_file(&dst)?;
        }
        std::os::unix::fs::symlink(fs::read_link(src)?, &dst)?;
        // The link's target is shared, so there's no metadata to copy
        return Ok(());
    } else if src.is_file() {
        fs::copy(src, &dst)?;
    } else {
        return Err(io::Error::other("Not a file"));
    }

    match copystat(src, &dst) {
        Err(e) if !ignorable(&e, &[libc::EPERM, libc::ENOTSUP]) => Err(e),
        _ => Ok(()),
    }
}

/// Copy a folder tree like `shutil.copytree()`, but always copy
/// symlinks as links and don't fail if the destination exists
pub fn copytree(src: &Path, dst: &Path) -> io::Result<()> {
    // If the source tree is a symlink, duplicate the link and we're done
    if fs::symlink_metadata(src)?.file_type().is_symlink() {
        return std::os::unix::fs::symlink(fs::read_link(src)?, dst);
    }

    match fs::create_dir(dst) {
        Err(e) if e.kind() != io::ErrorKind::AlreadyExists => return Err(e),
        _ => {}
    }
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let src_name = entry.path();
        let dst_name = dst.join(entry.file_name());
        let file_type = fs::symlink_metadata(&src_name)?.file_type();
        if file_type.is_symlink() {
            std::os::unix::fs::symlink(fs::read_link(&src_name)?, &dst_name)?;
        } else if file_type.is_dir() {
            copytree(&src_name, &dst_name)?;
        } else {
            copy2(&src_name, &dst_name)?;
        }
    }

    match copystat(src, dst) {
        Err(e) if !ignorable(&e, &[libc::EPERM]) => Err(e),
        _ => Ok(()),
    }
}

/// Copy a row's entry from one pane to another
///
/// Files are copied into the destination's folder (created if missing)
/// with their metadata. Folders are copied recursively; if the
/// destination folder exists, [`FileOpError::FolderExists`] is returned
/// unless `replace_folders` is set.
pub fn copy_entry(
    src: &Path,
    dst: &Path,
    replace_folders: bool,
) -> Result<CopyOutcome, FileOpError> {
    let err = |source| FileOpError::Copy {
        src: src.to_path_buf(),
        dst: dst.to_path_buf(),
        source,
    };
    if src.is_file() {
        let dst_dir = dst.parent().unwrap_or(Path::new("."));
        if !dst_dir.exists() {
            fs::create_dir_all(dst_dir).map_err(err)?;
        }
        copy2(src, dst_dir).map_err(err)?;
        Ok(CopyOutcome::File)
    } else if src.is_dir() {
        if dst.exists() && !replace_folders {
            return Err(FileOpError::FolderExists {
                path: dst.to_path_buf(),
            });
        }
        copytree(src, dst).map_err(err)?;
        Ok(CopyOutcome::Folder)
    } else {
        Ok(CopyOutcome::Nothing)
    }
}

/// Create a folder and any missing parents
pub fn create_missing_dirs(path: &Path) -> io::Result<()> {
    fs::create_dir_all(path)
}

/// Whether two paths are the same file on disk (same device and inode)
pub fn same_file(a: &Path, b: &Path) -> bool {
    match (fs::metadata(a), fs::metadata(b)) {
        (Ok(a), Ok(b)) => a.dev() == b.dev() && a.ino() == b.ino(),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::time::{Duration, SystemTime};

    #[test]
    fn copy_file_with_metadata() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("left/f.txt");
        fs::create_dir_all(src.parent().unwrap()).unwrap();
        fs::write(&src, b"data").unwrap();
        let mtime = SystemTime::UNIX_EPOCH + Duration::from_secs(1_600_000_000);
        fs::File::options()
            .write(true)
            .open(&src)
            .unwrap()
            .set_modified(mtime)
            .unwrap();
        fs::set_permissions(&src, fs::Permissions::from_mode(0o444)).unwrap();

        let dst = dir.path().join("right/deeper/f.txt");
        assert_eq!(copy_entry(&src, &dst, false).unwrap(), CopyOutcome::File);
        assert_eq!(fs::read(&dst).unwrap(), b"data");
        let meta = fs::metadata(&dst).unwrap();
        assert_eq!(meta.modified().unwrap(), mtime);
        assert_eq!(meta.permissions().mode() & 0o777, 0o444);
    }

    #[test]
    fn copy_symlink_as_link() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("target"), b"t").unwrap();
        let link = dir.path().join("link");
        std::os::unix::fs::symlink("target", &link).unwrap();
        let out = dir.path().join("out");
        fs::create_dir(&out).unwrap();
        copy2(&link, &out).unwrap();
        assert_eq!(
            fs::read_link(out.join("link")).unwrap(),
            PathBuf::from("target")
        );
    }

    #[test]
    fn copy_folder_tree() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        fs::create_dir_all(src.join("sub")).unwrap();
        fs::write(src.join("a"), b"a").unwrap();
        fs::write(src.join("sub/b"), b"b").unwrap();
        std::os::unix::fs::symlink("a", src.join("l")).unwrap();

        let dst = dir.path().join("dst");
        assert_eq!(copy_entry(&src, &dst, false).unwrap(), CopyOutcome::Folder);
        assert_eq!(fs::read(dst.join("sub/b")).unwrap(), b"b");
        assert!(
            fs::symlink_metadata(dst.join("l"))
                .unwrap()
                .file_type()
                .is_symlink()
        );

        // Copying again needs confirmation
        assert!(matches!(
            copy_entry(&src, &dst, false),
            Err(FileOpError::FolderExists { .. })
        ));
        fs::remove_file(dst.join("l")).unwrap();
        fs::write(dst.join("extra"), b"").unwrap();
        copy_entry(&src, &dst, true).unwrap();
        assert!(dst.join("extra").exists(), "replacing merges, as in Meld");
    }

    #[test]
    fn copy_missing_source_does_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let outcome =
            copy_entry(&dir.path().join("missing"), &dir.path().join("x"), false).unwrap();
        assert_eq!(outcome, CopyOutcome::Nothing);
        assert!(copy2(&dir.path().join("missing"), &dir.path().join("x")).is_err());
    }
}
