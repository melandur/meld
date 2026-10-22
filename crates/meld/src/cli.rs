//! Command-line handling, matching the options of the Python `meld` launcher.

use std::path::{Path, PathBuf};

use clap::Parser;

use crate::comparison::{Comparison, PrSpec};

/// The text after the options in `--help` (`meldapp.py`'s usage).
fn usage_epilog() -> String {
    let (file, folder) = (tr!("file"), tr!("folder"));
    let rows = [
        ("meld".to_string(), tr!("Start with an empty window")),
        (format!("meld <{file}|{folder}>"), tr!("Start a version control comparison")),
        (format!("meld <{file}> <{file}> [<{file}>]"), tr!("Start a 2- or 3-way file comparison")),
        (format!("meld <{folder}> <{folder}> [<{folder}>]"), tr!("Start a 2- or 3-way folder comparison")),
        ("meld --git [<path>]".to_string(), tr!("Compare two revisions of a Git repository")),
        ("meld --git --pr <number> [<path>]".to_string(), tr!("Review a pull or merge request")),
    ];
    let width = rows.iter().map(|(u, _)| u.chars().count()).max().unwrap_or(0);
    let mut out = format!("{}\n\n{}\n", tr!("Meld is a file and directory comparison tool."), tr!("Usage:"));
    for (usage, what) in rows {
        out.push_str(&format!("  {usage:width$} {what}\n"));
    }
    out.push('\n');
    out.push_str(tr!(
        "\n\nFor file comparisons, the special argument @blank may be used instead of\na <file> argument to create a blank pane."
    ).trim_start_matches('\n'));
    out
}

#[derive(Debug, Parser)]
#[command(name = "meld", version, about = tr!("Visual diff and merge tool"), after_help = usage_epilog())]
pub struct Args {
    #[arg(short = 'L', long = "label", action = clap::ArgAction::Append, help = tr!("Set label to use instead of file name"))]
    pub label: Vec<String>,

    #[arg(short = 'n', long = "newtab", help = tr!("Open a new tab in an already running instance"))]
    pub newtab: bool,

    #[arg(short = 'a', long = "auto-compare", help = tr!("Automatically compare all differing files on startup"))]
    pub auto_compare: bool,

    #[arg(short = 'u', long = "unified", help = tr!("Ignored for compatibility"))]
    pub unified: bool,

    #[arg(short = 'o', long = "output", value_name = "FILE", help = tr!("Set the target file for saving a merge result"))]
    pub output: Option<PathBuf>,

    #[arg(long = "auto-merge", help = tr!("Automatically merge files"))]
    pub auto_merge: bool,

    #[arg(long = "comparison-file", value_name = "FILE", help = tr!("Load a saved comparison from a Meld comparison file"))]
    pub comparison_file: Option<PathBuf>,

    #[arg(long = "diff", value_name = "PATH", num_args = 1..=3, help = tr!("Create a diff tab for the supplied files or folders"))]
    pub diff_help: Option<Vec<String>>,

    /// `--diff` groups, gathered by [`parse`] (clap cannot keep them apart).
    #[arg(skip)]
    pub diff: Vec<Vec<String>>,

    #[arg(
        long = "git",
        help = tr!("Compare two revisions of the Git repository at the given path (default: the current folder), with why each change was made")
    )]
    pub git: bool,

    #[arg(
        long = "base",
        value_name = "REV",
        help = tr!("With --git: the base revision; a branch, tag, SHA, “staged” or “working-tree” (default: HEAD)")
    )]
    pub base: Option<String>,

    #[arg(
        long = "target",
        value_name = "REV",
        help = tr!("With --git: the target revision; also “staged”, “unstaged” or “working-tree” (default: working-tree)")
    )]
    pub target: Option<String>,

    #[arg(
        long = "pr",
        value_name = "NUMBER",
        value_parser = parse_pr_number,
        help = tr!("With --git: review pull request (GitHub) or merge request (GitLab) NUMBER, comparing its merge base with its head")
    )]
    pub pr: Option<u64>,

    #[arg(
        long = "remote",
        value_name = "NAME",
        help = tr!("With --pr: the git remote whose forge has it (default: the remote chosen before, else the first with pull requests)")
    )]
    pub remote: Option<String>,

    #[arg(help = tr!("Files or folders to compare"))]
    pub paths: Vec<String>,
}

