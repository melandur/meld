//! Comparing the contents of archives
//!
//! Port of `meld/archivehelpers.py`. Meld mounts archives with GVfs'
//! `archive://` backend and compares the mounted folders. Without GIO,
//! this module detects archives, builds the `archive://` URIs and the
//! `gio mount` command lines to mount and unmount them, and locates the
//! GVfs FUSE folder of a mount.

use std::io::Read;
use std::path::{Path, PathBuf};

/// Guess an archive content type from a file's name and magic bytes
///
/// Only archive types are recognised; anything else gives `None`.
pub fn archive_content_type(path: &Path) -> Option<&'static str> {
    let mut header = Vec::with_capacity(0x8006);
    std::fs::File::open(path)
        .ok()?
        .take(0x8006)
        .read_to_end(&mut header)
        .ok()?;
    let name = path.file_name()?.to_string_lossy().to_lowercase();
    archive_type_from_data(&name, &header)
}

fn archive_type_from_data(name: &str, header: &[u8]) -> Option<&'static str> {
    let has = |offset: usize, magic: &[u8]| header.get(offset..offset + magic.len()) == Some(magic);
    let ends = |suffixes: &[&str]| suffixes.iter().any(|s| name.ends_with(s));

    if has(0, b"PK\x03\x04") || has(0, b"PK\x05\x06") {
        return Some(if ends(&[".jar", ".war", ".ear"]) {
            "application/java-archive"
        } else if ends(&[".apk"]) {
            "application/vnd.android.package-archive"
        } else {
            "application/zip"
        });
    }
    if has(0, b"7z\xBC\xAF\x27\x1C") {
        return Some("application/x-7z-compressed");
    }
    if has(0, b"Rar!\x1A\x07") {
        return Some("application/x-rar");
    }
    if has(0, b"MSCF") {
        return Some("application/vnd.ms-cab-compressed");
    }
    if has(0, b"!<arch>\n") {
        return Some(if ends(&[".deb", ".udeb"]) {
            "application/x-deb"
        } else {
            "application/x-archive"
        });
    }
    if has(0, b"\xED\xAB\xEE\xDB") {
        return Some("application/x-rpm");
    }
    if has(0, b"070701")
        || has(0, b"070702")
        || has(0, b"070707")
        || has(0, b"\xC7\x71")
        || has(0, b"\x71\xC7")
    {
        return Some("application/x-cpio");
    }
    if has(0, b"xar!") {
        return Some("application/x-xar");
    }
    if has(0, b"SIT!") || has(0, b"StuffIt ") {
        return Some("application/x-stuffit");
    }
    if has(2, b"-lh") {
        return Some("application/x-lha");
    }
    if has(257, b"ustar") {
        return Some("application/x-tar");
    }
    if has(0x8001, b"CD001") {
        return Some("application/x-cd-image");
    }
    if has(0, b"\x1F\x8B") && ends(&[".tar.gz", ".tgz"]) {
        return Some("application/x-compressed-tar");
    }
    if has(0, b"BZh") && ends(&[".tar.bz2", ".tbz2", ".tbz", ".tb2"]) {
        return Some("application/x-bzip-compressed-tar");
    }
    if has(0, b"\xFD7zXZ\x00") && ends(&[".tar.xz", ".txz"]) {
        return Some("application/x-xz-compressed-tar");
    }
    if ends(&[".tar.lzma", ".tlz"]) {
        return Some("application/x-lzma-compressed-tar");
    }
    if (has(0, b"\x1F\x9D") || has(0, b"\x1F\xA0")) && ends(&[".tar.z", ".taz"]) {
        return Some("application/x-tarz");
    }
    None
}

/// Whether a path is a regular file of an archive content type
pub fn is_archive(path: &Path) -> bool {
    match std::fs::metadata(path) {
        Ok(meta) if meta.is_file() => archive_content_type(path).is_some(),
        Ok(_) => false,
        Err(err) => {
            log::warn!("Could not query file info for {}: {err}", path.display());
            false
        }
    }
}

