//! Port of test/ailens/test_agent_setup.py (itself ported from AI Lens
//! src/agents/__tests__/agentSetup.test.ts, plus edge cases)

use std::path::Path;

use meld_ailens::agent_setup::{
    AgentSetupOptions, SECTION_END, SECTION_START, merge_claude_md, plan_agent_setup,
    render_claude_md_section, render_why_skill, write_agent_setup,
};
use regex::Regex;

fn options() -> AgentSetupOptions {
    AgentSetupOptions {
        build_folder: "docs/agent".into(),
        notes_ref: "ai-lens".into(),
    }
}

fn section() -> String {
    render_claude_md_section(&options())
}

fn plan(root: &Path) -> Vec<meld_ailens::agent_setup::AgentSetupFile> {
    plan_agent_setup(root, &options()).unwrap()
}

fn skill_path(root: &Path) -> std::path::PathBuf {
    root.join(".claude/skills/why/SKILL.md")
}

// -- Ported --

#[test]
fn section_points_at_build_folder_and_why_between_markers() {
    let section = section();
    assert!(section.starts_with("<!-- ai-lens:start -->\n"));
    assert!(section.ends_with("\n<!-- ai-lens:end -->"));
    assert!(section.contains("`docs/agent/index.md`"));
    assert!(section.contains("/why <file>:<line>"));
}

#[test]
fn merge_creates_appends_and_replaces_in_place() {
    let section = section();
    assert_eq!(merge_claude_md(None, &section), format!("{section}\n"));

    let appended = merge_claude_md(Some("# Project\n\nRules.\n"), &section);
    assert_eq!(appended, format!("# Project\n\nRules.\n\n{section}\n"));

    let updated = merge_claude_md(
        Some(&appended.replace("## Why the code", "## Old heading")),
        &section,
    );
    assert_eq!(updated, appended);
    assert_eq!(merge_claude_md(Some(&updated), &section), updated);
}

#[test]
fn why_skill_finds_builds_by_sha_then_patch_id() {
    let skill = render_why_skill(&options());
    assert!(
        Regex::new(r"\A---\nname: why\ndescription: .+\n---\n")
            .unwrap()
            .is_match(&skill)
    );
    assert!(skill.contains("git blame --porcelain -L <start>,<end> -- <file>"));
    assert!(skill.contains("grep -l \"sha: <SHA>\" docs/agent/*_build.md"));
    assert!(skill.contains("grep -l \"patch_id: <PATCH_ID>\" docs/agent/*_build.md"));
    assert!(skill.contains("git notes --ref=ai-lens show <SHA>"));
}

#[test]
fn plan_only_changed_files_and_write_them() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    std::fs::write(repo.join("CLAUDE.md"), "# Project\n").unwrap();

    let first = plan(repo);
    let summary: Vec<(String, bool)> = first
        .iter()
        .map(|f| {
            let relative = f
                .path
                .strip_prefix(repo)
                .unwrap()
                .to_string_lossy()
                .into_owned();
            (relative, f.changed)
        })
        .collect();
    assert_eq!(
        summary,
        [
            ("CLAUDE.md".to_string(), true),
            (".claude/skills/why/SKILL.md".to_string(), true),
        ]
    );

    write_agent_setup(&first).unwrap();
    assert_eq!(
        std::fs::read_to_string(repo.join("CLAUDE.md")).unwrap(),
        format!("# Project\n\n{}\n", section())
    );
    assert_eq!(
        std::fs::read_to_string(skill_path(repo)).unwrap(),
        render_why_skill(&options())
    );

    assert!(!plan(repo).iter().any(|f| f.changed));
}

// -- Exact text --

#[test]
fn section_exact() {
    let expected = [
        "<!-- ai-lens:start -->",
        "## Why the code is the way it is (AI Lens)",
        "",
        "`agent/` holds AI Lens builds: one file per compared commit range, saying what each commit changed, why,",
        "and which lines it touched. `agent/index.md` lists them, newest first.",
        "",
        "- Before changing code whose purpose isn't clear, find out why it is the way it is: run `/why <file>:<line>`,",
        "  or read the build in `agent/index.md` that covers it.",
        "- A \"Why (inferred …)\" is a model's reading of the diff, not the author's reason. Treat it as a guess.",
        "<!-- ai-lens:end -->",
    ];
    let section = render_claude_md_section(&AgentSetupOptions {
        build_folder: "agent".into(),
        notes_ref: "ai-lens".into(),
    });
    assert_eq!(section, expected.join("\n"));
}

/// The SHA-1 of the Python `render_why_skill(AgentSetupOptions("agent",
/// "ai-lens"))`, so the skill stays byte-identical to what AI Lens writes
#[test]
fn why_skill_matches_python() {
    let skill = render_why_skill(&AgentSetupOptions {
        build_folder: "agent".into(),
        notes_ref: "ai-lens".into(),
    });
    assert_eq!(
        sha1_smol::Sha1::from(skill.as_bytes()).digest().to_string(),
        "abd137bd9f834f6c639f5ce66d6536e6c046294f"
    );
    assert!(skill.contains("\n---\n\n# Why is this code the way it is?\n\nInput: "));
}

#[test]
fn why_skill_inserts_values_verbatim() {
    let skill = render_why_skill(&AgentSetupOptions {
        build_folder: "we{ir}d {folder}".into(),
        notes_ref: "re{f}".into(),
    });
    assert!(skill.contains("AI Lens builds in we{ir}d {folder}/ and"));
    assert!(skill.contains("git notes --ref=re{f} show <SHA>"));
    assert!(!skill.contains("{notes_ref}"));
    assert!(skill.ends_with("one up.\n"));
}