/// `12`, `#12` or `!12`.
fn parse_pr_number(text: &str) -> Result<u64, String> {
    let digits = text.trim().trim_start_matches(['#', '!']);
    digits
        .parse::<u64>()
        .ok()
        .filter(|n| *n > 0)
        .ok_or_else(|| tr!("“{text}” is not a pull request number", text = text))
}

/// Parses a command line (without the program name). `--diff` takes every
/// following argument up to the next option, like the Python launcher.
pub fn parse(args: impl IntoIterator<Item = String>) -> Result<Args, clap::Error> {
    let mut rest = vec!["meld".to_string()];
    let mut groups = Vec::new();
    let mut args = args.into_iter().peekable();
    while let Some(arg) = args.next() {
        if arg != "--diff" {
            rest.push(arg);
            continue;
        }
        let mut group = Vec::new();
        while let Some(next) = args.next_if(|a| !(a.starts_with("--") || (a.starts_with('-') && a.len() > 1))) {
            group.push(next);
        }
        if !(1..=3).contains(&group.len()) {
            use clap::CommandFactory;
            return Err(Args::command().error(
                clap::error::ErrorKind::WrongNumberOfValues,
                tr!("wrong number of arguments supplied to --diff"),
            ));
        }
        groups.push(group);
    }
    let mut parsed = Args::try_parse_from(rest)?;
    parsed.diff = groups;
    Ok(parsed)
}

/// What the command line asks the application to open.
#[derive(Debug, Clone, Default)]
pub struct Launch {
    pub comparisons: Vec<Comparison>,
    /// Ask an already running instance to open the comparisons.
    pub newtab: bool,
}

/// Turns parsed arguments into comparisons, validating them the way the
/// Python launcher does. Errors are user-facing messages.
pub fn launch_from_args(args: Args, cwd: &Path) -> Result<Launch, String> {
    let mut launch = Launch {
        newtab: args.newtab,
        ..Launch::default()
    };

    if args.paths.len() > 3 {
        return Err(tr!("too many arguments (wanted 0-3, got {})", args.paths.len()));
    }
    if args.auto_merge && args.paths.len() < 3 {
        return Err(tr!("can’t auto-merge less than 3 files").into());
    }
    if args.auto_merge && args.paths.iter().any(|p| resolve(cwd, p).is_dir()) {
        return Err(tr!("can’t auto-merge directories").into());
    }

    let comparison_file = args.comparison_file.clone().or_else(|| {
        (args.paths.len() == 1 && args.paths[0].ends_with(".meldcmp"))
            .then(|| PathBuf::from(&args.paths[0]))
    });
    if let Some(path) = comparison_file {
        let path = expand_user(&path);
        launch.comparisons.push(Comparison::SavedFile(resolve_path(cwd, &path)));
        return Ok(launch);
    }

    if args.remote.is_some() && args.pr.is_none() {
        return Err(tr!("--remote needs --pr").into());
    }
    if args.git || args.base.is_some() || args.target.is_some() || args.pr.is_some() {
        if args.paths.len() > 1 {
            return Err(tr!("--git takes at most one path").into());
        }
        if args.pr.is_some() && (args.base.is_some() || args.target.is_some()) {
            return Err(tr!("--pr compares the pull request’s own base and head; leave out --base and --target").into());
        }
        let location = match args.paths.first() {
            Some(p) => resolve(cwd, p),
            None => cwd.to_path_buf(),
        };
        launch.comparisons.push(Comparison::Git {
            location,
            base: args.base,
            target: args.target,
            pr: args.pr.map(|number| PrSpec {
                number,
                remote: args.remote,
            }),
        });
        return Ok(launch);
    }

    let groups = std::iter::once(args.paths.clone())
        .chain(args.diff.iter().cloned())
        .filter(|g| !g.is_empty());
    for (i, group) in groups.enumerate() {
        let primary = i == 0;
        let paths: Vec<Option<PathBuf>> = group
            .iter()
            .map(|p| (p != "@blank").then(|| resolve(cwd, p)))
            .collect();
        let mut comparison = Comparison::from_paths(paths, args.auto_compare)?;
        if primary {
            comparison.set_labels(&args.label);
            if let Some(out) = &args.output {
                comparison.set_merge_output(resolve_path(cwd, out));
            }
            if args.auto_merge {
                comparison.set_auto_merge();
            }
        }
        launch.comparisons.push(comparison);
    }
    Ok(launch)
}

