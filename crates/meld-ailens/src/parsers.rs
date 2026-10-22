//! Parsers for the output of the git commands AI Lens runs (port of
//! `parsers.py`)
//!
//! Every parser takes text decoded from git's output as UTF-8 (invalid
//! bytes replaced). Lines are split on `\n` only; a `\r` before it stays in
//! the content. Malformed output gives [`Error::Parse`].

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::LazyLock;

use regex::Regex;

use crate::error::{Error, Result};
use crate::models::{
    DiffLineKind, GitBlameLine, GitCommit, GitDiffHunk, GitDiffLine, GitFileChange, GitFileDiff,
    GitFileStatus, GitIdentity, GitRef, GitRefType, GitTrailer, GitWorktree,
};

fn parse_error(message: String) -> Error {
    Error::Parse(message)
}

// -- paths --

/// Undo git's C-style quoting of a path with special characters
///
/// Octal escapes are the bytes of the path's UTF-8 encoding (invalid UTF-8
/// is replaced). Unquoted paths are returned as they are.
pub fn unquote_git_path(value: &str) -> String {
    if value.len() < 2 || !value.starts_with('"') || !value.ends_with('"') {
        return value.to_string();
    }
    let inner: Vec<char> = value[1..value.len() - 1].chars().collect();
    let mut out: Vec<u8> = Vec::new();
    let mut i = 0;
    while i < inner.len() {
        let char = inner[i];
        if char != '\\' {
            let mut buffer = [0u8; 4];
            out.extend_from_slice(char.encode_utf8(&mut buffer).as_bytes());
            i += 1;
            continue;
        }
        let following = inner.get(i + 1).copied();
        match following {
            Some(digit @ '0'..='7') => {
                let digits: String = inner[i + 1..(i + 4).min(inner.len())]
                    .iter()
                    .take_while(|c| c.is_digit(8))
                    .collect();
                let digits = if digits.is_empty() {
                    digit.to_string()
                } else {
                    digits
                };
                let value = u32::from_str_radix(&digits, 8).unwrap_or(0);
                out.push((value & 0xFF) as u8);
                i += 4;
            }
            _ => {
                let byte = match following {
                    Some('a') => 0x07,
                    Some('b') => 0x08,
                    Some('f') => 0x0C,
                    Some('n') => 0x0A,
                    Some('r') => 0x0D,
                    Some('t') => 0x09,
                    Some('v') => 0x0B,
                    Some('\\') => 0x5C,
                    Some('"') => 0x22,
                    Some(other) => {
                        let mut buffer = [0u8; 4];
                        out.extend_from_slice(other.encode_utf8(&mut buffer).as_bytes());
                        i += 2;
                        continue;
                    }
                    None => b'\\',
                };
                out.push(byte);
                i += 2;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The quoted string at `value[start]`, and the byte index after it
fn read_quoted(value: &str, start: usize) -> Option<(&str, usize)> {
    let bytes = value.as_bytes();
    let mut i = start + 1;
    while i < bytes.len() {
        if bytes[i] == b'\\' {
            i += 2;
            continue;
        }
        if bytes[i] == b'"' {
            return Some((&value[start..=i], i + 1));
        }
        i += 1;
    }
    None
}

/// The two paths of a `diff --git <a> <b>` line, prefixes kept
fn split_header_paths(rest: &str) -> Option<(String, String)> {
    if rest.starts_with('"') {
        let (a, after) = read_quoted(rest, 0)?;
        if rest.as_bytes().get(after) != Some(&b' ') {
            return None;
        }
        let b = &rest[after + 1..];
        return Some((unquote_git_path(a), unquote_git_path(b)));
    }
    if rest.ends_with('"') {
        let index = rest.rfind(" \"")?;
        return Some((
            rest[..index].to_string(),
            unquote_git_path(&rest[index + 1..]),
        ));
    }
    // a/<path> b/<path>: with the same path on both sides, the halves have
    // equal length
    if rest.len() % 2 == 1 {
        let half = rest.len() / 2;
        if rest.is_char_boundary(half) && rest.as_bytes()[half] == b' ' {
            let (a, b) = (&rest[..half], &rest[half + 1..]);
            if a.get(2..).is_some() && a.get(2..) == b.get(2..) {
                return Some((a.to_string(), b.to_string()));
            }
        }
    }
    None
}

/// A `---`/`+++` path; None for `/dev/null`
fn header_path(value: &str, prefix: &str) -> Option<String> {
    // git appends a tab to paths that contain a space
    let value = value.strip_suffix('\t').unwrap_or(value);
    let path = unquote_git_path(value);
    if path == "/dev/null" {
        return None;
    }
    Some(
        path.strip_prefix(prefix)
            .map(str::to_string)
            .unwrap_or(path),
    )
}

// -- diffs --

static HUNK_HEADER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?s)\A@@ -([0-9]+)(?:,([0-9]+))? \+([0-9]+)(?:,([0-9]+))? @@ ?(.*)\z")
        .expect("valid regex")
});

#[derive(Default)]
struct FileDiffBuilder {
    old_path: Option<String>,
    new_path: Option<String>,
    binary: bool,
    new_file: bool,
    deleted_file: bool,
    explicit: bool,
    header: Option<(String, String)>,
    hunks: Vec<GitDiffHunk>,
}

impl FileDiffBuilder {
    fn build(self) -> GitFileDiff {
        let (mut old_path, mut new_path) = (self.old_path, self.new_path);
        if !self.explicit
            && let Some((a, b)) = &self.header
        {
            old_path = Some(a.strip_prefix("a/").unwrap_or(a).to_string());
            new_path = Some(b.strip_prefix("b/").unwrap_or(b).to_string());
        }
        if self.new_file {
            old_path = None;
        }
        if self.deleted_file {
            new_path = None;
        }
        GitFileDiff {
            old_path,
            new_path,
            binary: self.binary,
            hunks: self.hunks,
        }
    }
}

/// Parse unified `git diff` output (one or more files)
///
/// Hunk bodies are read by the line counts in their headers, so content
/// that looks like a header is safe. Files without `---`/`+++` lines
/// (binary, empty, mode-only) take their paths from the `diff --git` line
/// when it is unambiguous.
pub fn parse_diff(output: &str) -> Result<Vec<GitFileDiff>> {
    let lines: Vec<&str> = output.split('\n').collect();
    let mut files: Vec<FileDiffBuilder> = Vec::new();

    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        i += 1;

        if let Some(rest) = line.strip_prefix("diff --git ") {
            files.push(FileDiffBuilder {
                header: split_header_paths(rest),
                ..Default::default()
            });
            continue;
        }
        let Some(file) = files.last_mut() else {
            continue;
        };

        if line.starts_with("@@ ") {
            let (hunk, next) = parse_hunk(line, &lines, i)?;
            file.hunks.push(hunk);
            i = next;
        } else if let Some(rest) = line.strip_prefix("--- ") {
            file.old_path = header_path(rest, "a/");
            file.explicit = true;
        } else if let Some(rest) = line.strip_prefix("+++ ") {
            file.new_path = header_path(rest, "b/");
            file.explicit = true;
        } else if line.starts_with("rename from ") || line.starts_with("copy from ") {
            file.old_path = Some(unquote_git_path(line.splitn(3, ' ').nth(2).unwrap_or("")));
            file.explicit = true;
        } else if line.starts_with("rename to ") || line.starts_with("copy to ") {
            file.new_path = Some(unquote_git_path(line.splitn(3, ' ').nth(2).unwrap_or("")));
            file.explicit = true;
        } else if line.starts_with("new file mode ") {
            file.new_file = true;
        } else if line.starts_with("deleted file mode ") {
            file.deleted_file = true;
        } else if line.starts_with("Binary files ") || line == "GIT binary patch" {
            file.binary = true;
        }
    }
    Ok(files.into_iter().map(FileDiffBuilder::build).collect())
}

