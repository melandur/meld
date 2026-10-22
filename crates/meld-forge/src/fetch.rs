//! Fetching a pull request's commits into private refs
//!
//! The head is fetched from the forge's own ref (`refs/pull/<n>/head` on
//! GitHub, `refs/merge-requests/<n>/head` on GitLab), which outlives the
//! branch, into `refs/meld/pr/<remote>/<n>/head`; the base branch into
//! `…/base`. The user's branches and remote-tracking branches are never
//! touched. The comparison's base is where the pull request branched off.

use meld_ailens::git::RunOptions;
use meld_ailens::{CancelToken, GitRepository};

use crate::error::{Error, Result};
use crate::models::{FetchedPullRequest, ForgeKind, ForgeRemote, PrState, PullRequestDetails};

/// Where Meld keeps a pull request's commits
pub const REF_PREFIX: &str = "refs/meld/pr/";

/// `refs/meld/pr/<remote>/<number>/<which>`
pub fn pr_ref(remote: &str, number: u64, which: &str) -> String {
    format!("{REF_PREFIX}{remote}/{number}/{which}")
}

/// The remote and number of a ref made by [`pr_ref`] for a head
pub fn parse_pr_ref(name: &str) -> Option<(String, u64)> {
    let rest = name.strip_prefix(REF_PREFIX)?.strip_suffix("/head")?;
    let (remote, number) = rest.rsplit_once('/')?;
    if remote.is_empty() {
        return None;
    }
    Some((remote.to_string(), number.parse().ok()?))
}

/// The forge's ref that keeps a pull request's head
pub fn head_source(kind: ForgeKind, number: u64) -> String {
    match kind {
        ForgeKind::GitHub => format!("refs/pull/{number}/head"),
        ForgeKind::GitLab => format!("refs/merge-requests/{number}/head"),
    }
}

fn fetch(repo: &GitRepository, remote: &str, specs: &[String], cancel: &CancelToken) -> Result<()> {
    let mut args: Vec<String> = [
        "fetch",
        "--no-tags",
        "--no-write-fetch-head",
        "--no-recurse-submodules",
        "--no-auto-gc",
        // Don't update remote-tracking branches along the way
        "--refmap=",
        "--quiet",
        "--end-of-options",
    ]
    .iter()
    .map(|a| a.to_string())
    .collect();
    args.push(remote.to_string());
    args.extend(specs.iter().cloned());
    repo.run(&args, cancel)
        .map(|_| ())
        .map_err(|err| match err {
            meld_ailens::Error::Cancelled => Error::Cancelled,
            meld_ailens::Error::Git { stderr, .. } if !stderr.trim().is_empty() => {
                Error::Git(format!("Fetching from {remote} failed: {}", stderr.trim()))
            }
            other => Error::Git(format!("Fetching from {remote} failed: {other}")),
        })
}

fn resolve(repo: &GitRepository, name: &str, cancel: &CancelToken) -> Result<Option<String>> {
    Ok(repo.resolve_commit(name, cancel)?)
}

/// Makes `sha` available, fetching it into `dst` when it isn't here yet;
/// false when the forge won't hand it out
fn ensure_commit(
    repo: &GitRepository,
    remote: &str,
    sha: &str,
    dst: &str,
    cancel: &CancelToken,
) -> Result<bool> {
    if resolve(repo, sha, cancel)?.is_some() {
        return Ok(true);
    }
    match fetch(repo, remote, &[format!("+{sha}:{dst}")], cancel) {
        Ok(()) => Ok(resolve(repo, sha, cancel)?.is_some()),
        Err(Error::Cancelled) => Err(Error::Cancelled),
        Err(err) => {
            log::info!("Can't fetch {sha}: {err}");
            Ok(false)
        }
    }
}

/// Fetches the pull request and finds the commit to compare its head with
pub fn fetch_pull_request(
    repo: &GitRepository,
    remote: &ForgeRemote,
    details: &PullRequestDetails,
    cancel: &CancelToken,
    progress: &dyn Fn(&str),
) -> Result<FetchedPullRequest> {
    let number = details.pr.number;
    let reference = remote.kind.reference(number);
    let head_ref = pr_ref(&remote.name, number, "head");
    let base_ref = pr_ref(&remote.name, number, "base");
    progress(&format!("Fetching {reference} from {}…", remote.name));
    let head_spec = format!("+{}:{head_ref}", head_source(remote.kind, number));
    let base_branch = details.pr.base_branch.trim();
    let mut have_base = false;
    if base_branch.is_empty() {
        fetch(repo, &remote.name, std::slice::from_ref(&head_spec), cancel)?;
    } else {
        let base_spec = format!("+refs/heads/{base_branch}:{base_ref}");
        match fetch(repo, &remote.name, &[head_spec.clone(), base_spec], cancel) {
            Ok(()) => have_base = true,
            Err(Error::Cancelled) => return Err(Error::Cancelled),
            // The base branch may be gone
            Err(_) => fetch(repo, &remote.name, std::slice::from_ref(&head_spec), cancel)?,
        }
    }
    let head = resolve(repo, &head_ref, cancel)?
        .ok_or_else(|| Error::Git(format!("{} has no {reference}.", remote.name)))?;

    progress(&format!("Finding where {reference} branched off…"));
    let base = base_commit(
        repo,
        remote,
        details,
        have_base.then_some(base_ref.as_str()),
        &head,
        cancel,
    )?;
    Ok(FetchedPullRequest {
        base,
        head,
        head_ref,
    })
}

