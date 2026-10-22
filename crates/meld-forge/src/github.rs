//! GitHub pull requests through `gh`
//!
//! Lists and details come from `gh pr list/view --json`, line comments from
//! `gh api repos/{owner}/{repo}/pulls/{n}/comments`; reviews are posted to
//! `POST /repos/{owner}/{repo}/pulls/{n}/reviews` with `comments[]` using
//! `line` and `side`, replies to `…/comments/{id}/replies`.

use serde_json::{Value, json};

use meld_ailens::CancelToken;

use crate::error::{Error, Result};
use crate::json::{array, flag, number, opt_text, parse_object, parse_pages, text};
use crate::models::*;
use crate::runner::Runner;

const LIST_FIELDS: &str = "number,title,author,state,isDraft,createdAt,updatedAt,url,headRefName,baseRefName,headRefOid,labels";
const DETAIL_FIELDS: &str = "number,title,body,author,state,isDraft,createdAt,updatedAt,url,headRefName,\
     baseRefName,headRefOid,baseRefOid,labels,mergeCommit,statusCheckRollup,reviewDecision,additions,\
     deletions,changedFiles";

const KIND: ForgeKind = ForgeKind::GitHub;

fn repo_arg(remote: &ForgeRemote) -> String {
    format!("{}/{}", remote.host, remote.path)
}

fn strings(args: &[&str]) -> Vec<String> {
    args.iter().map(|a| a.to_string()).collect()
}

fn pull_request(v: &Value) -> PullRequest {
    let state = match text(v, "state").to_uppercase().as_str() {
        "MERGED" => PrState::Merged,
        "CLOSED" => PrState::Closed,
        _ => PrState::Open,
    };
    PullRequest {
        number: number(v, "number").unwrap_or_default(),
        title: text(v, "title"),
        author: text(v, "author.login"),
        state,
        draft: flag(v, "isDraft"),
        created_at: text(v, "createdAt"),
        updated_at: text(v, "updatedAt"),
        url: text(v, "url"),
        head_branch: text(v, "headRefName"),
        base_branch: text(v, "baseRefName"),
        head_sha: text(v, "headRefOid"),
        labels: array(v, "labels")
            .iter()
            .map(|l| text(l, "name"))
            .filter(|n| !n.is_empty())
            .collect(),
    }
}

/// A number typed into the search, e.g. `#12` or `12`
pub fn typed_number(search: &str) -> Option<u64> {
    let search = search.trim();
    let digits = search.strip_prefix(['#', '!']).unwrap_or(search);
    (!digits.is_empty() && digits.len() < 10 && digits.chars().all(|c| c.is_ascii_digit()))
        .then(|| digits.parse().ok())
        .flatten()
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
        StateFilter::Open => "open",
        StateFilter::Closed => "closed",
        StateFilter::Merged => "merged",
        StateFilter::All => "all",
    };
    let query = format!("{} sort:updated-desc", search.trim())
        .trim()
        .to_string();
    let mut args = strings(&["pr", "list", "--repo"]);
    args.push(repo_arg(remote));
    args.extend(strings(&["--state", state, "--limit"]));
    args.push(limit.max(1).to_string());
    args.extend(strings(&["--json", LIST_FIELDS, "--search"]));
    args.push(query);
    let out = runner.run(KIND, &args, None, cancel)?;
    let mut prs: Vec<PullRequest> = parse_pages("gh", &out)?.iter().map(pull_request).collect();
    if filter == StateFilter::Closed {
        // `closed` includes merged ones on GitHub; the filter means not merged
        prs.retain(|pr| pr.state == PrState::Closed);
    }
    Ok(prs)
}