fn parse_hunk(header: &str, lines: &[&str], start: usize) -> Result<(GitDiffHunk, usize)> {
    let captures = HUNK_HEADER
        .captures(header)
        .ok_or_else(|| parse_error(format!("Unexpected git diff hunk header {header:?}")))?;
    let number = |index: usize, default: usize| -> Result<usize> {
        match captures.get(index) {
            None => Ok(default),
            Some(value) => value
                .as_str()
                .parse()
                .map_err(|_| parse_error(format!("Unexpected git diff hunk header {header:?}"))),
        }
    };
    let old_start = number(1, 0)?;
    let old_count = number(2, 1)?;
    let new_start = number(3, 0)?;
    let new_count = number(4, 1)?;
    let section = captures.get(5).map_or("", |m| m.as_str()).to_string();

    let mut body: Vec<GitDiffLine> = Vec::new();
    let (mut old_line, mut new_line) = (old_start, new_start);
    let (mut old_left, mut new_left) = (old_count as i64, new_count as i64);
    let mut i = start;
    while old_left > 0 || new_left > 0 || (i < lines.len() && lines[i].starts_with('\\')) {
        if i >= lines.len() || (i == lines.len() - 1 && lines[i].is_empty()) {
            return Err(parse_error(format!("Truncated git diff hunk {header:?}")));
        }
        let line = lines[i];
        i += 1;

        let content = line.get(1..).unwrap_or("");
        match line.as_bytes().first() {
            Some(b' ') => {
                body.push(GitDiffLine::new(
                    DiffLineKind::Context,
                    content,
                    Some(old_line),
                    Some(new_line),
                ));
                old_line += 1;
                new_line += 1;
                old_left -= 1;
                new_left -= 1;
            }
            Some(b'-') => {
                body.push(GitDiffLine::new(
                    DiffLineKind::Delete,
                    content,
                    Some(old_line),
                    None,
                ));
                old_line += 1;
                old_left -= 1;
            }
            Some(b'+') => {
                body.push(GitDiffLine::new(
                    DiffLineKind::Add,
                    content,
                    None,
                    Some(new_line),
                ));
                new_line += 1;
                new_left -= 1;
            }
            Some(b'\\') => {
                // "\ No newline at end of file" applies to the line before it
                if let Some(last) = body.last_mut() {
                    last.no_newline_at_end = true;
                }
            }
            _ => {
                return Err(parse_error(format!(
                    "Unexpected git diff line {line:?} in hunk {header:?}"
                )));
            }
        }
    }

    if old_left < 0 || new_left < 0 {
        return Err(parse_error(format!(
            "git diff hunk {header:?} has more lines than its header says"
        )));
    }

    Ok((
        GitDiffHunk {
            old_start,
            old_count,
            new_start,
            new_count,
            section,
            lines: body,
        },
        i,
    ))
}