// -- merge_claude_md --

#[test]
fn merge_into_blank_file() {
    let section = section();
    assert_eq!(merge_claude_md(Some(""), &section), format!("{section}\n"));
    assert_eq!(
        merge_claude_md(Some(" \n\t\n"), &section),
        format!("{section}\n")
    );
}

#[test]
fn merge_keeps_text_around_the_section() {
    let section = section();
    let existing = format!("# Top\n\n{SECTION_START}\nold\n{SECTION_END}\n\n## Bottom\n");
    assert_eq!(
        merge_claude_md(Some(&existing), &section),
        format!("# Top\n\n{section}\n\n## Bottom\n")
    );
}

#[test]
fn merge_appends_when_markers_are_broken() {
    let section = section();
    // End before start, or start only: append a fresh section
    for existing in [
        format!("{SECTION_END}\nx\n{SECTION_START}\n"),
        format!("# P\n{SECTION_START}\nold"),
    ] {
        assert_eq!(
            merge_claude_md(Some(&existing), &section),
            format!("{}\n\n{section}\n", existing.trim_end())
        );
    }
}

#[test]
fn merge_replaces_only_the_first_section() {
    let section = section();
    let existing =
        format!("{SECTION_START}\na\n{SECTION_END}\n{SECTION_START}\nb\n{SECTION_END}\n");
    assert_eq!(
        merge_claude_md(Some(&existing), &section),
        format!("{section}\n{SECTION_START}\nb\n{SECTION_END}\n")
    );
}

#[test]
fn merge_keeps_crlf_and_unicode_elsewhere() {
    let section = section();
    let existing = "# Pröject\r\n\r\nRules \u{1f600}.\r\n\r\n";
    assert_eq!(
        merge_claude_md(Some(existing), &section),
        format!("# Pröject\r\n\r\nRules \u{1f600}.\n\n{section}\n")
    );
}

// -- plan / write --

#[test]
fn plan_in_empty_repo() {
    let tmp = tempfile::tempdir().unwrap();
    let files = plan(tmp.path());
    let flags: Vec<bool> = files.iter().map(|f| f.changed).collect();
    assert_eq!(flags, [true, true]);
    assert_eq!(files[0].content, format!("{}\n", section()));
}

#[cfg(unix)]
#[test]
fn write_skips_unchanged_files() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().unwrap();
    write_agent_setup(&plan(tmp.path())).unwrap();
    let skill = skill_path(tmp.path());
    std::fs::set_permissions(&skill, std::fs::Permissions::from_mode(0o444)).unwrap();
    // Nothing changed, so the read-only skill isn't touched
    let result = write_agent_setup(&plan(tmp.path()));
    std::fs::set_permissions(&skill, std::fs::Permissions::from_mode(0o644)).unwrap();
    result.unwrap();
}

#[test]
fn rewrites_an_edited_skill() {
    let tmp = tempfile::tempdir().unwrap();
    write_agent_setup(&plan(tmp.path())).unwrap();
    let skill = skill_path(tmp.path());
    std::fs::write(&skill, "edited").unwrap();
    let files = plan(tmp.path());
    let flags: Vec<bool> = files.iter().map(|f| f.changed).collect();
    assert_eq!(flags, [false, true]);
    write_agent_setup(&files).unwrap();
    assert_eq!(
        std::fs::read_to_string(&skill).unwrap(),
        render_why_skill(&options())
    );
}

/// Deviation: Python keeps bytes that aren't UTF-8 (surrogateescape); here
/// CLAUDE.md is read as lossy UTF-8, so such bytes become U+FFFD
#[test]
fn bytes_that_are_not_utf8_become_replacement_characters() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("CLAUDE.md"), b"# Caf\xe9\n").unwrap();
    write_agent_setup(&plan(tmp.path())).unwrap();
    assert_eq!(
        std::fs::read_to_string(tmp.path().join("CLAUDE.md")).unwrap(),
        format!("# Caf\u{fffd}\n\n{}\n", section())
    );
}

#[test]
fn claude_md_that_is_a_folder() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir(tmp.path().join("CLAUDE.md")).unwrap();
    let err = plan_agent_setup(tmp.path(), &options()).unwrap_err();
    assert!(err.to_string().contains("Could not read"), "{err}");
}

#[test]
fn claude_folder_that_is_a_file() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join(".claude"), "x").unwrap();
    let files = plan(tmp.path());
    let err = write_agent_setup(&files).unwrap_err();
    assert!(err.to_string().contains("Could not create"), "{err}");
}

#[cfg(unix)]
unsafe extern "C" {
    fn geteuid() -> u32;
}

#[cfg(unix)]
#[test]
fn read_only_repo() {
    use std::os::unix::fs::PermissionsExt;
    // SAFETY: geteuid has no preconditions
    if unsafe { geteuid() } == 0 {
        return; // root writes anywhere
    }
    let tmp = tempfile::tempdir().unwrap();
    let files = plan(tmp.path());
    std::fs::set_permissions(tmp.path(), std::fs::Permissions::from_mode(0o555)).unwrap();
    let result = write_agent_setup(&files);
    let left = std::fs::read_dir(tmp.path()).unwrap().count();
    std::fs::set_permissions(tmp.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
    let err = result.unwrap_err();
    assert!(err.to_string().contains("Could not write"), "{err}");
    assert_eq!(left, 0);
}
