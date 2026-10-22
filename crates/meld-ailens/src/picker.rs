//! Entries of the Base and Target revision pickers (port of `picker.py`)
//!
//! Labels are English; the app may translate them.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use regex::Regex;

use crate::git::realpath;
use crate::models::{GitCommit, GitRef, GitRefType, GitWorktree, Revision, RevisionKind};
use crate::time::{now_timestamp, parse_iso8601};

pub const COMMIT_PAGE_SIZE: usize = 200;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EntryKind {
    Head,
    Uncommitted,
    Branch,
    Remote,
    Tag,
    Commit,
    Typed,
}

/// Which picker an entry is for
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PickerSide {
    Base,
    Target,
}

#[derive(Debug, Clone)]
pub struct PickerEntry {
    pub kind: EntryKind,
    /// Section heading the entry is listed under
    pub section: String,
    pub label: String,
    pub description: String,
    /// A freedesktop icon name
    pub icon_name: String,
    /// What picking the entry sets the picker to
    pub revision: Revision,
    /// How the comparison names the revision once picked
    pub short_label: String,
    /// Picking this also sets the other side (e.g. Unstaged sets the base
    /// to Staged); None when only this side changes
    pub paired_base: Option<Revision>,
    /// Lower-cased text the filter matches against
    pub search_text: String,
}

impl PartialEq for PickerEntry {
    /// `search_text` doesn't count
    fn eq(&self, other: &Self) -> bool {
        self.kind == other.kind
            && self.section == other.section
            && self.label == other.label
            && self.description == other.description
            && self.icon_name == other.icon_name
            && self.revision == other.revision
            && self.short_label == other.short_label
            && self.paired_base == other.paired_base
    }
}

fn plural(n: i64, singular: &str, plural: &str) -> String {
    format!("{n} {}", if n == 1 { singular } else { plural })
}

pub fn short_sha(sha: &str) -> &str {
    crate::claude::prefix(sha, 7)
}

/// e.g. "3 days ago", "in 2 hours", "now"; dates are seconds since the
/// epoch
pub fn relative_date(date: f64, now: f64) -> String {
    let seconds = (date - now).round() as i64;
    let units: [(i64, &str, &str); 6] = [
        (365 * 24 * 3600, "year", "years"),
        (30 * 24 * 3600, "month", "months"),
        (7 * 24 * 3600, "week", "weeks"),
        (24 * 3600, "day", "days"),
        (3600, "hour", "hours"),
        (60, "minute", "minutes"),
    ];
    for (size, singular, plural_name) in units {
        if seconds.abs() >= size {
            let text = plural(seconds.abs() / size, singular, plural_name);
            return if seconds < 0 {
                format!("{text} ago")
            } else {
                format!("in {text}")
            };
        }
    }
    "now".into()
}

/// Seconds since the epoch; a date without a time zone is UTC
pub fn parse_iso_date(value: &str) -> Option<f64> {
    parse_iso8601(value)
}

fn when(value: &str, now: f64) -> String {
    parse_iso_date(value).map_or_else(String::new, |date| relative_date(date, now))
}

fn join(parts: &[Option<&str>]) -> String {
    parts
        .iter()
        .flatten()
        .filter(|part| !part.is_empty())
        .copied()
        .collect::<Vec<_>>()
        .join(" · ")
}

#[allow(clippy::too_many_arguments)]
fn entry(
    kind: EntryKind,
    section: &str,
    label: &str,
    description: &str,
    icon_name: &str,
    revision: Revision,
    short_label: &str,
    paired_base: Option<Revision>,
) -> PickerEntry {
    let search_text = format!("{label} {description} {short_label}").to_lowercase();
    PickerEntry {
        kind,
        section: section.into(),
        label: label.into(),
        description: description.into(),
        icon_name: icon_name.into(),
        revision,
        short_label: short_label.into(),
        paired_base,
        search_text,
    }
}

pub fn worktree_name(worktree: &GitWorktree) -> String {
    if let Some(branch) = &worktree.branch {
        return branch.clone();
    }
    if let Some(head) = &worktree.head {
        return short_sha(head).to_string();
    }
    worktree
        .path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| worktree.path.display().to_string())
}

/// What [`build_entries`] lists
#[derive(Debug, Clone, Default)]
pub struct PickerSources<'a> {
    pub refs: &'a [GitRef],
    pub worktrees: &'a [GitWorktree],
    pub commits: &'a [GitCommit],
    pub current_worktree: Option<&'a Path>,
    pub head: Option<&'a GitCommit>,
    pub head_branch: Option<&'a str>,
    /// Seconds since the epoch; now by default
    pub now: Option<f64>,
}