fn resolve(cwd: &Path, arg: &str) -> PathBuf {
    let arg = arg.strip_prefix("file://").unwrap_or(arg);
    resolve_path(cwd, &expand_user(Path::new(arg)))
}

fn resolve_path(cwd: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        cwd.join(path)
    }
}

fn expand_user(path: &Path) -> PathBuf {
    match path.strip_prefix("~") {
        Ok(rest) => std::env::var_os("HOME")
            .map(|home| PathBuf::from(home).join(rest))
            .unwrap_or_else(|| path.to_path_buf()),
        Err(_) => path.to_path_buf(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Launch, String> {
        let args = super::parse(args.iter().map(|s| s.to_string())).map_err(|e| e.to_string())?;
        launch_from_args(args, Path::new("/work"))
    }

    #[test]
    fn no_arguments_opens_nothing() {
        assert!(parse(&[]).unwrap().comparisons.is_empty());
    }

    #[test]
    fn too_many_paths_is_an_error() {
        assert!(parse(&["a", "b", "c", "d"]).is_err());
    }

    #[test]
    fn auto_merge_needs_three_files() {
        assert!(parse(&["--auto-merge", "a", "b"]).is_err());
    }

    #[test]
    fn git_defaults_to_cwd() {
        let launch = parse(&["--git", "--base", "main"]).unwrap();
        match &launch.comparisons[0] {
            Comparison::Git {
                location,
                base,
                target,
                pr,
            } => {
                assert_eq!(location, Path::new("/work"));
                assert_eq!(base.as_deref(), Some("main"));
                assert!(target.is_none());
                assert!(pr.is_none());
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn pr_opens_a_pull_request_review() {
        let launch = parse(&["--git", "--pr", "#123", "repo"]).unwrap();
        match &launch.comparisons[0] {
            Comparison::Git { location, pr, .. } => {
                assert_eq!(location, Path::new("/work/repo"));
                assert_eq!(
                    pr,
                    &Some(PrSpec {
                        number: 123,
                        remote: None
                    })
                );
            }
            other => panic!("unexpected {other:?}"),
        }
        // --pr implies --git
        let launch = parse(&["--pr", "7", "--remote", "github"]).unwrap();
        assert!(matches!(
            &launch.comparisons[0],
            Comparison::Git { pr: Some(PrSpec { number: 7, remote: Some(r) }), .. } if r == "github"
        ));
        assert!(parse(&["--git", "--pr", "x"]).is_err());
        assert!(parse(&["--git", "--pr", "1", "--base", "main"]).is_err());
        assert!(parse(&["--git", "--remote", "origin"]).is_err());
    }

    #[test]
    fn git_takes_one_path() {
        assert!(parse(&["--git", "a", "b"]).is_err());
    }

    #[test]
    fn diff_option_adds_tabs() {
        let launch = parse(&["--diff", "x", "y", "--diff", "p", "q", "r"]).unwrap();
        assert_eq!(launch.comparisons.len(), 2);
    }

    #[test]
    fn blank_argument_is_an_empty_pane() {
        let launch = parse(&["@blank", "b"]).unwrap();
        match &launch.comparisons[0] {
            Comparison::Files { paths, .. } => {
                assert_eq!(paths[0], None);
                assert_eq!(paths[1].as_deref(), Some(Path::new("/work/b")));
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn meldcmp_argument_loads_saved_comparison() {
        let launch = parse(&["saved.meldcmp"]).unwrap();
        assert!(matches!(launch.comparisons[0], Comparison::SavedFile(_)));
    }
}
