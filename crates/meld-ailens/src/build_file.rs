//! Build files: rendering, reading their header back, and `index.md` (port
//! of `build_file.py`)
//!
//! The output is byte-identical to what AI Lens writes for the same facts,
//! so a repository can mix builds from both. See `docs/build-format.md` in
//! AI Lens for the format.

use std::cmp::Ordering;
use std::collections::HashMap;
use std::sync::LazyLock;

use regex::Regex;

use crate::build_facts::{
    BuildAgent, BuildCommit, BuildFacts, BuildFile, BuildFileEntry, BuildRevision,
};
use crate::build_narrative::{
    BUILD_PROMPT_VERSION, BuildNarrative, JS_SPACE_CLASS, is_js_space, js_trim, one_line,
};
use crate::models::{ComparisonRelation, ExplanationSource, LineRange};

pub const BUILD_FORMAT_VERSION: u32 = 2;

const MINUS: char = '\u{2212}';
const EN_DASH: char = '\u{2013}';

fn inferred_label(model: Option<&str>) -> String {
    format!(
        "Why (inferred by {}, not the author’s words)",
        model.unwrap_or("a model")
    )
}

pub fn build_file_name(facts: &BuildFacts) -> String {
    format!(
        "{}_{}_build.md",
        facts.base.short_sha, facts.target.short_sha
    )
}

/// `created:` as AI Lens writes it: UTC, to the second (`created` is
/// seconds since the epoch)
pub fn format_created(created: f64) -> String {
    crate::time::format_utc(created)
}

fn first_chars(text: &str, count: usize) -> &str {
    crate::claude::prefix(text, count)
}