/// Every entry a picker offers, in display order
///
/// HEAD, the uncommitted versions (this worktree's first, then the other
/// worktrees'), branches (current first), remote branches, tags (each
/// newest first), then commits newest first.
pub fn build_entries(sources: &PickerSources<'_>, side: PickerSide) -> Vec<PickerEntry> {
    let now = sources.now.unwrap_or_else(now_timestamp);
    let mut entries = Vec::new();

    if let Some(head) = sources.head {
        entries.push(entry(
            EntryKind::Head,
            "Current",
            "HEAD",
            &join(&[
                Some(sources.head_branch.unwrap_or("detached")),
                Some(short_sha(&head.sha)),
                Some(&head.subject),
            ]),
            "go-home-symbolic",
            Revision::commit("HEAD"),
            "HEAD",
            None,
        ));
    }

    let current = sources.current_worktree.map(realpath);
    let mut ordered: Vec<&GitWorktree> = sources.worktrees.iter().filter(|wt| !wt.bare).collect();
    ordered.sort_by_key(|wt| {
        (
            Some(realpath(&wt.path)) != current,
            !wt.main,
            wt.path.to_string_lossy().into_owned(),
        )
    });
    for wt in ordered {
        let is_current = current.as_ref().is_some_and(|c| realpath(&wt.path) == *c);
        // None names the repository's own worktree, so a comparison saved
        // with it survives the worktree being moved
        let place: Option<PathBuf> = if is_current {
            None
        } else {
            Some(wt.path.clone())
        };
        let name = worktree_name(wt);
        let section = if is_current {
            "Uncommitted".to_string()
        } else {
            format!("Worktree {name}")
        };
        let suffix = if is_current {
            String::new()
        } else {
            format!(" ({name})")
        };
        let path_text = wt.path.display().to_string();
        let location = join(&[
            (!is_current).then_some(path_text.as_str()),
            wt.head.is_none().then_some("unborn branch"),
        ]);
        let location = (!location.is_empty()).then_some(location.as_str());
        if side == PickerSide::Target {
            entries.push(entry(
                EntryKind::Uncommitted,
                &section,
                &format!("Working Tree{suffix}"),
                &join(&[Some("staged, unstaged and untracked changes"), location]),
                "document-edit-symbolic",
                Revision::working_tree(place.clone()),
                &format!("Working Tree{suffix}"),
                None,
            ));
            entries.push(entry(
                EntryKind::Uncommitted,
                &section,
                &format!("Unstaged{suffix}"),
                &join(&[
                    Some("changes not staged yet (Staged → Working Tree)"),
                    location,
                ]),
                "document-edit-symbolic",
                Revision::working_tree(place.clone()),
                &format!("Working Tree{suffix}"),
                Some(Revision::index(place.clone())),
            ));
        }
        entries.push(entry(
            EntryKind::Uncommitted,
            &section,
            &format!("Staged{suffix}"),
            &join(&[Some("staged changes only (the index)"), location]),
            "list-add-symbolic",
            Revision::index(place.clone()),
            &format!("Staged{suffix}"),
            None,
        ));
    }

    let newest_first = |kind: GitRefType| -> Vec<&GitRef> {
        let mut items: Vec<&GitRef> = sources.refs.iter().filter(|r| r.kind == kind).collect();
        items.sort_by(|a, b| {
            let a = parse_iso_date(&a.date).unwrap_or(now);
            let b = parse_iso_date(&b.date).unwrap_or(now);
            b.partial_cmp(&a).unwrap_or(std::cmp::Ordering::Equal)
        });
        items
    };
    let mut branches = newest_first(GitRefType::Branch);
    branches.sort_by_key(|r| !r.current);
    let sections = [
        (
            EntryKind::Branch,
            "Branches",
            "media-playlist-consecutive-symbolic",
            branches,
        ),
        (
            EntryKind::Remote,
            "Remote branches",
            "network-server-symbolic",
            newest_first(GitRefType::Remote),
        ),
        (
            EntryKind::Tag,
            "Tags",
            "bookmark-new-symbolic",
            newest_first(GitRefType::Tag),
        ),
    ];
    for (kind, section, icon, items) in sections {
        for r in items {
            let worktree = r
                .worktree
                .as_ref()
                .filter(|path| current.as_ref().is_none_or(|c| realpath(path) != *c))
                .map(|path| format!("in worktree {}", path.display()));
            entries.push(entry(
                kind,
                section,
                &r.name,
                &join(&[
                    r.current.then_some("current"),
                    worktree.as_deref(),
                    Some(short_sha(&r.sha)),
                    Some(&r.subject),
                ]),
                if r.current {
                    "object-select-symbolic"
                } else {
                    icon
                },
                Revision::commit(r.full_name.clone()),
                &r.name,
                None,
            ));
        }
    }

    for commit in sources.commits {
        let label = if commit.subject.is_empty() {
            "(no message)"
        } else {
            &commit.subject
        };
        entries.push(entry(
            EntryKind::Commit,
            "Commits",
            label,
            &join(&[
                Some(short_sha(&commit.sha)),
                Some(&commit.author.name),
                Some(&when(&commit.author.date, now)),
            ]),
            "emblem-documents-symbolic",
            Revision::commit(commit.sha.clone()),
            short_sha(&commit.sha),
            None,
        ));
    }
    entries
}