fn percent_encode(bytes: &[u8], keep: &[u8]) -> String {
    let mut out = String::new();
    for &b in bytes {
        if b.is_ascii_alphanumeric() || keep.contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// The `file://` URI of a path, escaped like `g_file_get_uri()`
pub fn file_uri(path: &Path) -> String {
    use std::os::unix::ffi::OsStrExt;
    let path = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    format!(
        "file://{}",
        percent_encode(path.as_os_str().as_bytes(), b"-_.~!$&'()*+,;=:@/")
    )
}

/// The mountable `archive://` URI for an archive file
///
/// The archive's URI is percent-encoded twice, as GVfs expects.
pub fn mountable_archive_uri(path: &Path) -> String {
    let uri = file_uri(path);
    let once = percent_encode(uri.as_bytes(), b"-_.~");
    let twice = percent_encode(once.as_bytes(), b"-_.~");
    format!("archive://{twice}/")
}

/// The command that mounts an archive: `gio mount archive://…`
pub fn mount_archive_command(path: &Path) -> Vec<String> {
    vec!["gio".into(), "mount".into(), mountable_archive_uri(path)]
}

/// The command that unmounts an archive mounted from `path`
pub fn unmount_archive_command(path: &Path) -> Vec<String> {
    vec![
        "gio".into(),
        "mount".into(),
        "-u".into(),
        mountable_archive_uri(path),
    ]
}

/// The GVfs FUSE folder where an archive mount appears, under
/// `$XDG_RUNTIME_DIR/gvfs`; `None` if the runtime folder is unknown
pub fn gvfs_mount_path(path: &Path) -> Option<PathBuf> {
    let runtime = std::env::var_os("XDG_RUNTIME_DIR").filter(|r| !r.is_empty())?;
    let uri = mountable_archive_uri(path);
    let host = uri.strip_prefix("archive://")?.trim_end_matches('/');
    Some(
        PathBuf::from(runtime)
            .join("gvfs")
            .join(format!("archive:host={host}")),
    )
}

/// How an archive was opened as a folder
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ArchiveFolderKind {
    /// Mounted with `gio mount`; unmounted on close
    Mounted,
    /// Already mounted by someone else; left alone on close, as Meld
    /// doesn't track mounts it didn't make
    AlreadyMounted,
    /// Extracted into the temporary folder `temp`; deleted on close
    Extracted {
        /// The temporary folder holding [`ArchiveFolder::root`]
        temp: PathBuf,
    },
}

/// An archive opened as a folder by [`open_archive`]
///
/// The folder is unmounted or deleted by [`ArchiveFolder::close`], or
/// when the value is dropped.
#[derive(Debug)]
pub struct ArchiveFolder {
    /// The archive file
    pub archive: PathBuf,
    /// The folder showing the archive's contents
    pub root: PathBuf,
    /// How the folder was made
    pub kind: ArchiveFolderKind,
    closed: bool,
}

impl ArchiveFolder {
    /// Unmounts or deletes the folder; later calls do nothing
    pub fn close(&mut self) -> Result<(), String> {
        if std::mem::replace(&mut self.closed, true) {
            return Ok(());
        }
        match &self.kind {
            ArchiveFolderKind::Mounted => {
                log::info!("Unmounting archive {}", self.archive.display());
                let output =
                    run_with_timeout(&unmount_archive_command(&self.archive), GIO_TIMEOUT)?;
                if output.status.success() {
                    Ok(())
                } else {
                    Err(format!(
                        "Failed to unmount archive {}: {}",
                        self.archive.display(),
                        String::from_utf8_lossy(&output.stderr).trim()
                    ))
                }
            }
            ArchiveFolderKind::AlreadyMounted => Ok(()),
            ArchiveFolderKind::Extracted { temp } => std::fs::remove_dir_all(temp)
                .map_err(|e| format!("Failed to remove {}: {e}", temp.display())),
        }
    }
}

impl Drop for ArchiveFolder {
    fn drop(&mut self) {
        if let Err(e) = self.close() {
            log::error!("{e}");
        }
    }
}

const GIO_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// Runs a command, killing it if it takes longer than `timeout`
fn run_with_timeout(
    argv: &[String],
    timeout: std::time::Duration,
) -> Result<std::process::Output, String> {
    use std::process::{Command, Stdio};
    let (program, args) = argv.split_first().ok_or("empty command")?;
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("Could not run {program}: {e}"))?;
    let start = std::time::Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return child.wait_with_output().map_err(|e| e.to_string()),
            Ok(None) if start.elapsed() < timeout => {
                std::thread::sleep(std::time::Duration::from_millis(20))
            }
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!(
                    "{program} did not finish in {} seconds",
                    timeout.as_secs()
                ));
            }
            Err(e) => return Err(e.to_string()),
        }
    }
}