/// The build file
///
/// YAML frontmatter, then Summary, Decisions, Commits, Changes and Risks.
/// Everything except the narrative and `created` (seconds since the epoch)
/// comes from `facts`, in a fixed order, so the same comparison renders the
/// same way. It holds no diff text: each commit's explanation is given once
/// under Commits, and changes are line ranges, with what each one does and
/// why when the commit's changes were explained one by one.
pub fn render_build(
    facts: &BuildFacts,
    narrative: &BuildNarrative,
    created: f64,
    generator: &str,
) -> String {
    let commits: HashMap<&str, &BuildCommit> =
        facts.commits.iter().map(|c| (c.sha.as_str(), c)).collect();
    let stats = &facts.stats;

    let mut lines: Vec<String> = vec![
        "---".into(),
        format!("ai_lens: {BUILD_FORMAT_VERSION}"),
        format!("title: {}", yaml_string(&narrative.title)),
    ];
    lines.extend(yaml_revision("base", &facts.base));
    lines.extend(yaml_revision("target", &facts.target));
    lines.extend([
        format!(
            "merge_base: {}",
            facts.merge_base.as_deref().unwrap_or("null")
        ),
        format!("relation: {}", facts.relation),
        format!("created: {}", format_created(created)),
        format!("build_model: {}", yaml_string(&narrative.model)),
        format!("generator: {}", yaml_string(generator)),
        format!("prompt_version: {BUILD_PROMPT_VERSION}"),
        format!(
            "stats: {{ files: {}, insertions: {}, deletions: {}, commits: {} }}",
            stats.files, stats.insertions, stats.deletions, stats.commits
        ),
        if facts.commits.is_empty() {
            "commits: []".into()
        } else {
            "commits:".into()
        },
    ]);
    for commit in &facts.commits {
        lines.extend(yaml_commit(commit));
    }

    let mut revisions = format!(
        "{} → {}",
        describe_revision(&facts.base),
        describe_revision(&facts.target)
    );
    if facts.relation == ComparisonRelation::Diverged
        && let Some(merge_base) = &facts.merge_base
    {
        let merge_base = first_chars(merge_base, facts.base.short_sha.chars().count());
        revisions.push_str(&format!(" · changes since the merge base `{merge_base}`"));
    }

    lines.extend([
        "---".into(),
        String::new(),
        format!("# {}", narrative.title),
        String::new(),
        revisions,
        String::new(),
    ]);
    lines.extend([
        "## Summary".into(),
        String::new(),
        narrative.summary.clone(),
        String::new(),
    ]);

    lines.extend(["## Decisions".into(), String::new()]);
    if narrative.decisions.is_empty() {
        lines.push("- None recorded.".into());
    }
    for decision in &narrative.decisions {
        let cited = if decision.commits.is_empty() {
            String::new()
        } else {
            format!(
                " ({})",
                decision
                    .commits
                    .iter()
                    .map(|sha| format!("`{sha}`"))
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        };
        lines.push(format!("- {}{cited}", decision.text));
    }
    lines.push(String::new());

    lines.push("## Commits".into());
    if facts.commits.is_empty() {
        lines.extend([String::new(), "- None.".into()]);
    }
    for commit in &facts.commits {
        lines.push(String::new());
        lines.extend(render_commit(commit));
    }
    lines.push(String::new());

    lines.push("## Changes".into());
    for file in &facts.files {
        lines.push(String::new());
        lines.extend(render_file(file, &commits));
    }
    lines.push(String::new());

    lines.extend(["## Risks / open questions".into(), String::new()]);
    if narrative.risks.is_empty() {
        lines.push("- None recorded.".into());
    }
    lines.extend(narrative.risks.iter().map(|risk| format!("- {risk}")));
    lines.push(String::new());

    lines.join("\n")
}

fn render_file(file: &BuildFile, commits: &HashMap<&str, &BuildCommit>) -> Vec<String> {
    let counts = match file.additions {
        None => "binary".to_string(),
        Some(additions) => format!("+{additions} {MINUS}{}", file.deletions.unwrap_or(0)),
    };
    let original = file
        .original_path
        .as_ref()
        .map_or_else(String::new, |original| {
            format!(" (from {})", escape_inline(original))
        });
    let heading = format!(
        "### {}{original} · {} · {counts}",
        escape_inline(&file.path),
        file.status
    );

    if file.ignored {
        return vec![
            heading,
            String::new(),
            "- Left out: generated, or matched by `.ailensignore`.".into(),
        ];
    }
    if file.binary || file.entries.is_empty() {
        let by: Vec<String> = file
            .touched_by
            .iter()
            .map(|sha| {
                let short = commits
                    .get(sha.as_str())
                    .map_or_else(|| first_chars(sha, 7).to_string(), |c| c.short_sha.clone());
                format!("`{short}`")
            })
            .collect();
        let kind = if file.binary {
            "Binary file"
        } else {
            "No line changes"
        };
        let changed_by = if by.is_empty() {
            String::new()
        } else {
            format!("; changed by {}", by.join(", "))
        };
        return vec![heading, String::new(), format!("- {kind}{changed_by}.")];
    }

    let mut lines = vec![heading, String::new()];
    for entry in &file.entries {
        lines.extend(render_entry(entry, commits));
    }
    lines
}

fn render_commit(commit: &BuildCommit) -> Vec<String> {
    let label = if commit.why_source == ExplanationSource::Inferred {
        inferred_label(commit.inferred_by.as_deref())
    } else {
        "Why".to_string()
    };
    let mut lines = vec![
        format!(
            "### `{}` {}",
            commit.short_sha,
            escape_inline(&one_line(&commit.what))
        ),
        String::new(),
        format!(
            "- {} · source: {}",
            describe_author(commit),
            commit.why_source
        ),
    ];
    match &commit.why {
        None => lines.push("- Why: not recorded.".into()),
        Some(why) => {
            lines.extend(render_why(&label, why, ""));
            lines.extend(render_justification(commit.justification.as_deref(), ""));
        }
    }
    lines
}

/// The Justification under a Why, when there is one
fn render_justification(justification: Option<&str>, indent: &str) -> Vec<String> {
    match justification.filter(|j| !js_trim(j).is_empty()) {
        Some(justification) => render_why("Justification", justification, indent),
        None => Vec::new(),
    }
}

fn render_entry(entry: &BuildFileEntry, commits: &HashMap<&str, &BuildCommit>) -> Vec<String> {
    let ranges: Vec<String> = entry
        .added
        .iter()
        .map(|r| format!("**{}**", format_range(r)))
        .chain(
            entry
                .deleted
                .iter()
                .map(|r| format!("base **{}**", format_range(r))),
        )
        .collect();
    let ranges = ranges.join(", ");
    let commit = entry.sha.as_deref().and_then(|sha| commits.get(sha));
    let Some(commit) = commit else {
        return vec![
            format!("- {ranges} · commit unknown"),
            "  - Why: git can’t tell which commit made this change.".into(),
        ];
    };

    let label = inferred_label(commit.changes_inferred_by.as_deref());
    let mut lines = vec![format!(
        "- {ranges} · `{}` · {}",
        commit.short_sha,
        escape_inline(&one_line(&commit.what))
    )];
    for change in entry.changes.iter().flatten() {
        lines.push(format!("  - What: {}", one_line(&change.what)));
        lines.extend(render_why(&label, &change.why, "  "));
        lines.extend(render_justification(change.justification.as_deref(), "  "));
    }
    lines
}

fn describe_author(commit: &BuildCommit) -> String {
    if commit.agent == BuildAgent::ClaudeCode {
        let model = commit
            .agent_model
            .as_ref()
            .map_or_else(String::new, |model| format!(" ({model})"));
        return format!("claude-code{model}");
    }
    format!("human ({})", escape_inline(&commit.author))
}

fn is_line_break(c: char) -> bool {
    matches!(c, '\n' | '\r' | '\u{2028}' | '\u{2029}')
}

/// `(?:^|(?<=[\n\r  ]))\s*[-*] |\n\s*\n` with JavaScript's `\s`
fn is_structured(text: &str) -> bool {
    let chars: Vec<char> = text.chars().collect();
    for start in 0..chars.len() {
        if start == 0 || is_line_break(chars[start - 1]) {
            let mut i = start;
            while i < chars.len() && is_js_space(chars[i]) {
                i += 1;
            }
            // Backtracking can only stop the spaces earlier, at a space,
            // which is never a list marker
            if i + 1 < chars.len() && matches!(chars[i], '-' | '*') && chars[i + 1] == ' ' {
                return true;
            }
        }
        if chars[start] == '\n' {
            let mut i = start + 1;
            while i < chars.len() && is_js_space(chars[i]) {
                if chars[i] == '\n' {
                    return true;
                }
                i += 1;
            }
        }
    }
    false
}

/// The Why (or Justification) as list items
///
/// Hard-wrapped prose joins into one line; a Why with its own lists or
/// paragraphs keeps them, indented under the label so it stays part of the
/// entry.
fn render_why(label: &str, why: &str, indent: &str) -> Vec<String> {
    let text = js_trim(why);
    if !is_structured(text) {
        return vec![format!("{indent}- {label}: {}", one_line(text))];
    }
    let mut lines = vec![format!("{indent}- {label}:")];
    lines.extend(text.split('\n').map(|line| {
        if js_trim(line).is_empty() {
            String::new()
        } else {
            format!("{indent}  {}", js_trim(line))
        }
    }));
    lines
}

fn yaml_revision(key: &str, revision: &BuildRevision) -> Vec<String> {
    let ref_value = revision
        .ref_name
        .as_deref()
        .map_or_else(|| "null".to_string(), yaml_string);
    vec![
        format!("{key}:"),
        format!("  sha: {}", revision.sha),
        format!("  ref: {ref_value}"),
    ]
}

fn yaml_commit(commit: &BuildCommit) -> Vec<String> {
    let mut lines = vec![format!("  - sha: {}", commit.sha)];
    if let Some(patch_id) = &commit.patch_id {
        lines.push(format!("    patch_id: {patch_id}"));
    }
    lines.push(format!("    author: {}", yaml_string(&commit.author)));
    lines.push(format!("    agent: {}", commit.agent.as_str()));
    if let Some(model) = &commit.agent_model {
        lines.push(format!("    agent_model: {}", yaml_string(model)));
    }
    lines.push(format!("    why_source: {}", commit.why_source));
    if let Some(inferred_by) = &commit.inferred_by {
        lines.push(format!("    inferred_by: {}", yaml_string(inferred_by)));
    }
    lines
}

/// `JSON.stringify(value)`, which is a valid YAML double-quoted scalar
///
/// Non-ASCII stays as is; control characters are escaped, exactly as
/// JavaScript does.
pub fn yaml_string(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for c in value.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0c}' => out.push_str("\\f"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn describe_revision(revision: &BuildRevision) -> String {
    let ref_text = revision
        .ref_name
        .as_ref()
        .map_or_else(String::new, |r| format!(" ({})", escape_inline(r)));
    format!("`{}`{ref_text}", revision.short_sha)
}