// -- blame --

static BLAME_HEADER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\A([0-9a-f]{40,64}) ([0-9]+) ([0-9]+)(?: [0-9]+)?\z").expect("valid regex")
});

/// Parse `git blame --porcelain` (also with `--reverse`)
///
/// Commit details, `boundary` and `filename` among them, only appear the
/// first time a commit does; they apply to all of its lines. Sorted by
/// `final_line`.
pub fn parse_blame(output: &str) -> Result<Vec<GitBlameLine>> {
    let mut result: Vec<GitBlameLine> = Vec::new();
    let mut commits: HashMap<String, (bool, String)> = HashMap::new();
    let mut current: Option<(String, usize, usize)> = None;

    for line in output.split('\n') {
        let Some((sha, original, final_line)) = &current else {
            if line.is_empty() {
                continue;
            }
            let captures = BLAME_HEADER
                .captures(line)
                .ok_or_else(|| parse_error(format!("Unexpected git blame line {line:?}")))?;
            let sha = captures[1].to_string();
            let original = captures[2]
                .parse()
                .map_err(|_| parse_error(format!("Unexpected git blame line {line:?}")))?;
            let final_line = captures[3]
                .parse()
                .map_err(|_| parse_error(format!("Unexpected git blame line {line:?}")))?;
            commits
                .entry(sha.clone())
                .or_insert_with(|| (false, String::new()));
            current = Some((sha, original, final_line));
            continue;
        };

        if line.starts_with('\t') {
            // The line's content ends the entry
            let (boundary, filename) = commits.get(sha).cloned().unwrap_or_default();
            result.push(GitBlameLine {
                sha: sha.clone(),
                original_line: *original,
                final_line: *final_line,
                boundary,
                filename,
            });
            current = None;
        } else if line == "boundary" {
            if let Some(info) = commits.get_mut(sha) {
                info.0 = true;
            }
        } else if let Some(name) = line.strip_prefix("filename ")
            && let Some(info) = commits.get_mut(sha)
        {
            info.1 = unquote_git_path(name);
        }
    }

    if current.is_some() {
        return Err(parse_error("Truncated git blame output".into()));
    }
    result.sort_by_key(|blamed| blamed.final_line);
    Ok(result)
}