/// e.g. "3 passed, 1 failed, 2 pending"
pub fn checks_summary(rollup: &[Value]) -> Option<String> {
    if rollup.is_empty() {
        return None;
    }
    let (mut passed, mut failed, mut pending, mut skipped) = (0, 0, 0, 0);
    for check in rollup {
        let conclusion = text(check, "conclusion").to_uppercase();
        let status = text(check, "status").to_uppercase();
        let state = text(check, "state").to_uppercase();
        let outcome = if !state.is_empty() {
            state
        } else if status == "COMPLETED" || !conclusion.is_empty() {
            conclusion
        } else {
            "PENDING".into()
        };
        match outcome.as_str() {
            "SUCCESS" => passed += 1,
            "FAILURE" | "ERROR" | "TIMED_OUT" | "CANCELLED" | "ACTION_REQUIRED"
            | "STARTUP_FAILURE" => failed += 1,
            "SKIPPED" | "NEUTRAL" | "STALE" => skipped += 1,
            _ => pending += 1,
        }
    }
    let parts: Vec<String> = [
        (passed, "passed"),
        (failed, "failed"),
        (pending, "pending"),
        (skipped, "skipped"),
    ]
    .iter()
    .filter(|(n, _)| *n > 0)
    .map(|(n, label)| format!("{n} {label}"))
    .collect();
    Some(format!("Checks: {}", parts.join(", ")))
}

fn review_decision(decision: &str) -> Option<String> {
    Some(
        match decision {
            "APPROVED" => "Approved",
            "CHANGES_REQUESTED" => "Changes requested",
            "REVIEW_REQUIRED" => "Review required",
            _ => return None,
        }
        .to_string(),
    )
}

pub fn details(
    runner: &dyn Runner,
    remote: &ForgeRemote,
    number: u64,
    cancel: &CancelToken,
) -> Result<PullRequestDetails> {
    let mut args = strings(&["pr", "view"]);
    args.push(number.to_string());
    args.push("--repo".into());
    args.push(repo_arg(remote));
    args.extend(strings(&["--json", DETAIL_FIELDS]));
    let out = runner.run(KIND, &args, None, cancel)?;
    let v = parse_object("gh", &out)?;
    Ok(PullRequestDetails {
        pr: pull_request(&v),
        body: text(&v, "body"),
        checks: checks_summary(array(&v, "statusCheckRollup")),
        review_decision: review_decision(&text(&v, "reviewDecision")),
        base_sha: opt_text(&v, "baseRefOid"),
        merge_commit: opt_text(&v, "mergeCommit.oid"),
        diff_refs: None,
        additions: crate::json::number(&v, "additions"),
        deletions: crate::json::number(&v, "deletions"),
        changed_files: crate::json::number(&v, "changedFiles"),
    })
}

fn review_state(state: &str) -> Option<ReviewState> {
    Some(match state {
        "APPROVED" => ReviewState::Approved,
        "CHANGES_REQUESTED" => ReviewState::ChangesRequested,
        "COMMENTED" => ReviewState::Commented,
        "DISMISSED" => ReviewState::Dismissed,
        // PENDING: the viewer's own unsubmitted review
        _ => return None,
    })
}

/// Threads of review comments: replies grouped under the comment they
/// answer, in the order the threads started
pub fn threads_from_comments(comments: &[Value]) -> Vec<ReviewThread> {
    let mut threads: Vec<ReviewThread> = Vec::new();
    for c in comments {
        let id = text(c, "id");
        let comment = Comment {
            id: id.clone(),
            author: text(c, "user.login"),
            body: text(c, "body"),
            created_at: text(c, "created_at"),
            url: text(c, "html_url"),
        };
        let parent = text(c, "in_reply_to_id");
        if !parent.is_empty()
            && let Some(thread) = threads
                .iter_mut()
                .find(|t| t.id == parent || t.comments.iter().any(|x| x.id == parent))
        {
            thread.comments.push(comment);
            continue;
        }
        let side = if text(c, "side").eq_ignore_ascii_case("LEFT") {
            Side::Old
        } else {
            Side::New
        };
        let line = number(c, "line").map(|n| n as u32);
        let original_line = number(c, "original_line").map(|n| n as u32);
        threads.push(ReviewThread {
            id,
            path: text(c, "path"),
            original_path: None,
            side,
            line,
            original_line,
            outdated: line.is_none() && original_line.is_some(),
            resolved: false,
            comments: vec![comment],
        });
    }
    threads
}

