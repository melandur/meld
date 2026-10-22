//! Moving files to the freedesktop.org trash
//!
//! Replaces `trash_or_confirm()` from `meld/iohelpers.py`, which used
//! `g_file_trash()`. This implements the [FreeDesktop.org Trash
//! specification] directly: files on the same file system as the home
//! trash go to `$XDG_DATA_HOME/Trash`, others to `$topdir/.Trash/$uid`
//! (if an administrator created a sticky `.Trash`) or
//! `$topdir/.Trash-$uid`.
//!
//! When a file can't be trashed (e.g., there's no usable trash folder
//! on its mount), [`trash_or_confirm`] returns
//! [`TrashError::NeedsConfirmation`]; the app should then ask
//! "Delete Permanently?" and call [`delete_permanently`] if the user
//! agrees.
//!
//! [FreeDesktop.org Trash specification]: https://specifications.freedesktop.org/trash-spec/latest/

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

use thiserror::Error;

/// Errors from trashing files
#[derive(Debug, Error)]
pub enum TrashError {
    /// The file can't be moved to a trash folder; the user should be
    /// asked whether to delete it permanently instead
    #[error("“{}” can’t be put in the trash. Do you want to delete it permanently?", .path.display())]
    NeedsConfirmation {
        /// The file that couldn't be trashed
        path: PathBuf,
        /// Whether the file is a folder
        is_dir: bool,
        /// Why trashing failed
        reason: String,
    },
    /// The path is neither a regular file, a folder nor a symlink
    #[error("Not a file or directory")]
    NotAFileOrDirectory(PathBuf),
    /// Some other error, e.g., permission denied
    #[error("{}: {source}", .path.display())]
    Io {
        /// The file being trashed
        path: PathBuf,
        /// The underlying error
        source: io::Error,
    },
}

/// Where a file ended up in the trash
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrashedFile {
    /// The new location of the file, under `<trash>/files`
    pub trashed_path: PathBuf,
    /// The `.trashinfo` file describing it, under `<trash>/info`
    pub info_path: PathBuf,
}

/// Trash a file or folder, or report that it needs confirmation for a
/// permanent deletion
pub fn trash_or_confirm(path: &Path) -> Result<TrashedFile, TrashError> {
    let home_trash = home_trash_dir();
    trash_with_home(path, home_trash.as_deref())
}

/// Permanently delete a file, symlink or (recursively) a folder
pub fn delete_permanently(path: &Path) -> io::Result<()> {
    let meta = fs::symlink_metadata(path)?;
    if meta.is_dir() {
        fs::remove_dir_all(path)
    } else {
        fs::remove_file(path)
    }
}

/// The user's home trash folder, `$XDG_DATA_HOME/Trash`
pub fn home_trash_dir() -> Option<PathBuf> {
    let data_home = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| crate::iohelpers::home_dir().map(|h| h.join(".local/share")))?;
    Some(data_home.join("Trash"))
}

fn unsupported(path: &Path, is_dir: bool, reason: impl Into<String>) -> TrashError {
    TrashError::NeedsConfirmation {
        path: path.to_path_buf(),
        is_dir,
        reason: reason.into(),
    }
}

