//! Command-line handling, matching the options of the Python `meld` launcher.

use std::path::{Path, PathBuf};

use clap::Parser;

use crate::comparison::Comparison;

const USAGE_EPILOG: &str = "Meld is a file and directory comparison tool.\n\n\
    Usage:\n  meld                              Start with an empty window\n  \
    meld <file|folder>                Start a version control comparison\n  \
    meld <file> <file> [<file>]       Start a 2- or 3-way file comparison\n  \
    meld <folder> <folder> [<folder>] Start a 2- or 3-way folder comparison\n  \
    meld --git [<path>]               Compare two revisions of a Git repository\n\n\
    For file comparisons, the special argument @blank may be used instead of\n\
    a <file> argument to create a blank pane.";

#[derive(Debug, Parser)]
#[command(name = "meld", version, about = "Visual diff and merge tool", after_help = USAGE_EPILOG)]
pub struct Args {
    /// Set label to use instead of file name
    #[arg(short = 'L', long = "label", action = clap::ArgAction::Append)]
    pub label: Vec<String>,

    /// Open a new tab in an already running instance
    #[arg(short = 'n', long = "newtab")]
    pub newtab: bool,

    /// Automatically compare all differing files on startup
    #[arg(short = 'a', long = "auto-compare")]
    pub auto_compare: bool,

    /// Ignored for compatibility
    #[arg(short = 'u', long = "unified")]
    pub unified: bool,

    /// Set the target file for saving a merge result
    #[arg(short = 'o', long = "output", value_name = "FILE")]
    pub output: Option<PathBuf>,

    /// Automatically merge files
    #[arg(long = "auto-merge")]
    pub auto_merge: bool,

    /// Load a saved comparison from a Meld comparison file
    #[arg(long = "comparison-file", value_name = "FILE")]
    pub comparison_file: Option<PathBuf>,

    /// Create a diff tab for the supplied files or folders
    #[arg(long = "diff", value_name = "PATH", num_args = 1..=3)]
    pub diff_help: Option<Vec<String>>,

    /// `--diff` groups, gathered by [`parse`] (clap cannot keep them apart).
    #[arg(skip)]
    pub diff: Vec<Vec<String>>,

    /// Compare two revisions of the Git repository at the given path
    /// (default: the current folder), with why each change was made
    #[arg(long = "git")]
    pub git: bool,

    /// With --git: the base revision; a branch, tag, SHA, "staged" or
    /// "working-tree" (default: HEAD)
    #[arg(long = "base", value_name = "REV")]
    pub base: Option<String>,

    /// With --git: the target revision; also "staged", "unstaged" or
    /// "working-tree" (default: working-tree)
    #[arg(long = "target", value_name = "REV")]
    pub target: Option<String>,

    /// Files or folders to compare
    pub paths: Vec<String>,
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
                "wrong number of arguments supplied to --diff",
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
        return Err(format!(
            "too many arguments (wanted 0-3, got {})",
            args.paths.len()
        ));
    }
    if args.auto_merge && args.paths.len() < 3 {
        return Err("can’t auto-merge less than 3 files".into());
    }
    if args.auto_merge && args.paths.iter().any(|p| resolve(cwd, p).is_dir()) {
        return Err("can’t auto-merge directories".into());
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

    if args.git || args.base.is_some() || args.target.is_some() {
        if args.paths.len() > 1 {
            return Err("--git takes at most one path".into());
        }
        let location = match args.paths.first() {
            Some(p) => resolve(cwd, p),
            None => cwd.to_path_buf(),
        };
        launch.comparisons.push(Comparison::Git {
            location,
            base: args.base,
            target: args.target,
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
            Comparison::Git { location, base, target } => {
                assert_eq!(location, Path::new("/work"));
                assert_eq!(base.as_deref(), Some("main"));
                assert!(target.is_none());
            }
            other => panic!("unexpected {other:?}"),
        }
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
