//! Agent instructions: a `CLAUDE.md` section and the `/why` skill (port of
//! `agent_setup.py`)
//!
//! They point coding agents at the build folder, so they look up why code
//! is the way it is before changing it. The text is the same as AI Lens
//! writes.

use std::path::{Path, PathBuf};

use crate::build_narrative::{js_trim, js_trim_end};
use crate::error::{Error, Result};
use crate::generate_build::write_text_atomic;

pub const SECTION_START: &str = "<!-- ai-lens:start -->";
pub const SECTION_END: &str = "<!-- ai-lens:end -->";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentSetupOptions {
    /// Build folder relative to the repository root, e.g. `agent`
    pub build_folder: String,
    /// Notes ref name, e.g. `ai-lens`
    pub notes_ref: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentSetupFile {
    pub path: PathBuf,
    pub content: String,
    pub existed: bool,
    pub changed: bool,
}

/// The section added to a repository's `CLAUDE.md`
///
/// It sits between markers so it can be updated in place.
pub fn render_claude_md_section(options: &AgentSetupOptions) -> String {
    let folder = &options.build_folder;
    [
        SECTION_START.to_string(),
        "## Why the code is the way it is (AI Lens)".into(),
        String::new(),
        format!(
            "`{folder}/` holds AI Lens builds: one file per compared commit range, saying what each commit changed, why,"
        ),
        format!("and which lines it touched. `{folder}/index.md` lists them, newest first."),
        String::new(),
        "- Before changing code whose purpose isn't clear, find out why it is the way it is: run `/why <file>:<line>`,".into(),
        format!("  or read the build in `{folder}/index.md` that covers it."),
        "- A \"Why (inferred …)\" is a model's reading of the diff, not the author's reason. Treat it as a guess.".into(),
        SECTION_END.to_string(),
    ]
    .join("\n")
}

/// Adds the section to `CLAUDE.md`'s text, or replaces the one there
pub fn merge_claude_md(existing: Option<&str>, section: &str) -> String {
    let Some(existing) = existing.filter(|text| !js_trim(text).is_empty()) else {
        return format!("{section}\n");
    };
    if let Some(start) = existing.find(SECTION_START)
        && let Some(offset) = existing[start..].find(SECTION_END)
    {
        let end = start + offset;
        if end > start {
            return format!(
                "{}{section}{}",
                &existing[..start],
                &existing[end + SECTION_END.len()..]
            );
        }
    }
    format!("{}\n\n{section}\n", js_trim_end(existing))
}

const WHY_SKILL: &str = r#"---
name: why
description: Explain why a line or block of code is the way it is, from the AI Lens builds in {folder}/ and the git history. Use when asked why code is written a certain way, before changing code whose purpose is unclear, or as /why <file>:<line> or /why <file>:<start>-<end>.
---

# Why is this code the way it is?

Input: `<file>:<line>` or `<file>:<start>-<end>`. Ask for it if it is missing.

1. Find the commits that last changed those lines:
   `git blame --porcelain -L <start>,<end> -- <file>`
   Each line's header starts with the full commit SHA. Skip `0000000000000000000000000000000000000000`: that
   line isn't committed yet.
2. Find the builds that cover each commit:
   `grep -l "sha: <SHA>" {folder}/*_build.md`
   None found? The commit may have been rebased or cherry-picked since. Find it by its change instead:
   `git show <SHA> | git patch-id --stable` (the first word is the patch ID), then
   `grep -l "patch_id: <PATCH_ID>" {folder}/*_build.md`
3. In each build found, newest `created:` first, read:
   - under `## Changes`, the `### <file>` section and the entries for the commit (its short SHA in backticks)
     whose line ranges cover the code: when its changes were explained one by one, each entry has the What and
     Why of that change. Line numbers there are as of the build's target commit, so they may have moved.
   - under `## Commits`, the commit's own What and Why.
   - `## Summary`, `## Decisions` and `## Risks / open questions`, for the wider reason.
4. No build covers it? Read the commit itself: `git log -1 --format=%B <SHA>`, and its AI Lens note if it has
   one: `git notes --ref={notes_ref} show <SHA>`. A note's `Change: -<old> +<new> <path>` sections
   explain single hunks of the commit's own diff (`git show <SHA>`); use the one whose `+<start>,<count>`
   range holds the line's number in that commit (the second number in its blame header).
5. Answer in a few sentences: what the code does, why it was written this way, and where that reason comes
   from (build, commit message or note, with the commit's short SHA and the build's file name). A Why marked
   "inferred" is a model's guess, so say so. If no reason is recorded anywhere, say that plainly. Don't make
   one up.
"#;

/// A Claude Code project skill
///
/// `/why <file>:<line>` explains code from the builds and the history.
pub fn render_why_skill(options: &AgentSetupOptions) -> String {
    // Not a format string: the folder and ref are inserted verbatim, braces
    // too
    WHY_SKILL
        .replace("{folder}", &options.build_folder)
        .replace("{notes_ref}", &options.notes_ref)
}

/// What setup would write; nothing is written yet
///
/// `CLAUDE.md` with the section, and the `/why` skill.
pub fn plan_agent_setup(
    repo_root: &Path,
    options: &AgentSetupOptions,
) -> Result<Vec<AgentSetupFile>> {
    let claude_md_path = repo_root.join("CLAUDE.md");
    let skill_path = repo_root
        .join(".claude")
        .join("skills")
        .join("why")
        .join("SKILL.md");
    let claude_md = read_if_exists(&claude_md_path)?;
    let skill = read_if_exists(&skill_path)?;

    let planned = [
        (
            claude_md_path,
            merge_claude_md(claude_md.as_deref(), &render_claude_md_section(options)),
            claude_md,
        ),
        (skill_path, render_why_skill(options), skill),
    ];
    Ok(planned
        .into_iter()
        .map(|(path, content, existing)| AgentSetupFile {
            path,
            existed: existing.is_some(),
            changed: existing.as_deref() != Some(content.as_str()),
            content,
        })
        .collect())
}

/// Writes the planned files that change, each atomically
pub fn write_agent_setup(files: &[AgentSetupFile]) -> Result<()> {
    for file in files.iter().filter(|file| file.changed) {
        if let Some(directory) = file.path.parent() {
            std::fs::create_dir_all(directory).map_err(|err| {
                Error::msg(format!("Could not create {}: {err}", directory.display()))
            })?;
        }
        write_text_atomic(&file.path, &file.content)?;
    }
    Ok(())
}

fn read_if_exists(path: &Path) -> Result<Option<String>> {
    match std::fs::read(path) {
        // Invalid UTF-8 is replaced: the file is rewritten as UTF-8
        Ok(data) => Ok(Some(String::from_utf8_lossy(&data).into_owned())),
        Err(err)
            if err.kind() == std::io::ErrorKind::NotFound
                || err.kind() == std::io::ErrorKind::NotADirectory =>
        {
            // A file where a folder on the path should be: writing will say so
            Ok(None)
        }
        Err(err) => Err(Error::msg(format!(
            "Could not read {}: {err}",
            path.display()
        ))),
    }
}
