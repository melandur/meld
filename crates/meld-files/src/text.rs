//! Loading and saving text files
//!
//! Replaces the `GtkSource.FileLoader`/`GtkSource.FileSaver` handling
//! in `meld/filediff.py`, `meld/meldbuffer.py` and
//! `get_custom_encoding_candidates()` in `meld/sourceview.py`.
//!
//! Loading tries, in order: an explicitly requested encoding; a byte
//! order mark; the `detect-encodings` setting; UTF-8; the locale's
//! encoding; and finally the `chardetng` detector (standing in for
//! GtkSourceView's ISO-8859-15 last resort). Text keeps its original
//! line endings; [`Newlines`] records which styles occur. Saving
//! re-encodes with the loaded encoding (and BOM), optionally converting
//! line endings, and replaces the file atomically.

use std::borrow::Cow;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use encoding_rs::{Encoding, UTF_8, UTF_16BE, UTF_16LE};
use thiserror::Error;

/// Lines longer than this many characters stop loading unless forced,
/// since very long lines make text views slow
pub const LINE_LENGTH_LIMIT: usize = 16 * 1024;

/// Number of bytes checked for NUL bytes by [`is_binary`]
pub const BINARY_SAMPLE_SIZE: usize = 8000;

/// A line ending style
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum NewlineType {
    /// `\n`
    Lf,
    /// `\r\n`
    CrLf,
    /// `\r`
    Cr,
}

impl NewlineType {
    /// The line terminator
    pub fn as_str(self) -> &'static str {
        match self {
            NewlineType::Lf => "\n",
            NewlineType::CrLf => "\r\n",
            NewlineType::Cr => "\r",
        }
    }

    /// The (untranslated) name used in Meld's UI, from `const.NEWLINES`
    pub fn label(self) -> &'static str {
        match self {
            NewlineType::Lf => "UNIX (LF)",
            NewlineType::CrLf => "DOS/Windows (CR-LF)",
            NewlineType::Cr => "Mac OS (CR)",
        }
    }

    /// The platform default
    pub fn native() -> NewlineType {
        if cfg!(windows) {
            NewlineType::CrLf
        } else {
            NewlineType::Lf
        }
    }
}

/// The line ending styles present in a text, in order of first use
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct Newlines {
    /// Each style found, once, in order of first occurrence
    pub types: Vec<NewlineType>,
}

impl Newlines {
    /// Find the line endings used in `text`
    pub fn detect(text: &str) -> Newlines {
        let mut types = Vec::new();
        let bytes = text.as_bytes();
        let mut i = 0;
        while i < bytes.len() && types.len() < 3 {
            let found = match bytes[i] {
                b'\n' => Some(NewlineType::Lf),
                b'\r' if bytes.get(i + 1) == Some(&b'\n') => {
                    i += 1;
                    Some(NewlineType::CrLf)
                }
                b'\r' => Some(NewlineType::Cr),
                _ => None,
            };
            if let Some(nl) = found
                && !types.contains(&nl)
            {
                types.push(nl);
            }
            i += 1;
        }
        Newlines { types }
    }

    /// The style used when saving: the first one found, else the
    /// platform default
    pub fn primary(&self) -> NewlineType {
        self.types
            .first()
            .copied()
            .unwrap_or_else(NewlineType::native)
    }

    /// Whether more than one style is used
    pub fn is_mixed(&self) -> bool {
        self.types.len() > 1
    }

    /// A description like "UNIX (LF), DOS/Windows (CR-LF)"
    pub fn label(&self) -> String {
        if self.types.is_empty() {
            return self.primary().label().to_string();
        }
        self.types
            .iter()
            .map(|t| t.label())
            .collect::<Vec<_>>()
            .join(", ")
    }
}

/// A character encoding plus whether the file has a byte order mark
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TextEncoding {
    /// The encoding
    pub encoding: &'static Encoding,
    /// Whether a byte order mark is written before the text
    pub bom: bool,
}

impl TextEncoding {
    /// UTF-8 without a BOM
    pub const UTF8: TextEncoding = TextEncoding {
        encoding: UTF_8,
        bom: false,
    };

    /// Look up an encoding by a charset label such as `"latin1"`,
    /// `"UTF-16LE"` or `"shift_jis"`
    pub fn for_label(label: &str) -> Option<TextEncoding> {
        Encoding::for_label(label.trim().as_bytes()).map(|encoding| TextEncoding {
            encoding,
            bom: false,
        })
    }