fn format_range(range: &LineRange) -> String {
    if range.start == range.end {
        format!("L{}", range.start)
    } else {
        format!("L{}{EN_DASH}{}", range.start, range.end)
    }
}

/// Keeps paths and names from being read as Markdown
fn escape_inline(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if matches!(c, '\\' | '`' | '*' | '_' | '[' | ']' | '|' | '<' | '>') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

// -- reading the header back --

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct HeaderRevision {
    pub sha: Option<String>,
    pub ref_name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeaderCommit {
    pub sha: String,
    pub patch_id: Option<String>,
}

impl HeaderCommit {
    pub fn new(sha: impl Into<String>, patch_id: Option<&str>) -> Self {
        Self {
            sha: sha.into(),
            patch_id: patch_id.map(str::to_string),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct BuildHeader {
    pub file_name: String,
    pub title: Option<String>,
    pub created: Option<String>,
    pub build_model: Option<String>,
    pub base: HeaderRevision,
    pub target: HeaderRevision,
    /// The commits the build covers, to find it again from a commit, also
    /// after a rebase (by patch ID)
    pub commits: Vec<HeaderCommit>,
}

impl BuildHeader {
    pub fn new(file_name: impl Into<String>) -> Self {
        Self {
            file_name: file_name.into(),
            ..Default::default()
        }
    }
}

const REST: &str = "([^\n\r\u{2028}\u{2029}]*)";

static TOP_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(r"\A([a-z_]+):{JS_SPACE_CLASS}*{REST}\z")).expect("valid regex")
});
static COMMIT_SHA_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(
        r"\A {{2}}- sha:{JS_SPACE_CLASS}*([0-9a-f]+){JS_SPACE_CLASS}*\z"
    ))
    .expect("valid regex")
});
static PATCH_ID_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(
        r"\A {{4}}patch_id:{JS_SPACE_CLASS}*([0-9a-f]+){JS_SPACE_CLASS}*\z"
    ))
    .expect("valid regex")
});
static NESTED_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(r"\A {{2}}(sha|ref):{JS_SPACE_CLASS}*{REST}\z")).expect("valid regex")
});

