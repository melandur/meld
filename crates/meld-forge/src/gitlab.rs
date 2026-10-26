//! GitLab merge requests through `glab api`
//!
//! Everything goes through the REST API of the remote's host
//! (`glab api --hostname <host> projects/<path>/merge_requests…`): lists,
//! details (with `diff_refs`, whose `base_sha` is the merge base), new
//! discussions with a position for line comments, notes for the overall
//! comment, `approve`.

use serde_json::{Value, json};

use meld_ailens::CancelToken;

use crate::error::{Error, Result};
use crate::github::typed_number;
use crate::json::{array, at, encode, flag, number, opt_text, parse_object, parse_pages, text};
use crate::models::*;
use crate::runner::Runner;

const KIND: ForgeKind = ForgeKind::GitLab;
const PAGE: usize = 100;
/// Most pages read of a list (comments of huge merge requests are cut)
const MAX_PAGES: usize = 20;

fn project(remote: &ForgeRemote) -> String {
    format!("projects/{}", encode(&remote.path))
}

fn get(
    runner: &dyn Runner,
    remote: &ForgeRemote,
    path: &str,
    cancel: &CancelToken,
) -> Result<Vec<u8>> {
    let args = vec![
        "api".to_string(),
        "--hostname".into(),
        remote.host.clone(),
        path.to_string(),
    ];
    runner.run(KIND, &args, None, cancel)
}

fn post(
    runner: &dyn Runner,
    remote: &ForgeRemote,
    path: &str,
    body: &Value,
    cancel: &CancelToken,
) -> Result<()> {
    let args = vec![
        "api".to_string(),
        "--hostname".into(),
        remote.host.clone(),
        "-X".into(),
        "POST".into(),
        "-H".into(),
        "Content-Type: application/json".into(),
        path.to_string(),
        "--input".into(),
        "-".into(),
    ];
    let input = serde_json::to_vec(body).unwrap_or_default();
    runner.run(KIND, &args, Some(&input), cancel)?;
    Ok(())
}

fn merge_request(v: &Value) -> PullRequest {
    let state = match text(v, "state").as_str() {
        "merged" => PrState::Merged,
        "closed" | "locked" => PrState::Closed,
        _ => PrState::Open,
    };
    let labels = array(v, "labels")
        .iter()
        .map(|l| match l {
            Value::String(s) => s.clone(),
            other => text(other, "name"),
        })
        .filter(|l| !l.is_empty())
        .collect();
    PullRequest {
        number: number(v, "iid").unwrap_or_default(),
        title: text(v, "title"),
        author: text(v, "author.username"),
        state,
        draft: flag(v, "draft") || flag(v, "work_in_progress"),
        updated_at: text(v, "updated_at"),
        url: text(v, "web_url"),
        head_branch: text(v, "source_branch"),
        base_branch: text(v, "target_branch"),
        head_sha: text(v, "sha"),
        labels,
    }
}

pub fn list(
    runner: &dyn Runner,
    remote: &ForgeRemote,
    filter: StateFilter,
    search: &str,
    limit: usize,
    cancel: &CancelToken,
) -> Result<Vec<PullRequest>> {
    if let Some(number) = typed_number(search) {
        return match details(runner, remote, number, cancel) {
            Ok(details) => Ok(vec![details.pr]),
            Err(Error::Command { auth: false, .. }) => Ok(Vec::new()),
            Err(err) => Err(err),
        };
    }
    let state = match filter {
        StateFilter::Open => "opened",
        StateFilter::Closed => "closed",
        StateFilter::Merged => "merged",
        StateFilter::All => "all",
    };
    let limit = limit.max(1);
    let per_page = limit.min(PAGE);
    let mut prs = Vec::new();
    for page in 1..=MAX_PAGES {
        let mut path = format!(
            "{}/merge_requests?state={state}&order_by=updated_at&sort=desc&per_page={per_page}&page={page}",
            project(remote)
        );
        if !search.trim().is_empty() {
            path.push_str(&format!("&search={}", encode(search.trim())));
        }
        let items = parse_pages("glab", &get(runner, remote, &path, cancel)?)?;
        let count = items.len();
        prs.extend(items.iter().map(merge_request));
        if count < per_page || prs.len() >= limit {
            break;
        }
    }
    prs.truncate(limit);
    Ok(prs)
}

