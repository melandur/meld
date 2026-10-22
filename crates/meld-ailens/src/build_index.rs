//! Finding the build files in a repository's build folder (port of
//! `build_index.py`)

use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use regex::Regex;

use crate::build_file::{BuildHeader, newest_first, parse_build_header};
use crate::build_narrative::js_trim;
use crate::error::{Error, Result};
use crate::git::{normalize, realpath};

pub const DEFAULT_BUILD_FOLDER: &str = "agent";

/// Headers of every `*_build.md` in the folder, in file-name order
///
/// Empty when the folder doesn't exist. A build file that can't be read is
/// left out (and logged) rather than failing the whole list; invalid UTF-8
/// is replaced, as AI Lens reads it.
pub fn read_build_headers(folder: &Path) -> Result<Vec<BuildHeader>> {
    let entries = match std::fs::read_dir(folder) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => {
            return Err(Error::msg(format!(
                "Could not read the build folder {}: {err}",
                folder.display()
            )));
        }
    };
    let mut names: Vec<String> = entries
        .filter_map(|entry| entry.ok())
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter(|name| name.ends_with("_build.md"))
        .collect();
    names.sort();

    let mut headers = Vec::new();
    for name in names {
        let path = folder.join(&name);
        if !path.is_file() {
            continue;
        }
        match std::fs::read(&path) {
            Ok(data) => headers.push(parse_build_header(&name, &String::from_utf8_lossy(&data))),
            Err(err) => {
                log::warn!("Skipping unreadable build file {}: {err}", path.display());
            }
        }
    }
    Ok(headers)
}

/// File names of the builds that cover a commit, newest first
///
/// By SHA, or by patch ID when the commit was rebased or cherry-picked
/// since (same change, new SHA). Squashed commits have a new patch and
/// aren't found.
pub fn find_builds(headers: &[BuildHeader], sha: &str, patch_id: Option<&str>) -> Vec<String> {
    let mut found: Vec<&BuildHeader> = headers
        .iter()
        .filter(|header| {
            header.commits.iter().any(|commit| {
                commit.sha == sha || (patch_id.is_some() && commit.patch_id.as_deref() == patch_id)
            })
        })
        .collect();
    found.sort_by(|a, b| newest_first(a, b));
    found.into_iter().map(|h| h.file_name.clone()).collect()
}

static BUILD_FILE_NAME_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\A[^/\\]+_build\.md\z").expect("valid regex"));

/// Whether `name` is a plain build file name, safe to join to the folder
pub fn is_build_file_name(name: &str) -> bool {
    BUILD_FILE_NAME_RE.is_match(name) && !name.starts_with('.')
}

/// The build folder inside the repository
///
/// `folder` is relative to `repo_root`; empty means
/// [`DEFAULT_BUILD_FOLDER`]. Refuses absolute paths, paths that climb out
/// with `..`, and paths that leave the repository through a symlink.
pub fn resolve_build_folder(repo_root: &Path, folder: &str) -> Result<PathBuf> {
    let trimmed = js_trim(folder);
    let relative = normalize(Path::new(if trimmed.is_empty() {
        DEFAULT_BUILD_FOLDER
    } else {
        trimmed
    }));
    let error = || {
        Error::msg(format!(
            "The build folder \"{folder}\" must be a folder inside the repository."
        ))
    };
    let text = relative.to_string_lossy();
    if relative.is_absolute() || text == ".." || text.starts_with("../") || text.starts_with("..\\")
    {
        return Err(error());
    }

    let path = normalize(&repo_root.join(&relative));
    // Symlinks in the existing part of the path must stay inside, too
    let real_root = realpath(repo_root);
    let real_path = realpath(&path);
    if !real_path.starts_with(&real_root) {
        return Err(error());
    }
    Ok(path)
}