    /// The encoding's canonical name, e.g. `"UTF-8"` or `"windows-1252"`
    pub fn name(&self) -> &'static str {
        self.encoding.name()
    }

    fn bom_bytes(&self) -> &'static [u8] {
        if !self.bom {
            return b"";
        }
        if self.encoding == UTF_8 {
            b"\xEF\xBB\xBF"
        } else if self.encoding == UTF_16LE {
            b"\xFF\xFE"
        } else if self.encoding == UTF_16BE {
            b"\xFE\xFF"
        } else {
            b""
        }
    }

    fn is_utf16(&self) -> bool {
        self.encoding == UTF_16LE || self.encoding == UTF_16BE
    }
}

impl Default for TextEncoding {
    fn default() -> Self {
        TextEncoding::UTF8
    }
}

/// How to decode a file
#[derive(Clone, Debug, Default)]
pub struct LoadOptions {
    /// The `detect-encodings` setting: charsets tried before the
    /// defaults. Invalid names are skipped with a warning.
    pub detect_encodings: Vec<String>,
    /// Decode with exactly this encoding ("reload with encoding")
    pub encoding: Option<&'static Encoding>,
    /// Fail with [`LoadError::LineTooLong`] for lines longer than
    /// [`LINE_LENGTH_LIMIT`]; Meld's default unless the user forces
    /// loading
    pub check_line_length: bool,
}

/// A decoded text file
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LoadedText {
    /// The text, without BOM, with the file's original line endings
    pub text: String,
    /// The encoding used to decode the text
    pub encoding: TextEncoding,
    /// The line endings found
    pub newlines: Newlines,
    /// Whether the file looks binary (NUL bytes in a non-UTF-16 file)
    pub binary: bool,
    /// Whether undecodable bytes were replaced with U+FFFD; saving
    /// such a text would change the file
    pub lossy: bool,
    /// Whether the encoding came from the last-resort detector rather
    /// than a candidate that decoded cleanly
    pub guessed: bool,
}

impl LoadedText {
    /// Whether the text ends with a line terminator
    pub fn has_trailing_newline(&self) -> bool {
        self.text.ends_with(['\n', '\r'])
    }

    /// The text as lines without terminators, as a text buffer with an
    /// implicit trailing newline sees it (`"a\nb\n"` is `["a", "b"]`)
    pub fn lines(&self) -> Vec<String> {
        split_lines(strip_trailing_newline(&self.text))
    }
}

/// Remove one trailing line terminator, if any
pub fn strip_trailing_newline(text: &str) -> &str {
    text.strip_suffix("\r\n")
        .or_else(|| text.strip_suffix('\n'))
        .or_else(|| text.strip_suffix('\r'))
        .unwrap_or(text)
}

/// Split text on `\n`, `\r\n` and `\r`; `""` gives one empty line
pub fn split_lines(text: &str) -> Vec<String> {
    let mut lines = Vec::new();
    let mut start = 0;
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'\n' => {
                lines.push(text[start..i].to_string());
                start = i + 1;
            }
            b'\r' => {
                lines.push(text[start..i].to_string());
                if bytes.get(i + 1) == Some(&b'\n') {
                    i += 1;
                }
                start = i + 1;
            }
            _ => {}
        }
        i += 1;
    }
    lines.push(text[start..].to_string());
    lines
}

/// Join lines with a terminator, optionally adding a final one
pub fn join_lines<S: AsRef<str>>(
    lines: &[S],
    newline: NewlineType,
    trailing_newline: bool,
) -> String {
    let mut out = lines
        .iter()
        .map(AsRef::as_ref)
        .collect::<Vec<_>>()
        .join(newline.as_str());
    if trailing_newline {
        out.push_str(newline.as_str());
    }
    out
}

/// Convert every line ending in `text` to `newline`
pub fn convert_newlines(text: &str, newline: NewlineType) -> Cow<'_, str> {
    let current = Newlines::detect(text);
    if current.types.is_empty() || current.types == [newline] {
        return Cow::Borrowed(text);
    }
    let lines = split_lines(text);
    Cow::Owned(lines.join(newline.as_str()))
}