pub fn details(
    runner: &dyn Runner,
    remote: &ForgeRemote,
    number: u64,
    cancel: &CancelToken,
) -> Result<PullRequestDetails> {
    let path = format!("{}/merge_requests/{number}", project(remote));
    let v = parse_object("glab", &get(runner, remote, &path, cancel)?)?;
    let diff_refs = at(&v, "diff_refs")
        .filter(|d| d.is_object())
        .map(|d| DiffRefs {
            base_sha: text(d, "base_sha"),
            start_sha: text(d, "start_sha"),
            head_sha: text(d, "head_sha"),
        });
    let mut pr = merge_request(&v);
    if pr.head_sha.is_empty()
        && let Some(refs) = &diff_refs
    {
        pr.head_sha = refs.head_sha.clone();
    }
    Ok(PullRequestDetails {
        pr,
        base_sha: diff_refs
            .as_ref()
            .map(|d| d.base_sha.clone())
            .filter(|s| !s.is_empty()),
        merge_commit: opt_text(&v, "merge_commit_sha")
            .or_else(|| opt_text(&v, "squash_commit_sha")),
        diff_refs,
    })
}

/// The JSON body of a new discussion on a line
pub fn line_comment_body(refs: &DiffRefs, comment: &DraftComment) -> Value {
    let old_path = comment
        .original_path
        .clone()
        .unwrap_or_else(|| comment.path.clone());
    let mut position = json!({
        "position_type": "text",
        "base_sha": refs.base_sha,
        "start_sha": refs.start_sha,
        "head_sha": refs.head_sha,
        "old_path": old_path,
        "new_path": comment.path,
    });
    match comment.side {
        Side::New => position["new_line"] = Value::from(comment.line),
        Side::Old => position["old_line"] = Value::from(comment.line),
    }
    json!({ "body": comment.body, "position": position })
}

pub fn submit_review(
    runner: &dyn Runner,
    remote: &ForgeRemote,
    details: &PullRequestDetails,
    review: &ReviewSubmission,
    cancel: &CancelToken,
) -> Result<()> {
    if let Some(problem) = review.problem(KIND) {
        return Err(Error::Invalid(problem));
    }
    let mr = format!("{}/merge_requests/{}", project(remote), details.pr.number);
    if !review.comments.is_empty() {
        let Some(refs) = details
            .diff_refs
            .as_ref()
            .filter(|r| !r.head_sha.is_empty())
        else {
            return Err(Error::Unsupported(
                "GitLab didn’t say which versions the merge request compares; line comments can’t be placed.".into(),
            ));
        };
        for (posted, comment) in review.comments.iter().enumerate() {
            let result = post(
                runner,
                remote,
                &format!("{mr}/discussions"),
                &line_comment_body(refs, comment),
                cancel,
            );
            if let Err(err) = result {
                return Err(partial(err, posted, review.comments.len()));
            }
        }
    }
    let posted = review.comments.len();
    if !review.body.trim().is_empty()
        && let Err(err) = post(
            runner,
            remote,
            &format!("{mr}/notes"),
            &json!({ "body": review.body }),
            cancel,
        )
    {
        return Err(partial(err, posted, posted));
    }
    if review.event == ReviewEvent::Approve {
        let mut body = json!({});
        if !details.pr.head_sha.is_empty() {
            // Only approve the version that was reviewed
            body["sha"] = Value::from(details.pr.head_sha.clone());
        }
        post(runner, remote, &format!("{mr}/approve"), &body, cancel)
            .map_err(|err| partial(err, posted, posted))?;
    }
    Ok(())
}

