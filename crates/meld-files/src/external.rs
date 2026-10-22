//! Opening files in external applications
//!
//! Port of `meld/externalhelpers.py`. Rather than launching through
//! GTK/GIO, these functions return command lines for the app to spawn
//! (see [`spawn_detached`]).

use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use thiserror::Error;

use crate::shlex;

/// The settings that control opening files externally
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExternalEditorSettings {
    /// `use-system-editor`
    pub use_system_editor: bool,
    /// `custom-editor-command`, with `{file}` and `{line}` tokens
    pub custom_editor_command: String,
}

impl Default for ExternalEditorSettings {
    fn default() -> Self {
        ExternalEditorSettings {
            use_system_editor: true,
            custom_editor_command: String::new(),
        }
    }
}

/// Errors from opening files externally
#[derive(Debug, Error)]
pub enum ExternalError {
    /// The path isn't a regular file or folder
    #[error("External opening of files of type “{file_type}” is not supported")]
    UnsupportedFileType {
        /// The path
        path: PathBuf,
        /// A description of the file type
        file_type: &'static str,
    },
    /// The path couldn't be inspected
    #[error("Couldn’t open {}: {source}", .path.display())]
    Io {
        /// The path
        path: PathBuf,
        /// The underlying error
        source: io::Error,
    },
}

/// Parse the replacement field names of a Python format string;
/// `None` if the braces are unbalanced
fn format_fields(template: &str) -> Option<Vec<String>> {
    let mut fields = Vec::new();
    let mut chars = template.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '{' if chars.peek() == Some(&'{') => {
                chars.next();
            }
            '{' => {
                let mut field = String::new();
                loop {
                    match chars.next()? {
                        '}' => break,
                        c => field.push(c),
                    }
                }
                let name = field.split([':', '!']).next().unwrap_or("").to_string();
                fields.push(name);
            }
            '}' if chars.peek() == Some(&'}') => {
                chars.next();
            }
            '}' => return None,
            _ => {}
        }
    }
    Some(fields)
}

/// Substitute `{file}` and `{line}` in a Python format string
fn format_template(template: &str, file: &str, line: usize) -> String {
    let mut out = String::new();
    let mut chars = template.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '{' if chars.peek() == Some(&'{') => {
                chars.next();
                out.push('{');
            }
            '}' if chars.peek() == Some(&'}') => {
                chars.next();
                out.push('}');
            }
            '{' => {
                let mut field = String::new();
                for c in chars.by_ref() {
                    if c == '}' {
                        break;
                    }
                    field.push(c);
                }
                match field.split([':', '!']).next().unwrap_or("") {
                    "file" => out.push_str(file),
                    "line" => out.push_str(&line.to_string()),
                    _ => {}
                }
            }
            c => out.push(c),
        }
    }
    out
}

/// Build the command line for the custom editor
///
/// `template` is `custom-editor-command`; `{file}` is replaced by the
/// shell-quoted path and `{line}` by the line number, and the result is
/// split like a shell command. As in Meld, a template without tokens
/// (or with unsupported ones) gives `[template, path]`.
pub fn make_custom_editor_command(template: &str, path: &Path, line: usize) -> Vec<String> {
    let path_str = path.to_string_lossy().into_owned();
    let fallback = || vec![template.to_string(), path_str.clone()];
    let Some(fields) = format_fields(template) else {
        log::error!("Unsupported fields found");
        return fallback();
    };
    if fields.iter().all(String::is_empty) {
        return fallback();
    }
    if !fields.iter().all(|f| f == "file" || f == "line") {
        log::error!("Unsupported fields found");
        return fallback();
    }
    let cmd = format_template(template, &shlex::quote(&path_str), line);
    shlex::split(&cmd).unwrap_or_else(fallback)
}

/// The command that opens a path with the desktop's default handler
pub fn default_open_command(path: &Path) -> Vec<String> {
    let path = path.to_string_lossy().into_owned();
    if cfg!(target_os = "macos") {
        vec!["open".into(), path]
    } else if cfg!(windows) {
        vec![
            "cmd".into(),
            "/C".into(),
            "start".into(),
            String::new(),
            path,
        ]
    } else {
        vec!["xdg-open".into(), path]
    }
}

/// The command that shows a path's containing folder
pub fn open_containing_folder_command(path: &Path) -> Vec<String> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    default_open_command(parent)
}