/// Rough test for binary data: a NUL byte in the first
/// [`BINARY_SAMPLE_SIZE`] bytes
pub fn is_binary(data: &[u8]) -> bool {
    data[..data.len().min(BINARY_SAMPLE_SIZE)].contains(&0)
}

/// Errors from loading files
#[derive(Debug, Error)]
pub enum LoadError {
    /// The file couldn't be read
    #[error("There was a problem opening the file “{}”: {source}", .path.display())]
    Io {
        /// The file being loaded
        path: PathBuf,
        /// The underlying error
        source: io::Error,
    },
    /// A line is longer than [`LINE_LENGTH_LIMIT`]
    #[error(
        "Line {} exceeded maximum line length ({length} > {LINE_LENGTH_LIMIT}).\n\n\
         Loading files with very long lines may make Meld slow or unresponsive.",
        .line + 1
    )]
    LineTooLong {
        /// The 0-based line number
        line: usize,
        /// The line's length in characters
        length: usize,
    },
}

/// The charset of the current locale, from `LC_ALL`, `LC_CTYPE` or
/// `LANG` (e.g. `ISO-8859-1` for `de_DE.ISO-8859-1@euro`)
pub fn locale_encoding() -> Option<&'static Encoding> {
    let value = ["LC_ALL", "LC_CTYPE", "LANG"]
        .iter()
        .filter_map(|var| std::env::var(var).ok())
        .find(|v| !v.is_empty())?;
    let charset = value.split('.').nth(1)?.split('@').next()?;
    Encoding::for_label(charset.as_bytes())
}

/// The ordered candidate encodings for `detect-encodings`
///
/// As in GtkSourceView, the user's encodings come first, followed by
/// UTF-8 and the locale encoding. Invalid charsets are skipped.
pub fn candidate_encodings(detect_encodings: &[String]) -> Vec<&'static Encoding> {
    let mut candidates: Vec<&'static Encoding> = Vec::new();
    for charset in detect_encodings {
        match Encoding::for_label(charset.trim().as_bytes()) {
            Some(enc) => candidates.push(enc),
            None => log::warn!("Invalid charset \"{charset}\" skipped"),
        }
    }
    candidates.push(UTF_8);
    candidates.extend(locale_encoding());
    let mut unique = Vec::new();
    for enc in candidates {
        if !unique.contains(&enc) {
            unique.push(enc);
        }
    }
    unique
}

fn finish(
    text: String,
    encoding: TextEncoding,
    lossy: bool,
    guessed: bool,
    data: &[u8],
) -> LoadedText {
    let newlines = Newlines::detect(&text);
    let binary = !encoding.is_utf16() && is_binary(data);
    LoadedText {
        text,
        encoding,
        newlines,
        binary,
        lossy,
        guessed,
    }
}

/// Decode file contents; see the module docs for the detection order
pub fn decode_text(data: &[u8], options: &LoadOptions) -> LoadedText {
    let bom = Encoding::for_bom(data);

    if let Some(forced) = options.encoding {
        let (encoding, body) = match bom {
            Some((enc, len)) if enc == forced => (
                TextEncoding {
                    encoding: forced,
                    bom: true,
                },
                &data[len..],
            ),
            _ => (
                TextEncoding {
                    encoding: forced,
                    bom: false,
                },
                data,
            ),
        };
        let (text, lossy) = forced.decode_without_bom_handling(body);
        return finish(text.into_owned(), encoding, lossy, false, data);
    }

    if let Some((enc, len)) = bom {
        let (text, lossy) = enc.decode_without_bom_handling(&data[len..]);
        return finish(
            text.into_owned(),
            TextEncoding {
                encoding: enc,
                bom: true,
            },
            lossy,
            false,
            data,
        );
    }

    for enc in candidate_encodings(&options.detect_encodings) {
        if let Some(text) = enc.decode_without_bom_handling_and_without_replacement(data) {
            return finish(
                text.into_owned(),
                TextEncoding {
                    encoding: enc,
                    bom: false,
                },
                false,
                false,
                data,
            );
        }
    }

    let mut detector = chardetng::EncodingDetector::new();
    detector.feed(data, true);
    let enc = detector.guess(None, true);
    let (text, lossy) = enc.decode_without_bom_handling(data);
    finish(
        text.into_owned(),
        TextEncoding {
            encoding: enc,
            bom: false,
        },
        lossy,
        true,
        data,
    )
}