/// [`trash_or_confirm`] with an explicit home trash folder
pub(crate) fn trash_with_home(
    path: &Path,
    home_trash: Option<&Path>,
) -> Result<TrashedFile, TrashError> {
    let io_err = |source| TrashError::Io {
        path: path.to_path_buf(),
        source,
    };
    let path = std::path::absolute(path).map_err(io_err)?;
    let path = crate::iohelpers::canonicalize_lexically(&path);
    let meta = fs::symlink_metadata(&path).map_err(io_err)?;
    let file_type = meta.file_type();
    let is_dir = file_type.is_dir();
    if !(is_dir || file_type.is_file() || file_type.is_symlink()) {
        return Err(TrashError::NotAFileOrDirectory(path));
    }
    if path.parent().is_none() {
        return Err(unsupported(&path, is_dir, "can't trash the root folder"));
    }

    let (trash_dir, topdir) = match home_trash.and_then(|t| prepare_home_trash(t, meta.dev())) {
        Some(dir) => (dir, None),
        None => {
            let topdir = find_topdir(&path, meta.dev());
            match topdir_trash(&topdir) {
                Some(dir) => (dir, Some(topdir)),
                None => {
                    return Err(unsupported(
                        &path,
                        is_dir,
                        "no trash folder available on this mount",
                    ));
                }
            }
        }
    };

    let info_dir = trash_dir.join("info");
    let files_dir = trash_dir.join("files");

    // Paths in a top directory trash are relative to the top directory
    let recorded = match &topdir {
        Some(top) => path.strip_prefix(top).unwrap_or(&path).to_path_buf(),
        None => path.clone(),
    };
    let contents = format!(
        "[Trash Info]\nPath={}\nDeletionDate={}\n",
        escape_path(&recorded),
        local_timestamp()
    );

    let base = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    for attempt in 1u32.. {
        let name = if attempt == 1 {
            base.clone()
        } else {
            format!("{base}.{attempt}")
        };
        let info_path = info_dir.join(format!("{name}.trashinfo"));
        let trashed_path = files_dir.join(&name);
        if fs::symlink_metadata(&trashed_path).is_ok() {
            continue;
        }
        let mut info = match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&info_path)
        {
            Ok(file) => file,
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(unsupported(&path, is_dir, e.to_string())),
        };
        if let Err(e) = info
            .write_all(contents.as_bytes())
            .and_then(|_| info.sync_all())
        {
            let _ = fs::remove_file(&info_path);
            return Err(unsupported(&path, is_dir, e.to_string()));
        }
        drop(info);

        return match fs::rename(&path, &trashed_path) {
            Ok(()) => Ok(TrashedFile {
                trashed_path,
                info_path,
            }),
            Err(e) => {
                let _ = fs::remove_file(&info_path);
                if e.raw_os_error() == Some(libc::EXDEV) {
                    Err(unsupported(&path, is_dir, e.to_string()))
                } else {
                    Err(TrashError::Io { path, source: e })
                }
            }
        };
    }
    unreachable!("the attempt counter is unbounded")
}

fn make_private_dir(path: &Path) -> io::Result<()> {
    match fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)
    {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists && path.is_dir() => Ok(()),
        Err(e) => Err(e),
    }
}

/// Use the home trash if it's on the device `dev`, creating it if needed
fn prepare_home_trash(trash: &Path, dev: u64) -> Option<PathBuf> {
    // The trash may not exist yet; compare against its closest
    // existing ancestor, which is where it would be created.
    let existing = trash.ancestors().find(|p| p.exists())?;
    if fs::metadata(existing).ok()?.dev() != dev {
        return None;
    }
    make_private_dir(&trash.join("files")).ok()?;
    make_private_dir(&trash.join("info")).ok()?;
    if fs::metadata(trash).ok()?.dev() != dev {
        return None;
    }
    Some(trash.to_path_buf())
}

/// The mount point containing `path`: its highest ancestor on `dev`
fn find_topdir(path: &Path, dev: u64) -> PathBuf {
    let mut top = path.parent().unwrap_or(path).to_path_buf();
    for ancestor in path.ancestors().skip(1) {
        match fs::metadata(ancestor) {
            Ok(meta) if meta.dev() == dev => top = ancestor.to_path_buf(),
            _ => break,
        }
    }
    top
}

/// The trash folder for a mount's top directory, created if needed
fn topdir_trash(topdir: &Path) -> Option<PathBuf> {
    // SAFETY: getuid() has no preconditions and cannot fail
    let uid = unsafe { libc::getuid() };

    // An administrator-provided $topdir/.Trash must be a real folder
    // with the sticky bit set
    let admin = topdir.join(".Trash");
    if let Ok(meta) = fs::symlink_metadata(&admin)
        && meta.is_dir()
        && meta.permissions().mode() & 0o1000 != 0
    {
        let dir = admin.join(uid.to_string());
        if make_trash_subdirs(&dir).is_ok() {
            return Some(dir);
        }
    }

    let dir = topdir.join(format!(".Trash-{uid}"));
    if let Ok(meta) = fs::symlink_metadata(&dir)
        && (!meta.is_dir() || meta.uid() != uid)
    {
        return None;
    }
    make_trash_subdirs(&dir).ok()?;
    Some(dir)
}

