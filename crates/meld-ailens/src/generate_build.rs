//! Writing a build file and the folder's `index.md` (port of
//! `generate_build.py`)
//!
//! The caller gathers the facts and asks for the narrative
//! ([`crate::build_narrative::write_narrative`]); this module only writes
//! them. Builds cover commits only, so the caller must not pass facts for
//! an uncommitted target.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::build_facts::BuildFacts;
use crate::build_file::{build_file_name, render_build, render_index};
use crate::build_index::{is_build_file_name, read_build_headers, resolve_build_folder};
use crate::build_narrative::BuildNarrative;
use crate::error::{Error, Result};

pub const INDEX_FILE_NAME: &str = "index.md";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GeneratedBuild {
    pub path: PathBuf,
}

/// Writes `<base>_<target>_build.md` and rebuilds `index.md`
///
/// `folder` is the build folder relative to `repo_root` (see
/// [`resolve_build_folder`]); it is created if needed. `generator` names the
/// writer, e.g. `meld-ailens@3.23.0`; `created` is seconds since the epoch.
/// The same facts, narrative and `created` always give the same bytes; an
/// existing build for the same pair is replaced.
///
/// Each file is written to a temporary file and renamed into place, so a
/// failure never leaves a half-written build.
pub fn write_build(
    repo_root: &Path,
    folder: &str,
    facts: &BuildFacts,
    narrative: &BuildNarrative,
    generator: &str,
    created: f64,
) -> Result<GeneratedBuild> {
    let folder_path = resolve_build_folder(repo_root, folder)?;
    let name = build_file_name(facts);
    if !is_build_file_name(&name) {
        return Err(Error::msg(format!("Cannot name a build file {name:?}.")));
    }

    std::fs::create_dir_all(&folder_path).map_err(|err| {
        Error::msg(format!(
            "Could not create the build folder {}: {}",
            folder_path.display(),
            describe(&err)
        ))
    })?;

    let path = folder_path.join(&name);
    write_text_atomic(&path, &render_build(facts, narrative, created, generator))?;

    let index_path = folder_path.join(INDEX_FILE_NAME);
    write_text_atomic(
        &index_path,
        &render_index(&read_build_headers(&folder_path)?),
    )?;

    Ok(GeneratedBuild { path })
}

fn describe(err: &std::io::Error) -> String {
    let text = err.to_string();
    match text.rfind(" (os error ") {
        Some(index) => text[..index].to_string(),
        None => text,
    }
}

fn random_hex() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or_default();
    let count = COUNTER.fetch_add(1, Ordering::Relaxed);
    let mixed = (u64::from(nanos) ^ count.wrapping_mul(0x9E37_79B9_7F4A_7C15)) as u32;
    format!("{mixed:08x}")
}

/// Writes `text` as UTF-8 to `path` through a temporary file
///
/// The temporary file sits next to `path` and is renamed over it, so
/// readers see the old file or the new one, never a partial one. New files
/// get the usual permissions (`0o666` less the umask).
pub fn write_text_atomic(path: &Path, text: &str) -> Result<()> {
    let directory = path.parent().unwrap_or(Path::new("."));
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let temp_path = directory.join(format!(
        ".{name}.{}.{}.tmp",
        std::process::id(),
        random_hex()
    ));
    let failed = |err: std::io::Error| {
        Error::msg(format!(
            "Could not write {}: {}",
            path.display(),
            describe(&err)
        ))
    };

    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o666);
    }
    let mut file = options.open(&temp_path).map_err(failed)?;
    let written = file
        .write_all(text.as_bytes())
        .and_then(|()| file.flush())
        .and_then(|()| file.sync_all())
        .and_then(|()| {
            drop(file);
            std::fs::rename(&temp_path, path)
        });
    if let Err(err) = written {
        let _ = std::fs::remove_file(&temp_path);
        return Err(failed(err));
    }
    Ok(())
}