static REF_CHARS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^[^\s~^:?*\[\\]+(?:[~^][0-9]*)*$|^[0-9a-fA-F]{4,64}$").expect("valid regex")
});

/// The entry for a ref or SHA typed into the filter
///
/// None when the text can't be one. Whether it names a commit is only known
/// once git resolves it.
pub fn typed_entry(text: &str) -> Option<PickerEntry> {
    let ref_name = text.trim();
    if ref_name.is_empty() || !REF_CHARS.is_match(ref_name) || ref_name.starts_with('-') {
        return None;
    }
    Some(entry(
        EntryKind::Typed,
        "Typed",
        &format!("Use “{ref_name}”"),
        "a branch, tag or SHA",
        "system-search-symbolic",
        Revision::commit(ref_name),
        ref_name,
        None,
    ))
}

/// Entries whose text contains every word of `query`
///
/// A typed-ref entry comes first when the query could name a revision that
/// is not already listed by that exact name.
pub fn filter_entries(entries: &[PickerEntry], query: &str) -> Vec<PickerEntry> {
    let lowered = query.to_lowercase();
    let words: Vec<&str> = lowered.split_whitespace().collect();
    if words.is_empty() {
        return entries.to_vec();
    }
    let mut matched: Vec<PickerEntry> = entries
        .iter()
        .filter(|e| words.iter().all(|w| e.search_text.contains(w)))
        .cloned()
        .collect();
    if let Some(typed) = typed_entry(query) {
        let listed = matched
            .iter()
            .any(|e| e.short_label == query.trim() || e.revision == typed.revision);
        if !listed {
            matched.insert(0, typed);
        }
    }
    matched
}

const WORKING_TREE_NAMES: [&str; 4] = ["working-tree", "worktree", "working", "wt"];
const INDEX_NAMES: [&str; 3] = ["staged", "index", "cached"];
const UNSTAGED_NAMES: [&str; 1] = ["unstaged"];

/// Base and target from command-line text
///
/// Besides any ref or SHA, `working-tree`, `staged` and (as the target
/// only) `unstaged`, which also makes the base `staged`. Defaults: HEAD →
/// working tree. Fails with a message for text that can't be a revision.
pub fn revisions_from_args(
    base: Option<&str>,
    target: Option<&str>,
) -> Result<(Revision, Revision), String> {
    fn parse(text: &str) -> Result<Revision, String> {
        let name = text.trim().to_lowercase();
        if WORKING_TREE_NAMES.contains(&name.as_str()) {
            return Ok(Revision::working_tree(None));
        }
        if INDEX_NAMES.contains(&name.as_str()) {
            return Ok(Revision::index(None));
        }
        if typed_entry(text).is_none() {
            return Err(format!("“{text}” is not a revision"));
        }
        Ok(Revision::commit(text.trim()))
    }

    let is_unstaged = |text: &str| UNSTAGED_NAMES.contains(&text.trim().to_lowercase().as_str());
    if target.is_some_and(is_unstaged) {
        if let Some(base) = base
            && parse(base)? != Revision::index(None)
        {
            return Err("“unstaged” compares Staged with the working tree".into());
        }
        return Ok((Revision::index(None), Revision::working_tree(None)));
    }
    if base.is_some_and(is_unstaged) {
        return Err("“unstaged” can only be the target".into());
    }
    Ok((
        match base {
            Some(base) => parse(base)?,
            None => Revision::commit("HEAD"),
        },
        match target {
            Some(target) => parse(target)?,
            None => Revision::working_tree(None),
        },
    ))
}

/// Base and target after picking `entry` for `side`
///
/// Keeps the pair valid: the index can only be a base for the working tree
/// of the same worktree, so picking one side adjusts the other when needed.
/// `worktree_heads` maps worktree paths (None: this one) to their HEAD.
pub fn apply_pick(
    base: &Revision,
    target: &Revision,
    entry: &PickerEntry,
    side: PickerSide,
    worktree_heads: &HashMap<Option<PathBuf>, String>,
) -> (Revision, Revision) {
    let head_of = |worktree: &Option<PathBuf>| {
        worktree_heads
            .get(worktree)
            .cloned()
            .unwrap_or_else(|| "HEAD".to_string())
    };
    if side == PickerSide::Base {
        let base = entry.revision.clone();
        return match base.kind {
            RevisionKind::Index => {
                let target = Revision::working_tree(base.worktree.clone());
                (base, target)
            }
            // Only a target can be the working tree; swap it there
            RevisionKind::WorkingTree => (Revision::commit(head_of(&base.worktree)), base),
            RevisionKind::Commit => (base, target.clone()),
        };
    }

    let target = entry.revision.clone();
    if let Some(paired) = &entry.paired_base {
        return (paired.clone(), target);
    }
    let mut base = base.clone();
    if base.kind != RevisionKind::Commit && target != Revision::working_tree(base.worktree.clone())
    {
        base = Revision::commit(head_of(&target.worktree));
    }
    (base, target)
}