/// Opens an archive as a folder, blocking until it is ready
///
/// Like Meld, this mounts the archive with GVfs (`gio mount`) and uses
/// its FUSE folder. Without GVfs the archive is extracted into a
/// temporary folder with `bsdtar`, `unzip` or `tar`, whichever works.
pub fn open_archive(path: &Path) -> Result<ArchiveFolder, String> {
    let archive = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    match mount_with_gio(&archive) {
        Ok(folder) => return Ok(folder),
        Err(e) => log::info!(
            "Could not mount {} with GVfs ({e}); extracting it",
            archive.display()
        ),
    }
    extract_archive(&archive)
}

fn mount_with_gio(archive: &Path) -> Result<ArchiveFolder, String> {
    let fuse = gvfs_mount_path(archive).ok_or("no XDG_RUNTIME_DIR")?;
    let output = run_with_timeout(&mount_archive_command(archive), GIO_TIMEOUT)?;
    let kind = if output.status.success() {
        ArchiveFolderKind::Mounted
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if stderr.to_lowercase().contains("already mounted") {
            ArchiveFolderKind::AlreadyMounted
        } else {
            return Err(stderr.trim().to_string());
        }
    };
    let mut folder = ArchiveFolder {
        archive: archive.to_path_buf(),
        root: fuse.clone(),
        kind,
        closed: false,
    };
    // The FUSE folder can take a moment to appear
    let start = std::time::Instant::now();
    while !fuse.is_dir() {
        if start.elapsed() > std::time::Duration::from_secs(3) {
            let _ = folder.close();
            return Err(format!(
                "the mount's folder {} did not appear",
                fuse.display()
            ));
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    Ok(folder)
}

/// Extracts an archive into a new temporary folder
///
/// The contents end up in a folder named like the archive, inside the
/// temporary folder. The extraction tools refuse absolute paths and
/// `..` components in archive entries.
pub fn extract_archive(archive: &Path) -> Result<ArchiveFolder, String> {
    let temp = tempfile::Builder::new()
        .prefix("meld-archive-")
        .tempdir()
        .map_err(|e| format!("Could not create a temporary folder: {e}"))?;
    let name = archive
        .file_name()
        .map(|n| n.to_os_string())
        .unwrap_or_else(|| "archive".into());
    let root = temp.path().join(name);
    std::fs::create_dir(&root).map_err(|e| format!("Could not create {}: {e}", root.display()))?;

    let archive_arg = archive.to_string_lossy().into_owned();
    let root_arg = root.to_string_lossy().into_owned();
    let content_type = archive_content_type(archive).unwrap_or_default();
    let mut commands: Vec<Vec<String>> = vec![vec![
        "bsdtar".into(),
        "-x".into(),
        "-f".into(),
        archive_arg.clone(),
        "-C".into(),
        root_arg.clone(),
    ]];
    if content_type.contains("zip")
        || content_type.contains("java-archive")
        || content_type.contains("android")
    {
        commands.push(vec![
            "unzip".into(),
            "-qq".into(),
            "-o".into(),
            archive_arg.clone(),
            "-d".into(),
            root_arg.clone(),
        ]);
    }
    if content_type.contains("tar") {
        commands.push(vec![
            "tar".into(),
            "-x".into(),
            "-f".into(),
            archive_arg,
            "-C".into(),
            root_arg,
        ]);
    }

    let mut errors = Vec::new();
    for argv in &commands {
        let output = match std::process::Command::new(&argv[0])
            .args(&argv[1..])
            .stdin(std::process::Stdio::null())
            .output()
        {
            Ok(output) => output,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => {
                errors.push(format!("{}: {e}", argv[0]));
                continue;
            }
        };
        if output.status.success() {
            return Ok(ArchiveFolder {
                archive: archive.to_path_buf(),
                root,
                kind: ArchiveFolderKind::Extracted { temp: temp.keep() },
                closed: false,
            });
        }
        errors.push(format!(
            "{}: {}",
            argv[0],
            String::from_utf8_lossy(&output.stderr).trim()
        ));
        // Start again from an empty folder for the next tool
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::create_dir(&root);
    }
    if errors.is_empty() {
        errors.push("no extraction tool (bsdtar, unzip or tar) found".into());
    }
    Err(format!(
        "Could not extract {}: {}",
        archive.display(),
        errors.join("; ")
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_archives() {
        let dir = tempfile::tempdir().unwrap();
        let zip = dir.path().join("a.zip");
        std::fs::write(&zip, b"PK\x03\x04rest").unwrap();
        let jar = dir.path().join("a.jar");
        std::fs::write(&jar, b"PK\x03\x04rest").unwrap();
        let mut tar = vec![0u8; 512];
        tar[257..262].copy_from_slice(b"ustar");
        let tar_path = dir.path().join("a.tar");
        std::fs::write(&tar_path, &tar).unwrap();
        let tgz = dir.path().join("a.tar.gz");
        std::fs::write(&tgz, b"\x1F\x8B\x08rest").unwrap();
        let text = dir.path().join("a.txt");
        std::fs::write(&text, b"hello").unwrap();

        assert_eq!(archive_content_type(&zip), Some("application/zip"));
        assert_eq!(archive_content_type(&jar), Some("application/java-archive"));
        assert_eq!(archive_content_type(&tar_path), Some("application/x-tar"));
        assert_eq!(
            archive_content_type(&tgz),
            Some("application/x-compressed-tar")
        );
        assert!(is_archive(&zip));
        assert!(!is_archive(&text));
        assert!(!is_archive(dir.path()));
    }

    #[test]
    fn archive_uris() {
        let path = Path::new("/home/me/a b.zip");
        assert_eq!(file_uri(path), "file:///home/me/a%20b.zip");
        assert_eq!(
            mountable_archive_uri(path),
            "archive://file%253A%252F%252F%252Fhome%252Fme%252Fa%252520b.zip/"
        );
        assert_eq!(mount_archive_command(path)[..2], ["gio", "mount"]);
        assert_eq!(unmount_archive_command(path)[2], "-u");
    }

    fn tool(name: &str) -> bool {
        std::process::Command::new(name)
            .arg("--version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok()
    }

    #[test]
    fn extracts_archives_into_temporary_folders() {
        if !tool("tar") {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        std::fs::create_dir_all(src.join("sub")).unwrap();
        std::fs::write(src.join("sub/f.txt"), "hello").unwrap();
        let tar = dir.path().join("t.tar");
        let status = std::process::Command::new("tar")
            .args(["-c", "-f"])
            .arg(&tar)
            .arg("-C")
            .arg(&src)
            .arg("sub")
            .status()
            .unwrap();
        assert!(status.success());
        assert!(is_archive(&tar));

        let mut folder = extract_archive(&tar).unwrap();
        assert_eq!(folder.root.file_name().unwrap(), "t.tar");
        assert_eq!(
            std::fs::read_to_string(folder.root.join("sub/f.txt")).unwrap(),
            "hello"
        );
        let ArchiveFolderKind::Extracted { temp } = folder.kind.clone() else {
            panic!("not extracted");
        };
        assert!(folder.root.starts_with(&temp));
        folder.close().unwrap();
        assert!(!temp.exists());
        folder.close().unwrap();

        // Zip files, when a tool that writes them is around
        if tool("bsdtar") {
            let zip = dir.path().join("z.zip");
            let status = std::process::Command::new("bsdtar")
                .args(["-a", "-c", "-f"])
                .arg(&zip)
                .arg("-C")
                .arg(&src)
                .arg("sub")
                .status()
                .unwrap();
            assert!(status.success());
            assert_eq!(archive_content_type(&zip), Some("application/zip"));
            let folder = extract_archive(&zip).unwrap();
            let root = folder.root.clone();
            assert_eq!(
                std::fs::read_to_string(root.join("sub/f.txt")).unwrap(),
                "hello"
            );
            drop(folder);
            assert!(!root.exists(), "dropping the folder deletes it");
        }

        // Not an archive at all
        let bogus = dir.path().join("bogus.tar");
        std::fs::write(&bogus, "not a tar file").unwrap();
        assert!(extract_archive(&bogus).is_err());
    }

    #[test]
    fn timeouts_kill_commands() {
        let argv: Vec<String> = vec!["sleep".into(), "5".into()];
        let start = std::time::Instant::now();
        if let Err(e) = run_with_timeout(&argv, std::time::Duration::from_millis(100)) {
            assert!(
                e.contains("did not finish") || e.contains("Could not run"),
                "{e}"
            );
        }
        assert!(start.elapsed() < std::time::Duration::from_secs(4));
    }
}