fn make_trash_subdirs(dir: &Path) -> io::Result<()> {
    make_private_dir(dir)?;
    let meta = fs::symlink_metadata(dir)?;
    if !meta.is_dir() {
        return Err(io::Error::other("trash folder is not a folder"));
    }
    make_private_dir(&dir.join("files"))?;
    make_private_dir(&dir.join("info"))
}

/// Percent-encode a path for a `.trashinfo` `Path=` key
fn escape_path(path: &Path) -> String {
    use std::os::unix::ffi::OsStrExt;
    let mut out = String::new();
    for &b in path.as_os_str().as_bytes() {
        if b.is_ascii_alphanumeric() || b"-_.~/!$&'()*+,;=:@".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// The current local time as `YYYY-MM-DDThh:mm:ss`
fn local_timestamp() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as libc::time_t)
        .unwrap_or(0);
    // SAFETY: localtime_r only writes to the provided tm struct, which
    // is zero-initialised plain data.
    let tm = unsafe {
        let mut tm: libc::tm = std::mem::zeroed();
        if libc::localtime_r(&now, &mut tm).is_null() {
            return "1970-01-01T00:00:00".to_string();
        }
        tm
    };
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}",
        tm.tm_year + 1900,
        tm.tm_mon + 1,
        tm.tm_mday,
        tm.tm_hour,
        tm.tm_min,
        tm.tm_sec
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trash_to_home_trash() {
        let dir = tempfile::tempdir().unwrap();
        let trash = dir.path().join("data/Trash");
        let file = dir.path().join("some file.txt");
        fs::write(&file, b"x").unwrap();

        let trashed = trash_with_home(&file, Some(&trash)).unwrap();
        assert!(!file.exists());
        assert_eq!(trashed.trashed_path, trash.join("files/some file.txt"));
        assert_eq!(fs::read(&trashed.trashed_path).unwrap(), b"x");
        let info = fs::read_to_string(&trashed.info_path).unwrap();
        assert!(info.starts_with("[Trash Info]\nPath=/"));
        assert!(info.contains("some%20file.txt\n"));
        assert!(info.contains("DeletionDate="));

        // A second file with the same name gets a unique trash name
        fs::write(&file, b"y").unwrap();
        let second = trash_with_home(&file, Some(&trash)).unwrap();
        assert_eq!(second.trashed_path, trash.join("files/some file.txt.2"));
    }

    #[test]
    fn trash_folder_and_symlink() {
        let dir = tempfile::tempdir().unwrap();
        let trash = dir.path().join("Trash");
        let sub = dir.path().join("sub");
        fs::create_dir(&sub).unwrap();
        fs::write(sub.join("a"), b"a").unwrap();
        let trashed = trash_with_home(&sub, Some(&trash)).unwrap();
        assert!(trashed.trashed_path.join("a").exists());

        let link = dir.path().join("link");
        std::os::unix::fs::symlink("/nonexistent", &link).unwrap();
        let trashed = trash_with_home(&link, Some(&trash)).unwrap();
        assert!(
            fs::symlink_metadata(&trashed.trashed_path)
                .unwrap()
                .file_type()
                .is_symlink()
        );
    }

    #[test]
    fn missing_file_is_io_error() {
        let dir = tempfile::tempdir().unwrap();
        let err =
            trash_with_home(&dir.path().join("missing"), Some(&dir.path().join("T"))).unwrap_err();
        assert!(matches!(err, TrashError::Io { .. }));
    }

    #[test]
    fn delete_permanently_removes_trees() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("sub");
        fs::create_dir_all(sub.join("deeper")).unwrap();
        fs::write(sub.join("deeper/f"), b"").unwrap();
        delete_permanently(&sub).unwrap();
        assert!(!sub.exists());
    }

    #[test]
    fn escapes_paths() {
        assert_eq!(escape_path(Path::new("/a b/ü%")), "/a%20b/%C3%BC%25");
    }
}