fn base_commit(
    repo: &GitRepository,
    remote: &ForgeRemote,
    details: &PullRequestDetails,
    base_ref: Option<&str>,
    head: &str,
    cancel: &CancelToken,
) -> Result<String> {
    let number = details.pr.number;
    let merged = details.pr.state == PrState::Merged;

    // GitLab says what the merge base is
    if remote.kind == ForgeKind::GitLab
        && let Some(sha) = &details.base_sha
        && ensure_commit(
            repo,
            &remote.name,
            sha,
            &pr_ref(&remote.name, number, "merge-base"),
            cancel,
        )?
        && repo.is_ancestor(sha, head, cancel)?
    {
        return Ok(sha.clone());
    }

    // Once merged, the base branch contains the head: compare with where
    // the base branch was when it was merged
    if merged
        && let Some(merge) = &details.merge_commit
        && ensure_commit(
            repo,
            &remote.name,
            merge,
            &pr_ref(&remote.name, number, "merge"),
            cancel,
        )?
        && let Some(parent) = resolve(repo, &format!("{merge}^1"), cancel)?
        && let Some(merge_base) = repo.merge_base(&parent, head, cancel)?
        && merge_base != head
    {
        return Ok(merge_base);
    }

    let mut fallback = None;
    let mut candidates: Vec<String> = base_ref.map(str::to_string).into_iter().collect();
    if let Some(sha) = &details.base_sha
        && ensure_commit(
            repo,
            &remote.name,
            sha,
            &pr_ref(&remote.name, number, "base-sha"),
            cancel,
        )?
    {
        candidates.push(sha.clone());
    }
    for candidate in candidates {
        if let Some(merge_base) = repo.merge_base(&candidate, head, cancel)? {
            if merge_base != head {
                return Ok(merge_base);
            }
            fallback.get_or_insert(merge_base);
        }
    }
    // Merged without a merge commit we can see: an empty comparison is
    // still better than none
    fallback.ok_or_else(|| {
        Error::Git(format!(
            "Couldn’t find where {} branched off {}.",
            remote.kind.reference(number),
            if details.pr.base_branch.is_empty() {
                "its base"
            } else {
                details.pr.base_branch.as_str()
            }
        ))
    })
}

