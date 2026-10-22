//! Comparing the contents of archives
//!
//! Port of `meld/archivehelpers.py`. Meld mounts archives with GVfs'
//! `archive://` backend and compares the mounted folders. Without GIO,
//! this module detects archives, builds the `archive://` URIs and the
//! `gio mount` command lines to mount and unmount them, locates the
//! GVfs FUSE folder of a mount, and keeps a registry of mounts made by
//! the app ([`ArchiveMounts`]) so that paths inside them can be shown
//! as the original archive.

use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};

/// Content types that the GVfs `archive://` backend should handle
pub const ARCHIVE_CONTENT_TYPES: &[&str] = &[
    "application/java-archive",
    "application/vnd.android.package-archive",
    "application/vnd.ms-cab-compressed",
    "application/x-7z-compressed",
    "application/x-archive",
    "application/x-bzip-compressed-tar",
    "application/x-cd-image",
    "application/x-compressed-tar",
    "application/x-cpio",
    "application/x-deb",
    "application/x-iso9660-image",
    "application/x-lha",
    "application/x-lzh-compressed",
    "application/x-lzma-compressed-tar",
    "application/x-rar",
    "application/x-rar-compressed",
    "application/x-rpm",
    "application/x-stuffit",
    "application/x-tar",
    "application/x-tarz",
    "application/x-xar",
    "application/x-xz-compressed-tar",
    "application/x-zip",
    "application/x-zip-compressed",
    "application/zip",
];

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

/// Archives mounted by the app, keyed by mount root
#[derive(Clone, Debug, Default)]
pub struct ArchiveMounts {
    mounts: HashMap<PathBuf, PathBuf>,
}

impl ArchiveMounts {
    /// An empty registry
    pub fn new() -> ArchiveMounts {
        ArchiveMounts::default()
    }

    /// Record that `archive` was mounted at `root`
    pub fn insert(&mut self, root: PathBuf, archive: PathBuf) {
        self.mounts.insert(root, archive);
    }

    /// Forget a mount, returning its archive
    pub fn remove(&mut self, root: &Path) -> Option<PathBuf> {
        self.mounts.remove(root)
    }

    /// Whether any mounts are registered
    pub fn have_active_mounts(&self) -> bool {
        !self.mounts.is_empty()
    }

    /// The registered mount roots
    pub fn roots(&self) -> impl Iterator<Item = &Path> {
        self.mounts.keys().map(PathBuf::as_path)
    }

    /// The mount root containing `path`, if any
    pub fn mount_for_path(&self, path: &Path) -> Option<&Path> {
        self.mounts
            .keys()
            .find(|root| path.starts_with(root))
            .map(PathBuf::as_path)
    }

    /// The original archive for a path inside a mounted archive;
    /// `format_archive_path()`
    pub fn format_archive_path(&self, path: &Path) -> Option<&Path> {
        self.mount_for_path(path)
            .and_then(|root| self.mounts.get(root))
            .map(PathBuf::as_path)
    }

    /// Whether `path` is the root of a mounted archive
    pub fn is_mounted_archive_root(&self, path: &Path) -> bool {
        self.mounts.contains_key(path)
    }
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
        for t in [&zip, &jar, &tar_path, &tgz].map(|p| archive_content_type(p).unwrap()) {
            assert!(ARCHIVE_CONTENT_TYPES.contains(&t));
        }
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

    #[test]
    fn mount_registry() {
        let mut mounts = ArchiveMounts::new();
        assert!(!mounts.have_active_mounts());
        mounts.insert(PathBuf::from("/run/gvfs/x"), PathBuf::from("/home/a.zip"));
        assert!(mounts.is_mounted_archive_root(Path::new("/run/gvfs/x")));
        assert_eq!(
            mounts.format_archive_path(Path::new("/run/gvfs/x/inner/f")),
            Some(Path::new("/home/a.zip"))
        );
        assert_eq!(mounts.format_archive_path(Path::new("/run/gvfs/y")), None);
        assert_eq!(
            mounts.remove(Path::new("/run/gvfs/x")),
            Some(PathBuf::from("/home/a.zip"))
        );
    }
}