/// Rough text check standing in for `g_content_type_is_a(type,
/// "text/plain")`: no NUL bytes in the first 8 KiB. Unreadable files
/// are assumed to be text, as Meld does for unknown content types.
fn looks_like_text(path: &Path) -> bool {
    use std::io::Read;
    let mut buf = Vec::with_capacity(crate::text::BINARY_SAMPLE_SIZE);
    match std::fs::File::open(path) {
        Ok(f) => {
            if f.take(crate::text::BINARY_SAMPLE_SIZE as u64)
                .read_to_end(&mut buf)
                .is_err()
            {
                return true;
            }
            !crate::text::is_binary(&buf)
        }
        Err(_) => true,
    }
}

/// The command line for opening a file externally; `open_cb()`
///
/// Folders (or any path when `open_parent` is set) open in the file
/// manager. Text files open in the system editor or the custom editor
/// at `line`, depending on the settings; other files open with their
/// default handler.
pub fn open_external_command(
    path: &Path,
    line: usize,
    settings: &ExternalEditorSettings,
    open_parent: bool,
) -> Result<Vec<String>, ExternalError> {
    let meta = std::fs::metadata(path).map_err(|source| ExternalError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    if open_parent {
        return Ok(open_containing_folder_command(path));
    }
    if meta.is_dir() {
        return Ok(default_open_command(path));
    }
    if !meta.is_file() {
        use std::os::unix::fs::FileTypeExt;
        let ft = meta.file_type();
        let file_type =
            if ft.is_fifo() || ft.is_socket() || ft.is_block_device() || ft.is_char_device() {
                "special"
            } else {
                "unknown"
            };
        return Err(ExternalError::UnsupportedFileType {
            path: path.to_path_buf(),
            file_type,
        });
    }
    if looks_like_text(path)
        && !settings.use_system_editor
        && !settings.custom_editor_command.trim().is_empty()
    {
        return Ok(make_custom_editor_command(
            &settings.custom_editor_command,
            path,
            line,
        ));
    }
    Ok(default_open_command(path))
}

/// Spawn a command without waiting for it, with stdio detached
pub fn spawn_detached(argv: &[String]) -> io::Result<()> {
    let (program, args) = argv
        .split_first()
        .ok_or_else(|| io::Error::other("empty command"))?;
    Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn custom_editor_templates() {
        let path = Path::new("/tmp/a file.txt");
        assert_eq!(
            make_custom_editor_command("gedit +{line} {file}", path, 12),
            vec!["gedit", "+12", "/tmp/a file.txt"]
        );
        assert_eq!(
            make_custom_editor_command("code -g {file}:{line}", path, 3),
            vec!["code", "-g", "/tmp/a file.txt:3"]
        );
        // No tokens: the command is used as-is, with the path appended
        assert_eq!(
            make_custom_editor_command("my editor", path, 1),
            vec!["my editor", "/tmp/a file.txt"]
        );
        // Unsupported tokens fall back the same way
        assert_eq!(
            make_custom_editor_command("ed {nope} {file}", path, 1),
            vec!["ed {nope} {file}", "/tmp/a file.txt"]
        );
        assert_eq!(
            make_custom_editor_command("ed {file", path, 1),
            vec!["ed {file", "/tmp/a file.txt"]
        );
        assert_eq!(
            make_custom_editor_command("ed {{x}} {file}", path, 1),
            vec!["ed", "{x}", "/tmp/a file.txt"]
        );
        assert_eq!(
            make_custom_editor_command("vim +{line:d} {file!s}", Path::new("/x"), 7),
            vec!["vim", "+7", "/x"]
        );
    }

    #[test]
    fn open_commands() {
        let dir = tempfile::tempdir().unwrap();
        let text = dir.path().join("t.txt");
        std::fs::write(&text, "hello").unwrap();
        let bin = dir.path().join("b.bin");
        std::fs::write(&bin, b"\0\x01").unwrap();

        let system = ExternalEditorSettings::default();
        let custom = ExternalEditorSettings {
            use_system_editor: false,
            custom_editor_command: "ed +{line} {file}".into(),
        };
        let path_str = |p: &Path| p.to_string_lossy().into_owned();

        assert_eq!(
            open_external_command(&text, 4, &system, false).unwrap(),
            default_open_command(&text)
        );
        assert_eq!(
            open_external_command(&text, 4, &custom, false).unwrap(),
            vec!["ed".to_string(), "+4".to_string(), path_str(&text)]
        );
        assert_eq!(
            open_external_command(&bin, 4, &custom, false).unwrap(),
            default_open_command(&bin)
        );
        assert_eq!(
            open_external_command(dir.path(), 0, &custom, false).unwrap(),
            default_open_command(dir.path())
        );
        assert_eq!(
            open_external_command(&text, 0, &custom, true).unwrap(),
            default_open_command(dir.path())
        );
        assert!(open_external_command(&dir.path().join("missing"), 0, &system, false).is_err());
    }
}