/// Reads the index fields back from a build file's frontmatter
///
/// Reads what [`render_build`] writes; anything else is skipped, and a file
/// without frontmatter yields a header with only its name.
pub fn parse_build_header(file_name: &str, text: &str) -> BuildHeader {
    let mut header = BuildHeader::new(file_name);
    let lines: Vec<&str> = text.split('\n').collect();
    if lines.first() != Some(&"---") {
        return header;
    }

    #[derive(Clone, Copy)]
    enum Block {
        Base,
        Target,
    }
    let mut block: Option<Block> = None;
    for line in &lines[1..] {
        if *line == "---" {
            break;
        }

        if let Some(top) = TOP_RE.captures(line) {
            let key = &top[1];
            let value = &top[2];
            block = match key {
                "base" => Some(Block::Base),
                "target" => Some(Block::Target),
                _ => None,
            };
            match key {
                "title" => header.title = yaml_value(value),
                "created" => header.created = yaml_value(value),
                "build_model" => header.build_model = yaml_value(value),
                _ => {}
            }
            continue;
        }

        if let Some(commit) = COMMIT_SHA_RE.captures(line) {
            header.commits.push(HeaderCommit::new(&commit[1], None));
            continue;
        }

        if let Some(patch_id) = PATCH_ID_RE.captures(line)
            && let Some(last) = header.commits.last_mut()
        {
            last.patch_id = Some(patch_id[1].to_string());
            continue;
        }

        if let (Some(nested), Some(block)) = (NESTED_RE.captures(line), block) {
            let target = match block {
                Block::Base => &mut header.base,
                Block::Target => &mut header.target,
            };
            let value = yaml_value(&nested[2]);
            if &nested[1] == "sha" {
                target.sha = value;
            } else {
                target.ref_name = value;
            }
        }
    }
    header
}