// -- log --

const LOG_FIELDS: [&str; 11] = [
    "%H",
    "%P",
    "%an",
    "%ae",
    "%aI",
    "%cn",
    "%ce",
    "%cI",
    "%s",
    "%b",
    "%(trailers:only,unfold,separator=%x1f)",
];

/// The `--format` argument [`parse_log`] reads; pass with `-z`
///
/// `with_notes` adds the commit's note (`%N`).
pub fn log_format(with_notes: bool) -> String {
    let mut fields: Vec<&str> = LOG_FIELDS.to_vec();
    if with_notes {
        fields.push("%N");
    }
    format!("--format={}", fields.join("%x00"))
}

pub fn parse_log(output: &str, with_notes: bool) -> Result<Vec<GitCommit>> {
    let field_count = LOG_FIELDS.len() + usize::from(with_notes);
    let mut tokens: Vec<&str> = output.split('\0').collect();
    // -z terminates the last record too, leaving one empty token
    if tokens.last() == Some(&"") {
        tokens.pop();
    }
    if !tokens.len().is_multiple_of(field_count) {
        return Err(parse_error(format!(
            "Unexpected git log output: {} fields is not a multiple of {field_count}",
            tokens.len()
        )));
    }

    let mut commits = Vec::new();
    for fields in tokens.chunks(field_count) {
        let note = if with_notes {
            fields[field_count - 1].trim()
        } else {
            ""
        };
        commits.push(GitCommit {
            sha: fields[0].to_string(),
            parents: fields[1].split_whitespace().map(str::to_string).collect(),
            author: GitIdentity {
                name: fields[2].into(),
                email: fields[3].into(),
                date: fields[4].into(),
            },
            committer: GitIdentity {
                name: fields[5].into(),
                email: fields[6].into(),
                date: fields[7].into(),
            },
            subject: fields[8].to_string(),
            body: fields[9].trim_end().to_string(),
            trailers: parse_trailers(fields[10]),
            note: (!note.is_empty()).then(|| note.to_string()),
        });
    }
    Ok(commits)
}

fn parse_trailers(value: &str) -> Vec<GitTrailer> {
    value
        .split('\x1f')
        .filter_map(|line| {
            let (key, rest) = line.split_once(':')?;
            let key = key.trim();
            if key.is_empty() {
                return None;
            }
            Some(GitTrailer::new(key, rest.trim()))
        })
        .collect()
}

// -- name-status, numstat --

fn nul_tokens(output: &str) -> Vec<&str> {
    let mut tokens: Vec<&str> = output.split('\0').collect();
    // Every entry ends with NUL, leaving one empty token
    if tokens.last() == Some(&"") {
        tokens.pop();
    }
    tokens
}