/// An error after some of a review was already posted
fn partial(err: Error, posted: usize, total: usize) -> Error {
    if posted == 0 || err.is_cancelled() {
        return err;
    }
    Error::Command {
        program: "glab".into(),
        message: format!("{posted} of {total} line comments were posted before this failed: {err}"),
        auth: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::ScriptedRunner;

    fn remote() -> ForgeRemote {
        crate::remote::parse_remote("origin", "https://gitlab.gnome.org/GNOME/meld.git").unwrap()
    }

    #[test]
    fn lists_merge_requests_page_by_page() {
        let one = r#"[{"iid":5,"title":"Port","author":{"username":"zed"},"state":"opened","draft":false,
                       "web_url":"u","source_branch":"port","target_branch":"main","sha":"h","labels":["a"]}]"#;
        let runner = ScriptedRunner::new().when("merge_requests?state=merged", one);
        let prs = list(
            &runner,
            &remote(),
            StateFilter::Merged,
            "port x",
            1,
            &CancelToken::new(),
        )
        .unwrap();
        assert_eq!(prs.len(), 1);
        assert_eq!(prs[0].number, 5);
        assert_eq!(prs[0].author, "zed");
        assert_eq!(prs[0].labels, ["a"]);
        let calls = runner.calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(
            calls[0].line(),
            "api --hostname gitlab.gnome.org projects/GNOME%2Fmeld/merge_requests?state=merged&order_by=updated_at&sort=desc&per_page=1&page=1&search=port%20x"
        );
    }

    #[test]
    fn details_have_the_merge_base() {
        let runner = ScriptedRunner::new().when(
            "merge_requests/5",
            r#"{"iid":5,"title":"T","description":"D","state":"merged","merge_commit_sha":"m",
                "diff_refs":{"base_sha":"b","start_sha":"s","head_sha":"h"},"head_pipeline":{"status":"success"}}"#,
        );
        let d = details(&runner, &remote(), 5, &CancelToken::new()).unwrap();
        assert_eq!(d.base_sha.as_deref(), Some("b"));
        assert_eq!(d.pr.head_sha, "h");
        assert_eq!(d.merge_commit.as_deref(), Some("m"));
        assert_eq!(d.pr.state, PrState::Merged);
    }

    fn mr() -> PullRequestDetails {
        PullRequestDetails {
            pr: PullRequest {
                number: 5,
                head_sha: "h".into(),
                ..Default::default()
            },
            diff_refs: Some(DiffRefs {
                base_sha: "b".into(),
                start_sha: "s".into(),
                head_sha: "h".into(),
            }),
            ..Default::default()
        }
    }

    #[test]
    fn reviews_post_positioned_discussions_then_approve() {
        let runner = ScriptedRunner::new().when("POST", "{}");
        let review = ReviewSubmission {
            event: ReviewEvent::Approve,
            body: "Looks good".into(),
            comments: vec![DraftComment {
                path: "x.rs".into(),
                original_path: None,
                side: Side::New,
                line: 4,
                body: "Nit".into(),
            }],
        };
        submit_review(&runner, &remote(), &mr(), &review, &CancelToken::new()).unwrap();
        let calls = runner.calls();
        let paths: Vec<&str> = calls.iter().map(|c| c.args[7].as_str()).collect();
        assert_eq!(
            paths,
            [
                "projects/GNOME%2Fmeld/merge_requests/5/discussions",
                "projects/GNOME%2Fmeld/merge_requests/5/notes",
                "projects/GNOME%2Fmeld/merge_requests/5/approve",
            ]
        );
        let sent: Value = serde_json::from_str(calls[0].input.as_deref().unwrap()).unwrap();
        assert_eq!(sent["position"]["new_line"], 4);
        assert_eq!(sent["position"]["base_sha"], "b");
        assert_eq!(sent["position"]["position_type"], "text");
        assert_eq!(calls[2].input.as_deref(), Some(r#"{"sha":"h"}"#));
    }

    #[test]
    fn request_changes_is_refused_without_posting() {
        let runner = ScriptedRunner::new().when("POST", "{}");
        let review = ReviewSubmission {
            event: ReviewEvent::RequestChanges,
            body: "No".into(),
            comments: Vec::new(),
        };
        assert!(submit_review(&runner, &remote(), &mr(), &review, &CancelToken::new()).is_err());
        assert!(runner.calls().is_empty());
    }
}