pub fn discussion(
    runner: &dyn Runner,
    remote: &ForgeRemote,
    number: u64,
    cancel: &CancelToken,
) -> Result<Discussion> {
    let mut args = strings(&["pr", "view"]);
    args.push(number.to_string());
    args.push("--repo".into());
    args.push(repo_arg(remote));
    args.extend(strings(&["--json", "comments,reviews"]));
    let out = runner.run(KIND, &args, None, cancel)?;
    let v = parse_object("gh", &out)?;
    let mut conversation: Vec<ConversationItem> = array(&v, "comments")
        .iter()
        .map(|c| ConversationItem {
            review: None,
            comment: Comment {
                id: text(c, "id"),
                author: text(c, "author.login"),
                body: text(c, "body"),
                created_at: text(c, "createdAt"),
                url: text(c, "url"),
            },
        })
        .collect();
    for r in array(&v, "reviews") {
        let Some(state) = review_state(&text(r, "state")) else {
            continue;
        };
        let body = text(r, "body");
        // A plain "reviewed" without words is only its line comments
        if state == ReviewState::Commented && body.trim().is_empty() {
            continue;
        }
        conversation.push(ConversationItem {
            review: Some(state),
            comment: Comment {
                id: text(r, "id"),
                author: text(r, "author.login"),
                body,
                created_at: text(r, "submittedAt"),
                url: String::new(),
            },
        });
    }
    // ISO 8601 in UTC sorts as text
    conversation.sort_by(|a, b| a.comment.created_at.cmp(&b.comment.created_at));

    let path = format!("repos/{}/pulls/{number}/comments?per_page=100", remote.path);
    let args = vec![
        "api".to_string(),
        "--hostname".into(),
        remote.host.clone(),
        "--paginate".into(),
        path,
    ];
    let out = runner.run(KIND, &args, None, cancel)?;
    let threads = threads_from_comments(&parse_pages("gh", &out)?);
    Ok(Discussion {
        conversation,
        threads,
    })
}

fn post(
    runner: &dyn Runner,
    remote: &ForgeRemote,
    path: String,
    body: &Value,
    cancel: &CancelToken,
) -> Result<Value> {
    let args = vec![
        "api".to_string(),
        "--hostname".into(),
        remote.host.clone(),
        "-X".into(),
        "POST".into(),
        "-H".into(),
        "Accept: application/vnd.github+json".into(),
        "-H".into(),
        "Content-Type: application/json".into(),
        path,
        "--input".into(),
        "-".into(),
    ];
    let input = serde_json::to_vec(body).unwrap_or_default();
    let out = runner.run(KIND, &args, Some(&input), cancel)?;
    Ok(serde_json::from_slice(&out).unwrap_or(Value::Null))
}

/// The JSON body of `POST /repos/{o}/{r}/pulls/{n}/reviews`
pub fn review_body(details: &PullRequestDetails, review: &ReviewSubmission) -> Value {
    let comments: Vec<Value> = review
        .comments
        .iter()
        .map(|c| {
            json!({
                "path": c.path,
                "line": c.line,
                "side": match c.side { Side::Old => "LEFT", Side::New => "RIGHT" },
                "body": c.body,
            })
        })
        .collect();
    let mut body = json!({
        "event": review.event.github_event(),
        "comments": comments,
    });
    if !review.body.trim().is_empty() {
        body["body"] = Value::from(review.body.clone());
    }
    if !details.pr.head_sha.is_empty() {
        body["commit_id"] = Value::from(details.pr.head_sha.clone());
    }
    body
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
    let path = format!("repos/{}/pulls/{}/reviews", remote.path, details.pr.number);
    post(runner, remote, path, &review_body(details, review), cancel)?;
    Ok(())
}