/// Parse `git diff --name-status -z` (and `diff-tree`)
pub fn parse_name_status(output: &str) -> Result<Vec<GitFileChange>> {
    let tokens = nul_tokens(output);
    let mut changes = Vec::new();
    let mut i = 0;
    while i < tokens.len() {
        let token = tokens[i];
        i += 1;
        let status = token
            .chars()
            .next()
            .and_then(GitFileStatus::from_letter)
            .ok_or_else(|| parse_error(format!("Unexpected git name-status entry {token:?}")))?;

        if matches!(status, GitFileStatus::Renamed | GitFileStatus::Copied) {
            if i + 1 >= tokens.len() {
                return Err(parse_error(format!(
                    "Truncated git name-status entry {token:?}"
                )));
            }
            let (original_path, path) = (tokens[i], tokens[i + 1]);
            i += 2;
            let digits = &token[1..];
            let similarity = if !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()) {
                digits.parse().ok()
            } else {
                None
            };
            changes.push(GitFileChange::renamed(
                status,
                path,
                original_path,
                similarity,
            ));
        } else {
            if i >= tokens.len() {
                return Err(parse_error(format!(
                    "Truncated git name-status entry {token:?}"
                )));
            }
            changes.push(GitFileChange::new(status, tokens[i]));
            i += 1;
        }
    }
    Ok(changes)
}

/// Lines added and deleted in one file; None for binary files
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct GitFileStats {
    /// For renames, the new path
    pub path: String,
    pub additions: Option<usize>,
    pub deletions: Option<usize>,
}

impl GitFileStats {
    pub fn new(
        path: impl Into<String>,
        additions: Option<usize>,
        deletions: Option<usize>,
    ) -> Self {
        Self {
            path: path.into(),
            additions,
            deletions,
        }
    }

    pub fn binary(&self) -> bool {
        self.additions.is_none()
    }
}

static NUMSTAT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?s)\A([0-9]+|-)\t([0-9]+|-)\t(.*)\z").expect("valid regex"));

/// Parse `git diff --numstat -z`; renames are keyed by their new path
pub fn parse_numstat(output: &str) -> Result<Vec<GitFileStats>> {
    let tokens = nul_tokens(output);
    let mut stats = Vec::new();
    let mut i = 0;
    while i < tokens.len() {
        let token = tokens[i];
        i += 1;
        let captures = NUMSTAT
            .captures(token)
            .ok_or_else(|| parse_error(format!("Unexpected git numstat entry {token:?}")))?;

        let mut path = captures[3].to_string();
        if path.is_empty() {
            // A rename: the old and new paths follow as their own tokens
            if i + 1 >= tokens.len() || tokens[i + 1].is_empty() {
                return Err(parse_error(format!(
                    "Truncated git numstat rename entry {token:?}"
                )));
            }
            path = tokens[i + 1].to_string();
            i += 2;
        }
        let count = |value: &str| -> Option<usize> {
            if value == "-" {
                None
            } else {
                value.parse().ok()
            }
        };
        stats.push(GitFileStats::new(
            path,
            count(&captures[1]),
            count(&captures[2]),
        ));
    }
    Ok(stats)
}

// -- small parsers --

/// Parse `git patch-id` output into commit SHA -> patch ID
pub fn parse_patch_ids(output: &str) -> Result<HashMap<String, String>> {
    let mut ids = HashMap::new();
    for line in output.split('\n') {
        if line.is_empty() {
            continue;
        }
        let parts: Vec<&str> = line.split(' ').collect();
        if parts.len() != 2 {
            return Err(parse_error(format!(
                "Unexpected git patch-id line {line:?}"
            )));
        }
        ids.insert(parts[1].to_string(), parts[0].to_string());
    }
    Ok(ids)
}

/// Parse `git rev-list --parents` into commit SHA -> parent SHAs
pub fn parse_rev_list_parents(output: &str) -> HashMap<String, Vec<String>> {
    parse_rev_list_parents_ordered(output).into_iter().collect()
}

/// [`parse_rev_list_parents`] in git's order (newest first)
pub fn parse_rev_list_parents_ordered(output: &str) -> Vec<(String, Vec<String>)> {
    output
        .split('\n')
        .filter(|line| !line.is_empty())
        .map(|line| {
            let mut parts = line.split(' ');
            let sha = parts.next().unwrap_or_default().to_string();
            (sha, parts.map(str::to_string).collect())
        })
        .collect()
}

// -- refs --

const REF_FIELDS: [&str; 12] = [
    "%(refname)",
    "%(refname:short)",
    "%(symref)",
    "%(HEAD)",
    "%(objecttype)",
    "%(objectname)",
    "%(contents:subject)",
    "%(committerdate:iso-strict)",
    // The same, peeled: set for annotated tags only
    "%(*objecttype)",
    "%(*objectname)",
    "%(*contents:subject)",
    "%(*committerdate:iso-strict)",
];

