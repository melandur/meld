//! Translated forms of the texts `meld_vc` builds in English: backend
//! choices, the push summary, the "Extra" column and comparison labels.
//! Texts that don't have the expected shape are shown as they are.

use meld_vc::view::VcAvailability;
use meld_vc::{VcChoice, VcChoices, VcKind};

/// A backend's display name (`None` for the null backend).
pub fn vc_name(kind: VcKind) -> &'static str {
    match kind {
        VcKind::Null => tr!("None"),
        kind => kind.name(),
    }
}

/// A backend chooser entry: "Git", "Git (not found)"…
pub fn choice_label(choice: &VcChoice) -> String {
    let name = vc_name(choice.kind);
    match choice.availability {
        VcAvailability::Available => name.to_string(),
        VcAvailability::NotFound => tr!("{name} (not found)", name = name),
        VcAvailability::NotInstalled => tr!(
            "{name} ({cmd} not installed)",
            name = name,
            cmd = choice.kind.cmd().unwrap_or_default()
        ),
        VcAvailability::InvalidRepository => tr!("{name} (invalid repository)", name = name),
    }
}

/// The backend chooser's tooltip.
pub fn choices_tooltip(choices: &VcChoices) -> &'static str {
    if choices
        .choices
        .iter()
        .any(|c| c.enabled() && c.kind != VcKind::Null)
    {
        tr!("Choose which version control system to use")
    } else {
        tr!("No valid version control system found in this folder")
    }
}

/// "3 unpushed commits in 2 branches" (`get_commits_to_push_summary`).
pub fn push_summary(summary: &str) -> String {
    let parse = || -> Option<String> {
        let (count, rest) = summary.split_once(' ')?;
        let commits: usize = count.parse().ok()?;
        let rest = rest
            .strip_prefix("unpushed commits")
            .or_else(|| rest.strip_prefix("unpushed commit"))?;
        let commits_text = trn!(
            "{} unpushed commit",
            "{} unpushed commits",
            commits,
            commits
        );
        if rest.is_empty() {
            return Some(commits_text);
        }
        let branches: usize = rest
            .strip_prefix(" in ")?
            .strip_suffix(" branches")?
            .parse()
            .ok()?;
        let branches_text = trn!("{} branch", "{} branches", branches, branches);
        Some(tr!(
            "{unpushed_commits} in {unpushed_branches}",
            unpushed_commits = commits_text,
            unpushed_branches = branches_text
        ))
    };
    parse().unwrap_or_else(|| summary.to_string())
}

/// The "Extra" column: "Staged", "Rev 12", "Mode changed from … to …",
/// several joined with "; ".
pub fn extra(options: &str) -> String {
    if options.is_empty() {
        return String::new();
    }
    options
        .split("; ")
        .map(|item| match item {
            "Staged" => tr!("Staged").to_string(),
            "Partially staged" => tr!("Partially staged").to_string(),
            _ => {
                if let Some((old, new)) = item
                    .strip_prefix("Mode changed from ")
                    .and_then(|r| r.split_once(" to "))
                {
                    tr!(
                        "Mode changed from {old_mode} to {new_mode}",
                        old_mode = old,
                        new_mode = new
                    )
                } else if let Some(rev) = item.strip_prefix("Rev ") {
                    tr!("Rev {}", rev)
                } else {
                    item.to_string()
                }
            }
        })
        .collect::<Vec<_>>()
        .join("; ")
}

/// Translates a label's base name into one of its forms.
type Translate = fn(&str) -> String;

/// A comparison's pane or tab label from `prepare_diff`: "a.txt — local",
/// "a.txt (working, repository)"…
pub fn diff_label(label: &str) -> String {
    let forms: [(&str, Translate); 7] = [
        (" — repository", |b| tr!("{} — repository", b)),
        (" — local", |b| tr!("{} — local", b)),
        (" — remote", |b| tr!("{} — remote", b)),
        (" (working, repository)", |b| {
            tr!("{} (working, repository)", b)
        }),
        (" (repository, working)", |b| {
            tr!("{} (repository, working)", b)
        }),
        (" (local, merge, remote)", |b| {
            tr!("{} (local, merge, remote)", b)
        }),
        (" (remote, merge, local)", |b| {
            tr!("{} (remote, merge, local)", b)
        }),
    ];
    for (suffix, translate) in forms {
        if let Some(base) = label.strip_suffix(suffix) {
            return translate(base);
        }
    }
    label.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    // Under an English locale the texts come back unchanged
    #[test]
    fn library_texts_round_trip() {
        if crate::i18n::language().is_some() {
            return;
        }
        for s in [
            "1 unpushed commit",
            "3 unpushed commits",
            "3 unpushed commits in 2 branches",
            "odd",
        ] {
            assert_eq!(push_summary(s), s);
        }
        for s in [
            "Staged",
            "Mode changed from 000000 to 100644; Staged",
            "Rev 3",
            "other",
        ] {
            assert_eq!(extra(s), s);
        }
        for s in ["a — local", "a.txt (repository, working)", "plain"] {
            assert_eq!(diff_label(s), s);
        }
    }
}