fn yaml_value(value: &str) -> Option<String> {
    let trimmed = js_trim(value);
    if trimmed.is_empty() || trimmed == "null" {
        return None;
    }
    if !trimmed.starts_with('"') {
        return Some(trimmed.to_string());
    }
    serde_json::from_str::<String>(trimmed).ok()
}

// -- index.md --

const PUNCTUATION_ORDER: &str = " _-,;:!?.'\"()[]{}@*/\\&#%`^+<=>|~$";

#[derive(PartialEq, Eq, PartialOrd, Ord)]
struct Primary(u8, u32, String);

fn collation_key(text: &str) -> (Vec<Primary>, Vec<bool>, &str) {
    let primary = |c: char| -> Primary {
        if c.is_alphabetic() {
            return Primary(3, 0, c.to_lowercase().collect());
        }
        if c.is_numeric() {
            return Primary(2, 0, c.to_string());
        }
        if let Some(position) = PUNCTUATION_ORDER.chars().position(|p| p == c) {
            return Primary(0, position as u32, c.to_string());
        }
        Primary(1, c as u32, c.to_string())
    };
    // Lowercase before uppercase when nothing else differs
    (
        text.chars().map(primary).collect(),
        text.chars().map(char::is_uppercase).collect(),
        text,
    )
}

/// An approximation of JavaScript's `a.localeCompare(b)`
///
/// Close enough to ICU's root collation for the ISO dates and file names
/// it sorts: punctuation before digits before letters, case-insensitive
/// first.
pub fn locale_compare(a: &str, b: &str) -> Ordering {
    collation_key(a).cmp(&collation_key(b))
}

/// Newest `created` first, then by file name
pub fn newest_first(a: &BuildHeader, b: &BuildHeader) -> Ordering {
    locale_compare(
        b.created.as_deref().unwrap_or(""),
        a.created.as_deref().unwrap_or(""),
    )
    .then_with(|| locale_compare(&a.file_name, &b.file_name))
}

static CELL_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!("{JS_SPACE_CLASS}*\n{JS_SPACE_CLASS}*")).expect("valid regex")
});

fn cell(text: &str) -> String {
    CELL_RE
        .replace_all(&text.replace('|', "\\|"), " ")
        .into_owned()
}

fn side(revision: &HeaderRevision) -> String {
    if let Some(ref_name) = &revision.ref_name {
        return ref_name.clone();
    }
    if let Some(sha) = &revision.sha {
        return first_chars(sha, 7).to_string();
    }
    "?".into()
}

/// `index.md`: one row per build, newest first
pub fn render_index(headers: &[BuildHeader]) -> String {
    let mut sorted: Vec<&BuildHeader> = headers.iter().collect();
    sorted.sort_by(|a, b| newest_first(a, b));
    let rows: Vec<String> = sorted
        .iter()
        .map(|header| {
            let name = cell(
                header
                    .file_name
                    .strip_suffix("_build.md")
                    .unwrap_or(&header.file_name),
            );
            let sides = cell(&format!(
                "{} → {}",
                side(&header.base),
                side(&header.target)
            ));
            let created = header
                .created
                .as_deref()
                .map_or("", |created| first_chars(created, 10));
            format!(
                "| [{name}]({}) | {sides} | {created} | {} | {} |",
                header.file_name,
                cell(header.build_model.as_deref().unwrap_or("")),
                cell(header.title.as_deref().unwrap_or(""))
            )
        })
        .collect();

    let mut lines: Vec<String> = vec![
        "# AI Lens builds".into(),
        String::new(),
        "Each build explains why the code changed between two commits: what every commit did, why, and which lines".into(),
        "it touched. Read the build that covers the code you are about to change. Newest first.".into(),
        String::new(),
        "| Build | Base → Target | Created | Model | Summary |".into(),
        "| ----- | ------------- | ------- | ----- | ------- |".into(),
    ];
    lines.extend(rows);
    lines.push(String::new());
    lines.join("\n")
}