/// The `--format` argument [`parse_refs`] reads
pub fn ref_format() -> String {
    format!("--format={}", REF_FIELDS.join("%00"))
}

/// Parse `git for-each-ref` with [`ref_format`]
///
/// Skips symbolic refs (e.g. `origin/HEAD`) and refs to non-commits.
/// `current` comes from `%(HEAD)`; `worktree` is left for the caller.
pub fn parse_refs(output: &str) -> Result<Vec<GitRef>> {
    let mut refs = Vec::new();
    for line in output.split('\n') {
        if line.is_empty() {
            continue;
        }
        let fields: Vec<&str> = line.split('\0').collect();
        if fields.len() != REF_FIELDS.len() {
            return Err(parse_error(format!(
                "Unexpected git for-each-ref line {line:?}"
            )));
        }
        let [
            full_name,
            name,
            symref,
            head,
            object_type,
            sha,
            subject,
            date,
            peeled_type,
            peeled_sha,
            peeled_subject,
            peeled_date,
        ] = fields[..]
        else {
            continue;
        };
        if !symref.is_empty() {
            continue;
        }
        let kind = if full_name.starts_with("refs/heads/") {
            GitRefType::Branch
        } else if full_name.starts_with("refs/remotes/") {
            GitRefType::Remote
        } else if full_name.starts_with("refs/tags/") {
            GitRefType::Tag
        } else {
            continue;
        };

        let peeled = !peeled_type.is_empty();
        if (if peeled { peeled_type } else { object_type }) != "commit" {
            continue;
        }
        refs.push(GitRef {
            kind,
            name: name.into(),
            full_name: full_name.into(),
            sha: if peeled { peeled_sha } else { sha }.into(),
            subject: if peeled { peeled_subject } else { subject }.into(),
            date: if peeled { peeled_date } else { date }.into(),
            current: head == "*",
            worktree: None,
        });
    }
    Ok(refs)
}

// -- worktrees --

/// Parse `git worktree list --porcelain -z`
///
/// The first worktree is the main one. An all-zero `HEAD` (unborn branch)
/// becomes None.
pub fn parse_worktrees(output: &str) -> Result<Vec<GitWorktree>> {
    let mut worktrees: Vec<GitWorktree> = Vec::new();
    let mut record: Vec<(String, String)> = Vec::new();

    fn finish(record: &mut Vec<(String, String)>, worktrees: &mut Vec<GitWorktree>) -> Result<()> {
        if record.is_empty() {
            return Ok(());
        }
        let get = |key: &str| {
            record
                .iter()
                .rev()
                .find(|(k, _)| k == key)
                .map(|(_, v)| v.as_str())
        };
        let Some(path) = get("worktree") else {
            return Err(parse_error(format!(
                "Unexpected git worktree record {record:?}"
            )));
        };
        let head = get("HEAD")
            .filter(|head| !head.is_empty() && !head.chars().all(|c| c == '0'))
            .map(str::to_string);
        let branch = get("branch")
            .filter(|branch| !branch.is_empty())
            .map(|branch| {
                branch
                    .strip_prefix("refs/heads/")
                    .unwrap_or(branch)
                    .to_string()
            });
        let worktree = GitWorktree {
            path: PathBuf::from(path),
            head,
            branch,
            bare: get("bare").is_some(),
            detached: get("detached").is_some(),
            locked: get("locked").is_some(),
            prunable: get("prunable").is_some(),
            main: worktrees.is_empty(),
        };
        worktrees.push(worktree);
        record.clear();
        Ok(())
    }

    for token in output.split('\0') {
        if token.is_empty() {
            finish(&mut record, &mut worktrees)?;
            continue;
        }
        let (key, value) = token.split_once(' ').unwrap_or((token, ""));
        record.push((key.to_string(), value.to_string()));
    }
    finish(&mut record, &mut worktrees)?;
    Ok(worktrees)
}