pub fn reply(
    runner: &dyn Runner,
    remote: &ForgeRemote,
    details: &PullRequestDetails,
    thread: &ReviewThread,
    body: &str,
    cancel: &CancelToken,
) -> Result<()> {
    if body.trim().is_empty() {
        return Err(Error::Invalid("The reply is empty.".into()));
    }
    let path = format!(
        "repos/{}/pulls/{}/comments/{}/replies",
        remote.path, details.pr.number, thread.id
    );
    post(runner, remote, path, &json!({ "body": body }), cancel)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::ScriptedRunner;

    fn remote() -> ForgeRemote {
        crate::remote::parse_remote("github", "git@github.com:melandur/meld.git").unwrap()
    }

    const LIST: &str = r#"[{"number":12,"title":"Add pull requests","author":{"login":"alice"},
        "state":"OPEN","isDraft":true,"createdAt":"2026-10-01T10:00:00Z","updatedAt":"2026-10-20T10:00:00Z",
        "url":"https://github.com/melandur/meld/pull/12","headRefName":"prs","baseRefName":"main",
        "headRefOid":"abc","labels":[{"name":"feature"}]},
        {"number":9,"title":"Old","author":{"login":"bob"},"state":"MERGED","isDraft":false,"labels":[]}]"#;

    #[test]
    fn lists_pull_requests() {
        let runner = ScriptedRunner::new().when("pr list", LIST);
        let prs = list(
            &runner,
            &remote(),
            StateFilter::All,
            "",
            30,
            &CancelToken::new(),
        )
        .unwrap();
        assert_eq!(prs.len(), 2);
        assert_eq!(prs[0].number, 12);
        assert_eq!(prs[0].author, "alice");
        assert_eq!(prs[0].state_label(), "Draft");
        assert_eq!(prs[0].labels, ["feature"]);
        assert_eq!(prs[1].state, PrState::Merged);
        let call = &runner.calls()[0];
        assert_eq!(
            call.line(),
            format!(
                "pr list --repo github.com/melandur/meld --state all --limit 30 --json {LIST_FIELDS} --search sort:updated-desc"
            )
        );
    }

    #[test]
    fn closed_means_closed_without_merging() {
        let runner = ScriptedRunner::new().when("pr list", LIST.replace("OPEN", "CLOSED"));
        let prs = list(
            &runner,
            &remote(),
            StateFilter::Closed,
            "add",
            5,
            &CancelToken::new(),
        )
        .unwrap();
        assert_eq!(prs.iter().map(|p| p.number).collect::<Vec<_>>(), [12]);
        assert!(
            runner.calls()[0]
                .line()
                .ends_with("--search add sort:updated-desc")
        );
    }

    #[test]
    fn a_typed_number_is_looked_up() {
        let runner =
            ScriptedRunner::new().when("pr view 12", r#"{"number":12,"title":"T","state":"OPEN"}"#);
        let prs = list(
            &runner,
            &remote(),
            StateFilter::Open,
            "#12",
            5,
            &CancelToken::new(),
        )
        .unwrap();
        assert_eq!(prs[0].title, "T");
        assert_eq!(typed_number("!7"), Some(7));
        assert_eq!(typed_number("7a"), None);
    }

    #[test]
    fn details_summarize_checks() {
        let runner = ScriptedRunner::new().when(
            "pr view",
            r#"{"number":3,"title":"T","body":"Desc","state":"MERGED","baseRefOid":"b","headRefOid":"h",
               "mergeCommit":{"oid":"m"},"reviewDecision":"APPROVED","additions":5,
               "statusCheckRollup":[
                 {"__typename":"CheckRun","status":"COMPLETED","conclusion":"SUCCESS"},
                 {"__typename":"CheckRun","status":"COMPLETED","conclusion":"FAILURE"},
                 {"__typename":"CheckRun","status":"IN_PROGRESS","conclusion":""},
                 {"__typename":"StatusContext","state":"SUCCESS"}]}"#,
        );
        let d = details(&runner, &remote(), 3, &CancelToken::new()).unwrap();
        assert_eq!(d.body, "Desc");
        assert_eq!(d.merge_commit.as_deref(), Some("m"));
        assert_eq!(
            d.checks.as_deref(),
            Some("Checks: 2 passed, 1 failed, 1 pending")
        );
        assert_eq!(d.review_decision.as_deref(), Some("Approved"));
        assert_eq!(d.additions, Some(5));
    }

    #[test]
    fn discussion_groups_threads_and_sorts_the_conversation() {
        let runner = ScriptedRunner::new()
            .when(
                "pr view comments,reviews",
                r#"{"comments":[{"id":"IC_1","author":{"login":"carol"},"body":"Thanks","createdAt":"2026-10-03T00:00:00Z"}],
                    "reviews":[{"id":"R_1","author":{"login":"dave"},"body":"","state":"APPROVED","submittedAt":"2026-10-02T00:00:00Z"},
                               {"id":"R_2","author":{"login":"erin"},"body":"","state":"COMMENTED","submittedAt":"2026-10-04T00:00:00Z"}]}"#,
            )
            .when(
                "api pulls/12/comments",
                r#"[{"id":100,"path":"a.rs","line":4,"original_line":4,"side":"RIGHT","body":"Why?","user":{"login":"dave"}},
                    {"id":101,"path":"b.rs","line":null,"original_line":7,"side":"LEFT","body":"Gone","user":{"login":"dave"}}]
                   [{"id":102,"in_reply_to_id":100,"path":"a.rs","body":"Because","user":{"login":"alice"}}]"#,
            );
        let d = discussion(&runner, &remote(), 12, &CancelToken::new()).unwrap();
        let authors: Vec<&str> = d
            .conversation
            .iter()
            .map(|c| c.comment.author.as_str())
            .collect();
        assert_eq!(authors, ["dave", "carol"]);
        assert_eq!(d.conversation[0].review, Some(ReviewState::Approved));
        assert_eq!(d.threads.len(), 2);
        assert_eq!(d.threads[0].comments.len(), 2);
        assert_eq!(d.threads[0].side, Side::New);
        assert_eq!(d.threads[0].anchor_line(), Some(4));
        assert!(d.threads[1].outdated);
        assert_eq!(d.threads[1].side, Side::Old);
        assert_eq!(d.threads[1].anchor_line(), Some(7));
        assert!(
            runner.calls()[1]
                .line()
                .contains("--hostname github.com --paginate repos/melandur/meld/pulls/12/comments")
        );
    }

    fn pr_details() -> PullRequestDetails {
        PullRequestDetails {
            pr: PullRequest {
                number: 12,
                head_sha: "abc".into(),
                ..Default::default()
            },
            ..Default::default()
        }
    }

    #[test]
    fn reviews_post_line_comments_with_sides() {
        let runner = ScriptedRunner::new().when("api POST reviews", "{}");
        let review = ReviewSubmission {
            event: ReviewEvent::RequestChanges,
            body: "Please fix".into(),
            comments: vec![DraftComment {
                path: "a.rs".into(),
                original_path: None,
                side: Side::Old,
                line: 3,
                body: "Not this".into(),
            }],
        };
        submit_review(
            &runner,
            &remote(),
            &pr_details(),
            &review,
            &CancelToken::new(),
        )
        .unwrap();
        let call = &runner.calls()[0];
        assert!(call.line().contains("-X POST"));
        assert!(
            call.line()
                .contains("repos/melandur/meld/pulls/12/reviews --input -")
        );
        let sent: Value = serde_json::from_str(call.input.as_deref().unwrap()).unwrap();
        assert_eq!(
            sent,
            json!({"event": "REQUEST_CHANGES", "body": "Please fix", "commit_id": "abc",
                   "comments": [{"path": "a.rs", "line": 3, "side": "LEFT", "body": "Not this"}]})
        );
    }

    #[test]
    fn invalid_reviews_post_nothing() {
        let runner = ScriptedRunner::new().when("api", "{}");
        let review = ReviewSubmission {
            event: ReviewEvent::Comment,
            body: String::new(),
            comments: Vec::new(),
        };
        assert!(matches!(
            submit_review(
                &runner,
                &remote(),
                &pr_details(),
                &review,
                &CancelToken::new()
            ),
            Err(Error::Invalid(_))
        ));
        assert!(runner.calls().is_empty());
    }

    #[test]
    fn replies_go_to_the_threads_first_comment() {
        let runner = ScriptedRunner::new().when("replies", "{}");
        let thread = ReviewThread {
            id: "100".into(),
            path: "a.rs".into(),
            original_path: None,
            side: Side::New,
            line: Some(1),
            original_line: Some(1),
            outdated: false,
            resolved: false,
            comments: Vec::new(),
        };
        reply(
            &runner,
            &remote(),
            &pr_details(),
            &thread,
            "Done",
            &CancelToken::new(),
        )
        .unwrap();
        let call = &runner.calls()[0];
        assert!(
            call.line()
                .contains("repos/melandur/meld/pulls/12/comments/100/replies")
        );
        assert_eq!(call.input.as_deref(), Some(r#"{"body":"Done"}"#));
    }
}