/// Deletes the refs Meld keeps for a pull request
pub fn delete_refs(
    repo: &GitRepository,
    remote: &str,
    number: u64,
    cancel: &CancelToken,
) -> Result<()> {
    let prefix = format!("{REF_PREFIX}{remote}/{number}/");
    let out = repo.run(&["for-each-ref", "--format=%(refname)", &prefix], cancel)?;
    let input: String = String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|l| l.starts_with(&prefix))
        .map(|l| format!("delete {l}\n"))
        .collect();
    if !input.is_empty() {
        repo.run_with(
            &["update-ref", "--stdin"],
            RunOptions {
                input: Some(input.as_bytes()),
                ..Default::default()
            },
            cancel,
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::PullRequest;
    use meld_ailens::GitOptions;
    use std::path::Path;

    fn git(dir: &Path, args: &[&str]) -> String {
        let out = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .env("GIT_AUTHOR_NAME", "T")
            .env("GIT_AUTHOR_EMAIL", "t@example.com")
            .env("GIT_COMMITTER_NAME", "T")
            .env("GIT_COMMITTER_EMAIL", "t@example.com")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    fn commit(dir: &Path, file: &str, text: &str) -> String {
        std::fs::write(dir.join(file), text).unwrap();
        git(dir, &["add", file]);
        git(dir, &["commit", "-q", "--no-gpg-sign", "-m", text]);
        git(dir, &["rev-parse", "HEAD"])
    }

    /// A "forge" repository with a pull request ref, and a clone of it
    /// whose remote URL claims to be on github.com
    fn setup() -> (tempfile::TempDir, String, String, String) {
        let tmp = tempfile::tempdir().unwrap();
        let forge = tmp.path().join("forge");
        std::fs::create_dir(&forge).unwrap();
        git(&forge, &["init", "-q", "-b", "main"]);
        let fork_point = commit(&forge, "a.txt", "one\n");
        git(&forge, &["checkout", "-q", "-b", "feature"]);
        let head = commit(&forge, "a.txt", "one\ntwo\n");
        git(&forge, &["update-ref", "refs/pull/7/head", &head]);
        git(&forge, &["checkout", "-q", "main"]);
        commit(&forge, "b.txt", "later\n");
        // The branch is deleted; the pull request's ref stays
        git(&forge, &["branch", "-q", "-D", "feature"]);

        let local = tmp.path().join("local");
        std::fs::create_dir(&local).unwrap();
        git(&local, &["init", "-q", "-b", "main"]);
        git(
            &local,
            &[
                "remote",
                "add",
                "github",
                "https://github.com/test/demo.git",
            ],
        );
        let url = format!("url.{}.insteadOf", forge.display());
        git(
            &local,
            &["config", &url, "https://github.com/test/demo.git"],
        );
        (tmp, fork_point, head, local.display().to_string())
    }

    fn details(state: PrState) -> PullRequestDetails {
        PullRequestDetails {
            pr: PullRequest {
                number: 7,
                state,
                base_branch: "main".into(),
                ..Default::default()
            },
            ..Default::default()
        }
    }

    #[test]
    fn fetches_into_private_refs_and_finds_the_fork_point() {
        let (_tmp, fork_point, head, local) = setup();
        let repo = GitRepository::new(&local, &GitOptions::default());
        let remotes = crate::remote::forge_remotes(&repo, &CancelToken::new()).unwrap();
        assert_eq!(remotes.len(), 1, "insteadOf doesn't hide the forge");
        let seen = std::sync::Mutex::new(Vec::new());
        let progress = |text: &str| seen.lock().unwrap().push(text.to_string());
        let fetched = fetch_pull_request(
            &repo,
            &remotes[0],
            &details(PrState::Closed),
            &CancelToken::new(),
            &progress,
        )
        .unwrap();
        assert_eq!(fetched.head, head);
        assert_eq!(fetched.base, fork_point);
        assert_eq!(fetched.head_ref, "refs/meld/pr/github/7/head");
        assert_eq!(seen.lock().unwrap()[0], "Fetching #7 from github…");
        // No branch or remote-tracking branch was made
        let refs = git(Path::new(&local), &["for-each-ref", "--format=%(refname)"]);
        assert_eq!(
            refs,
            "refs/meld/pr/github/7/base\nrefs/meld/pr/github/7/head"
        );
        delete_refs(&repo, "github", 7, &CancelToken::new()).unwrap();
        assert_eq!(git(Path::new(&local), &["for-each-ref"]), "");
    }

    #[test]
    fn merged_pull_requests_compare_with_the_base_before_the_merge() {
        let (tmp, fork_point, head, local) = setup();
        let forge = tmp.path().join("forge");
        git(
            &forge,
            &[
                "merge",
                "-q",
                "--no-ff",
                "--no-gpg-sign",
                "-m",
                "Merge",
                &head,
            ],
        );
        let merge = git(&forge, &["rev-parse", "HEAD"]);
        let repo = GitRepository::new(&local, &GitOptions::default());
        let remote = crate::remote::forge_remotes(&repo, &CancelToken::new())
            .unwrap()
            .remove(0);
        let mut d = details(PrState::Merged);
        d.merge_commit = Some(merge);
        let fetched = fetch_pull_request(&repo, &remote, &d, &CancelToken::new(), &|_| {}).unwrap();
        assert_eq!(fetched.base, fork_point);
        // Without the merge commit, the base branch now contains the head
        d.merge_commit = None;
        let fetched = fetch_pull_request(&repo, &remote, &d, &CancelToken::new(), &|_| {}).unwrap();
        assert_eq!(fetched.base, head);
    }

    #[test]
    fn a_missing_pull_request_is_an_error() {
        let (_tmp, _, _, local) = setup();
        let repo = GitRepository::new(&local, &GitOptions::default());
        let remote = crate::remote::forge_remotes(&repo, &CancelToken::new())
            .unwrap()
            .remove(0);
        let mut d = details(PrState::Open);
        d.pr.number = 99;
        let err = fetch_pull_request(&repo, &remote, &d, &CancelToken::new(), &|_| {}).unwrap_err();
        assert!(
            err.to_string().contains("Fetching from github failed"),
            "{err}"
        );
    }

    #[test]
    fn pr_refs_round_trip() {
        assert_eq!(pr_ref("origin", 5, "head"), "refs/meld/pr/origin/5/head");
        assert_eq!(
            parse_pr_ref("refs/meld/pr/my/fork/5/head"),
            Some(("my/fork".into(), 5))
        );
        assert_eq!(parse_pr_ref("refs/meld/pr/origin/5/base"), None);
        assert_eq!(parse_pr_ref("refs/heads/x"), None);
        assert_eq!(
            head_source(ForgeKind::GitLab, 3),
            "refs/merge-requests/3/head"
        );
    }
}
