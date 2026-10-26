//! GitHub pull requests through `gh`
//!
//! Lists and details come from `gh pr list/view --json`; reviews are
//! posted to `POST /repos/{owner}/{repo}/pulls/{n}/reviews` with
//! `comments[]` using `line` and `side`.

use serde_json::{Value, json};

use meld_ailens::CancelToken;

use crate::error::{Error, Result};
use crate::json::{array, flag, number, opt_text, parse_object, parse_pages, text};
use crate::models::*;
use crate::runner::Runner;

const LIST_FIELDS: &str =
    "number,title,author,state,isDraft,updatedAt,url,headRefName,baseRefName,headRefOid,labels";
const DETAIL_FIELDS: &str = "number,title,author,state,isDraft,updatedAt,url,headRefName,\
     baseRefName,headRefOid,baseRefOid,labels,mergeCommit";

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
        base_sha: opt_text(&v, "baseRefOid"),
        merge_commit: opt_text(&v, "mergeCommit.oid"),
        diff_refs: None,
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
    fn details_have_the_base_and_merge_commits() {
        let runner = ScriptedRunner::new().when(
            "pr view",
            r#"{"number":3,"title":"T","state":"MERGED","baseRefOid":"b","headRefOid":"h",
               "mergeCommit":{"oid":"m"}}"#,
        );
        let d = details(&runner, &remote(), 3, &CancelToken::new()).unwrap();
        assert_eq!(d.base_sha.as_deref(), Some("b"));
        assert_eq!(d.merge_commit.as_deref(), Some("m"));
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
}