/// Find the first line longer than `limit` characters
pub fn find_long_line(text: &str, limit: usize) -> Option<(usize, usize)> {
    split_lines(text)
        .iter()
        .enumerate()
        .map(|(i, line)| (i, line.chars().count()))
        .find(|(_, len)| *len > limit)
}

/// Read and decode a text file
pub fn load_text_file(path: &Path, options: &LoadOptions) -> Result<LoadedText, LoadError> {
    let data = fs::read(path).map_err(|source| LoadError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let loaded = decode_text(&data, options);
    if options.check_line_length
        && let Some((line, length)) = find_long_line(&loaded.text, LINE_LENGTH_LIMIT)
    {
        return Err(LoadError::LineTooLong { line, length });
    }
    Ok(loaded)
}

/// Text that can't be represented in the target encoding
#[derive(Debug, Error, Clone, PartialEq, Eq)]
#[error("Text contains characters that can’t be encoded using its current encoding “{encoding}”")]
pub struct EncodeError {
    /// The encoding's name
    pub encoding: &'static str,
    /// The first character that couldn't be encoded
    pub character: char,
    /// The byte offset of that character in the text
    pub offset: usize,
}

/// Encode text, adding the BOM if the encoding has one and optionally
/// converting line endings
pub fn encode_text(
    text: &str,
    encoding: TextEncoding,
    newline: Option<NewlineType>,
) -> Result<Vec<u8>, EncodeError> {
    let text = match newline {
        Some(nl) => convert_newlines(text, nl),
        None => Cow::Borrowed(text),
    };
    let mut out = encoding.bom_bytes().to_vec();
    if encoding.encoding == UTF_16LE {
        out.extend(text.encode_utf16().flat_map(u16::to_le_bytes));
    } else if encoding.encoding == UTF_16BE {
        out.extend(text.encode_utf16().flat_map(u16::to_be_bytes));
    } else {
        let (bytes, _used, had_errors) = encoding.encoding.encode(&text);
        if had_errors {
            let (offset, character) = text
                .char_indices()
                .find(|(_, c)| encoding.encoding.encode(c.encode_utf8(&mut [0; 4])).2)
                .unwrap_or((0, '\u{FFFD}'));
            return Err(EncodeError {
                encoding: encoding.name(),
                character,
                offset,
            });
        }
        out.extend_from_slice(&bytes);
    }
    Ok(out)
}

/// How to save a text file
#[derive(Clone, Debug, Default)]
pub struct SaveOptions {
    /// Encoding (and BOM) to save with; normally the loaded encoding
    pub encoding: TextEncoding,
    /// Convert all line endings to this style; `None` keeps the text's
    pub newline: Option<NewlineType>,
    /// Copy the existing file to `<name>~` before replacing it
    pub backup: bool,
    /// The modification time seen when the file was loaded or last
    /// saved; saving fails with [`SaveError::ExternallyModified`] if the
    /// file has changed since. `None` skips the check.
    pub expected_mtime: Option<SystemTime>,
}

/// Errors from saving files
#[derive(Debug, Error)]
pub enum SaveError {
    /// The file changed on disk since it was loaded
    #[error("File {} has changed on disk since it was opened", .path.display())]
    ExternallyModified {
        /// The file being saved
        path: PathBuf,
    },
    /// The text can't be encoded
    #[error(transparent)]
    Encode(#[from] EncodeError),
    /// Writing failed
    #[error("Couldn’t save file due to:\n{source}")]
    Io {
        /// The file being saved
        path: PathBuf,
        /// The underlying error
        source: io::Error,
    },
}

/// Encode and save text, returning the file's new modification time
pub fn save_text_file(
    path: &Path,
    text: &str,
    options: &SaveOptions,
) -> Result<Option<SystemTime>, SaveError> {
    if let Some(expected) = options.expected_mtime
        && file_mtime(path).is_some_and(|actual| actual != expected)
    {
        return Err(SaveError::ExternallyModified {
            path: path.to_path_buf(),
        });
    }
    let bytes = encode_text(text, options.encoding, options.newline)?;
    write_atomically(path, &bytes, options.backup).map_err(|source| SaveError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    Ok(file_mtime(path))
}

/// The modification time of a file, following symlinks
pub fn file_mtime(path: &Path) -> Option<SystemTime> {
    fs::metadata(path).and_then(|m| m.modified()).ok()
}

/// Whether a file was modified on disk after `seen`, as
/// `MeldBufferData.current_on_disk()` checks
pub fn modified_since(path: &Path, seen: Option<SystemTime>) -> bool {
    match (seen, file_mtime(path)) {
        (Some(seen), Some(now)) => now > seen,
        _ => false,
    }
}

/// Replace a file's contents atomically
///
/// Symlinks are followed so that the link survives. The data is written
/// to a temporary file in the same folder, given the original's
/// permissions, synced and renamed over the original. If that isn't
/// possible (e.g., the folder isn't writable, or the file is owned by
/// someone else) the file is overwritten in place instead, as GIO does.
pub fn write_atomically(path: &Path, data: &[u8], backup: bool) -> io::Result<()> {
    let target = match fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => fs::canonicalize(path)?,
        _ => path.to_path_buf(),
    };
    let existing = fs::metadata(&target).ok();
    if let Some(meta) = &existing
        && meta.is_dir()
    {
        return Err(io::Error::new(io::ErrorKind::IsADirectory, "is a folder"));
    }

    if backup && existing.is_some() {
        let mut backup_name = target.clone().into_os_string();
        backup_name.push("~");
        fs::copy(&target, PathBuf::from(backup_name))?;
    }

    let same_owner = existing.as_ref().is_none_or(|meta| {
        use std::os::unix::fs::MetadataExt;
        // SAFETY: geteuid() has no preconditions and cannot fail
        meta.uid() == unsafe { libc::geteuid() }
    });

    if same_owner && let Ok(()) = replace_via_temp(&target, data, existing.as_ref()) {
        return Ok(());
    }

    let mut file = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(&target)?;
    file.write_all(data)?;
    file.sync_all()
}

fn replace_via_temp(target: &Path, data: &[u8], existing: Option<&fs::Metadata>) -> io::Result<()> {
    let dir = match target.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => Path::new("."),
    };
    let name = target
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut temp = tempfile::Builder::new()
        .prefix(&format!(".{name}."))
        .suffix(".tmp")
        .tempfile_in(dir)?;
    temp.write_all(data)?;
    if let Some(meta) = existing {
        temp.as_file().set_permissions(meta.permissions())?;
    }
    temp.as_file().sync_all()?;
    temp.persist(target).map_err(|e| e.error)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_newlines() {
        assert_eq!(Newlines::detect("a\nb\n").types, vec![NewlineType::Lf]);
        assert_eq!(
            Newlines::detect("a\r\nb\r\n").types,
            vec![NewlineType::CrLf]
        );
        assert_eq!(Newlines::detect("a\rb").types, vec![NewlineType::Cr]);
        let mixed = Newlines::detect("a\r\nb\nc\r");
        assert_eq!(
            mixed.types,
            vec![NewlineType::CrLf, NewlineType::Lf, NewlineType::Cr]
        );
        assert!(mixed.is_mixed());
        assert_eq!(mixed.primary(), NewlineType::CrLf);
        assert_eq!(mixed.label(), "DOS/Windows (CR-LF), UNIX (LF), Mac OS (CR)");
        assert!(Newlines::detect("abc").types.is_empty());
    }

    #[test]
    fn lines_split_and_join() {
        assert_eq!(split_lines("a\r\nb\rc\nd"), vec!["a", "b", "c", "d"]);
        assert_eq!(split_lines(""), vec![""]);
        assert_eq!(split_lines("a\n"), vec!["a", ""]);
        assert_eq!(
            join_lines(&["a", "b"], NewlineType::CrLf, true),
            "a\r\nb\r\n"
        );
        assert_eq!(convert_newlines("a\r\nb\nc", NewlineType::Lf), "a\nb\nc");
    }

    #[test]
    fn decodes_utf8_and_bom() {
        let options = LoadOptions::default();
        let plain = decode_text("héllo\n".as_bytes(), &options);
        assert_eq!(plain.text, "héllo\n");
        assert_eq!(plain.encoding, TextEncoding::UTF8);
        assert!(!plain.binary && !plain.lossy && !plain.guessed);
        assert_eq!(plain.lines(), vec!["héllo"]);

        let bom = decode_text(b"\xEF\xBB\xBFabc", &options);
        assert_eq!(bom.text, "abc");
        assert!(bom.encoding.bom);
        assert_eq!(
            encode_text(&bom.text, bom.encoding, None).unwrap(),
            b"\xEF\xBB\xBFabc"
        );

        let utf16 = decode_text(b"\xFF\xFEa\x00\n\x00", &options);
        assert_eq!(utf16.text, "a\n");
        assert_eq!(utf16.encoding.encoding, UTF_16LE);
        assert!(!utf16.binary);
        assert_eq!(
            encode_text("a\n", utf16.encoding, None).unwrap(),
            b"\xFF\xFEa\x00\n\x00"
        );
    }

    #[test]
    fn uses_candidates_then_detector() {
        let latin1 = b"caf\xe9\n";
        let options = LoadOptions {
            detect_encodings: vec!["ISO-8859-15".into()],
            ..Default::default()
        };
        let loaded = decode_text(latin1, &options);
        assert_eq!(loaded.text, "café\n");
        assert_eq!(loaded.encoding.name(), "ISO-8859-15");
        assert!(!loaded.guessed);

        let options = LoadOptions {
            detect_encodings: vec!["not-a-charset".into()],
            ..Default::default()
        };
        let guessed = decode_text(latin1, &options);
        assert!(guessed.guessed);
        assert_eq!(guessed.text, "café\n");

        let forced = LoadOptions {
            encoding: Some(encoding_rs::WINDOWS_1252),
            ..Default::default()
        };
        assert_eq!(decode_text("é".as_bytes(), &forced).text, "Ã©");
    }

    #[test]
    fn detects_binary() {
        let loaded = decode_text(b"\x00\x01\x02abc", &LoadOptions::default());
        assert!(loaded.binary);
        assert!(!is_binary(b"plain text"));
    }

    #[test]
    fn encode_errors() {
        let latin = TextEncoding::for_label("iso-8859-15").unwrap();
        assert_eq!(encode_text("café", latin, None).unwrap(), b"caf\xe9");
        let err =
            encode_text("a€b✓", TextEncoding::for_label("iso-8859-2").unwrap(), None).unwrap_err();
        assert_eq!(err.character, '€');
        assert_eq!(err.offset, 1);
    }

    #[test]
    fn load_and_save_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f.txt");
        fs::write(&path, b"one\r\ntwo\r\n").unwrap();
        let loaded = load_text_file(&path, &LoadOptions::default()).unwrap();
        assert_eq!(loaded.newlines.primary(), NewlineType::CrLf);
        let mtime = file_mtime(&path);

        let options = SaveOptions {
            encoding: loaded.encoding,
            newline: Some(loaded.newlines.primary()),
            backup: true,
            expected_mtime: mtime,
        };
        save_text_file(&path, "one\ntwo\nthree\n", &options).unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"one\r\ntwo\r\nthree\r\n");
        assert_eq!(
            fs::read(dir.path().join("f.txt~")).unwrap(),
            b"one\r\ntwo\r\n"
        );

        // An outdated mtime is reported as an external modification
        let stale = SaveOptions {
            expected_mtime: Some(SystemTime::UNIX_EPOCH),
            ..Default::default()
        };
        assert!(matches!(
            save_text_file(&path, "x", &stale),
            Err(SaveError::ExternallyModified { .. })
        ));
    }

    #[test]
    fn save_preserves_symlinks_and_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real.txt");
        fs::write(&real, b"old").unwrap();
        fs::set_permissions(&real, fs::Permissions::from_mode(0o640)).unwrap();
        let link = dir.path().join("link.txt");
        std::os::unix::fs::symlink(&real, &link).unwrap();

        write_atomically(&link, b"new", false).unwrap();
        assert!(
            fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(fs::read(&real).unwrap(), b"new");
        assert_eq!(
            fs::metadata(&real).unwrap().permissions().mode() & 0o777,
            0o640
        );
    }

    #[test]
    fn long_lines() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("long.txt");
        let text = format!("short\n{}\n", "x".repeat(LINE_LENGTH_LIMIT + 1));
        fs::write(&path, &text).unwrap();
        let options = LoadOptions {
            check_line_length: true,
            ..Default::default()
        };
        match load_text_file(&path, &options) {
            Err(LoadError::LineTooLong { line, length }) => {
                assert_eq!((line, length), (1, LINE_LENGTH_LIMIT + 1));
            }
            other => panic!("unexpected {other:?}"),
        }
        assert!(load_text_file(&path, &LoadOptions::default()).is_ok());
    }
}
