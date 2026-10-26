//! Port of test/ailens/test_claude.py; the fake `claude` is a shell script

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use meld_ailens::claude::{ClaudeRequest, claude_args, parse_response, run_claude};
use meld_ailens::{CancelToken, ClaudeErrorReason, Error};
use serde_json::json;

fn base_request(claude_path: &str) -> ClaudeRequest {
    ClaudeRequest {
        prompt: "the prompt".into(),
        model: "claude-sonnet-5-5".into(),
        system_prompt: Some("the system prompt".into()),
        json_schema: Some(json!({"type": "object"})),
        claude_path: claude_path.into(),
        ..ClaudeRequest::new("", "")
    }
}

/// A stand-in `claude` that runs `body` with `sh`
fn fake_claude(dir: &Path, body: &str, name: &str) -> String {
    let path = dir.join(name);
    std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    path.to_string_lossy().into_owned()
}

fn never() -> CancelToken {
    CancelToken::new()
}

fn reason(err: &Error) -> Option<ClaudeErrorReason> {
    err.claude_reason()
}

#[test]
fn passes_the_prompt_on_stdin_with_tools_mcp_settings_and_sessions_off() {
    let temp = tempfile::tempdir().unwrap();
    let out = temp.path().join("out");
    std::fs::create_dir(&out).unwrap();
    let body = format!(
        r#"printf '%s\n' "$@" > '{out}/args'
cat > '{out}/input'
pwd > '{out}/cwd'
printf '%s' '{{"is_error": false, "result": "{{\"ok\": true}}", "modelUsage": {{"claude-sonnet-5-5": {{}}}}, "total_cost_usd": 0.01}}'"#,
        out = out.display()
    );
    let claude = fake_claude(temp.path(), &body, "claude");
    let response = run_claude(&base_request(&claude), &never()).unwrap();

    assert_eq!(response.model, "claude-sonnet-5-5");
    assert_eq!(response.output, json!({"ok": true}));
    assert_eq!(
        std::fs::read_to_string(out.join("input")).unwrap(),
        "the prompt"
    );
    let cwd = PathBuf::from(std::fs::read_to_string(out.join("cwd")).unwrap().trim_end());
    assert_eq!(
        std::fs::canonicalize(cwd).unwrap(),
        std::fs::canonicalize(std::env::temp_dir()).unwrap()
    );
    let args = std::fs::read_to_string(out.join("args")).unwrap();
    let args: Vec<&str> = args.strip_suffix('\n').unwrap().split('\n').collect();
    assert_eq!(
        args,
        [
            "-p",
            "--output-format",
            "json",
            "--model",
            "claude-sonnet-5-5",
            "--system-prompt",
            "the system prompt",
            "--json-schema",
            r#"{"type":"object"}"#,
            "--tools",
            "",
            "--strict-mcp-config",
            "--setting-sources",
            "",
            "--no-session-persistence",
        ]
    );
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&response.text).unwrap(),
        response.output
    );
}

#[test]
fn runs_in_the_requested_folder_with_extra_environment() {
    let temp = tempfile::tempdir().unwrap();
    let claude = fake_claude(
        temp.path(),
        r#"printf '{"result": "[\\"%s\\", \\"%s\\"]"}' "$(pwd)" "$EXTRA""#,
        "claude",
    );
    let work = temp.path().join("work");
    std::fs::create_dir(&work).unwrap();
    let mut request = base_request(&claude);
    request.cwd = Some(work.clone());
    request.env = Some([("EXTRA".to_string(), "yes".to_string())].into());
    let response = run_claude(&request, &never()).unwrap();
    assert_eq!(response.output, json!([work.to_string_lossy(), "yes"]));
}

#[test]
fn unicode_prompt_and_answer() {
    let temp = tempfile::tempdir().unwrap();
    let claude = fake_claude(
        temp.path(),
        r#"input=$(cat); printf '{"structured_output": {"echo": "%s ✓"}}' "$input""#,
        "claude",
    );
    let mut request = base_request(&claude);
    request.prompt = "Grüße ☃ 雪".into();
    let response = run_claude(&request, &never()).unwrap();
    assert_eq!(response.output, json!({"echo": "Grüße ☃ 雪 ✓"}));
}

#[test]
fn without_a_schema_the_answer_is_text() {
    let temp = tempfile::tempdir().unwrap();
    let claude = fake_claude(temp.path(), r#"printf '{"result": "%s"}' "$*""#, "claude");
    let mut request = base_request(&claude);
    request.json_schema = None;
    request.system_prompt = None;
    let response = run_claude(&request, &never()).unwrap();
    assert_eq!(response.output, json!(response.text));
    assert!(!response.text.contains("--json-schema"));
    assert!(!response.text.contains("--system-prompt"));
}

#[test]
fn claude_args_without_schema_or_system_prompt() {
    let mut request = ClaudeRequest::new("p", "m");
    request.claude_path = "/x/claude".into();
    assert_eq!(
        claude_args(&request),
        [
            "/x/claude",
            "-p",
            "--output-format",
            "json",
            "--model",
            "m",
            "--tools",
            "",
            "--strict-mcp-config",
            "--setting-sources",
            "",
            "--no-session-persistence",
        ]
    );
}

#[test]
fn schema_is_compact_json_with_unicode_kept() {
    let mut request = ClaudeRequest::new("p", "m");
    request.json_schema = Some(json!({"a": [1, "é"]}));
    let args = claude_args(&request);
    let index = args.iter().position(|a| a == "--json-schema").unwrap();
    assert_eq!(args[index + 1], r#"{"a":[1,"é"]}"#);
}

#[test]
fn schema_keeps_its_key_order() {
    let mut request = ClaudeRequest::new("p", "m");
    request.json_schema = Some(json!({"type": "object", "properties": {}, "required": []}));
    let args = claude_args(&request);
    let index = args.iter().position(|a| a == "--json-schema").unwrap();
    assert_eq!(
        args[index + 1],
        r#"{"type":"object","properties":{},"required":[]}"#
    );
}

#[test]
fn reports_a_missing_cli_with_a_hint_to_set_the_path() {
    let temp = tempfile::tempdir().unwrap();
    let missing = temp.path().join("missing").to_string_lossy().into_owned();
    let err = run_claude(&base_request(&missing), &never()).unwrap_err();
    assert!(err.is_claude_not_found());
    assert_eq!(reason(&err), Some(ClaudeErrorReason::NotFound));
    assert!(err.to_string().contains(&missing));
    assert!(err.to_string().contains("path"));
}

#[test]
fn reports_a_file_that_is_not_executable_as_missing() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("claude");
    std::fs::write(&path, "not a program").unwrap();
    let err = run_claude(&base_request(&path.to_string_lossy()), &never()).unwrap_err();
    assert!(err.is_claude_not_found());
}

#[test]
fn reports_a_failing_run_with_its_stderr() {
    let temp = tempfile::tempdir().unwrap();
    let claude = fake_claude(
        temp.path(),
        "printf 'something broke' >&2; exit 1",
        "claude",
    );
    let err = run_claude(&base_request(&claude), &never()).unwrap_err();
    assert!(
        err.to_string().contains("exited with 1: something broke"),
        "{err}"
    );
    assert_eq!(reason(&err), Some(ClaudeErrorReason::Failed));
}

#[test]
fn a_failing_run_without_stderr_shows_its_stdout_or_says_there_was_none() {
    let temp = tempfile::tempdir().unwrap();
    let claude = fake_claude(temp.path(), "echo partial; exit 2", "claude");
    let err = run_claude(&base_request(&claude), &never()).unwrap_err();
    assert!(err.to_string().contains("exited with 2: partial"), "{err}");

    let silent = fake_claude(temp.path(), "exit 3", "silent");
    let err = run_claude(&base_request(&silent), &never()).unwrap_err();
    assert!(
        err.to_string().contains("exited with 3: no output"),
        "{err}"
    );
}

#[test]
fn a_run_killed_by_a_signal() {
    let temp = tempfile::tempdir().unwrap();
    let claude = fake_claude(temp.path(), "kill -TERM $$", "claude");
    let err = run_claude(&base_request(&claude), &never()).unwrap_err();
    assert!(err.to_string().contains("exited with signal 15"), "{err}");
}

#[test]
fn says_how_to_sign_in_when_not_logged_in() {
    let temp = tempfile::tempdir().unwrap();
    let claude = fake_claude(
        temp.path(),
        "printf 'Invalid API key · Please run /login' >&2; exit 1",
        "claude",
    );
    let err = run_claude(&base_request(&claude), &never()).unwrap_err();
    let message = err.to_string();
    let run_login = message.find("Please run /login").unwrap();
    assert!(
        message[run_login..].contains("isn't signed in"),
        "{message}"
    );
}

#[test]
fn reports_a_json_error_before_the_exit_status() {
    let temp = tempfile::tempdir().unwrap();
    let claude = fake_claude(
        temp.path(),
        r#"printf '{"is_error": true, "result": "API Error: 429 rate_limit_error"}'; exit 1"#,
        "claude",
    );
    let err = run_claude(&base_request(&claude), &never()).unwrap_err();
    let message = err.to_string();
    assert!(message.starts_with("Claude Code reported an error: API Error: 429 rate_limit_error"));
    assert!(message.contains("rate limited"));
    assert!(!message.contains("exited"));
    assert_eq!(reason(&err), Some(ClaudeErrorReason::Failed));
}

#[test]
fn reports_an_is_error_reply_with_exit_status_zero() {
    let temp = tempfile::tempdir().unwrap();
    let claude = fake_claude(
        temp.path(),
        r#"printf '{"is_error": true, "result": "Credit balance too low"}'"#,
        "claude",
    );
    let err = run_claude(&base_request(&claude), &never()).unwrap_err();
    assert!(err.to_string().contains("Credit balance too low"));
}

#[test]
fn reports_garbage_output() {
    let temp = tempfile::tempdir().unwrap();
    let claude = fake_claude(temp.path(), "echo 'Here is your answer!'", "claude");
    let err = run_claude(&base_request(&claude), &never()).unwrap_err();
    assert!(
        err.to_string()
            .contains("something other than JSON: Here is your answer"),
        "{err}"
    );
    assert_eq!(reason(&err), Some(ClaudeErrorReason::InvalidOutput));
}

#[test]
fn garbage_output_with_a_failing_exit_reports_the_exit() {
    let temp = tempfile::tempdir().unwrap();
    let claude = fake_claude(
        temp.path(),
        "echo '<html>'; printf boom >&2; exit 4",
        "claude",
    );
    let err = run_claude(&base_request(&claude), &never()).unwrap_err();
    assert!(err.to_string().contains("exited with 4: boom"), "{err}");
    assert_eq!(reason(&err), Some(ClaudeErrorReason::Failed));
}

#[test]
fn reports_a_timeout() {
    let temp = tempfile::tempdir().unwrap();
    let claude = fake_claude(temp.path(), "sleep 10", "claude");
    let mut request = base_request(&claude);
    request.timeout = Duration::from_millis(300);
    let started = Instant::now();
    let err = run_claude(&request, &never()).unwrap_err();
    assert!(err.to_string().contains("took too long"), "{err}");
    assert!(err.to_string().contains("0.3 seconds"), "{err}");
    assert_eq!(reason(&err), Some(ClaudeErrorReason::Timeout));
    assert!(started.elapsed() < Duration::from_secs(5));
}

fn process_exists(pid: &str) -> bool {
    std::process::Command::new("kill")
        .args(["-0", pid])
        .stderr(std::process::Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

#[test]
fn cancelling_kills_the_process() {
    let temp = tempfile::tempdir().unwrap();
    let pid_file = temp.path().join("pid");
    let claude = fake_claude(
        temp.path(),
        &format!("echo $$ > '{}'; exec sleep 30", pid_file.display()),
        "claude",
    );
    let cancel = CancelToken::new();
    let thread_cancel = cancel.clone();
    let thread = std::thread::spawn(move || run_claude(&base_request(&claude), &thread_cancel));
    let deadline = Instant::now() + Duration::from_secs(10);
    let pid = loop {
        if let Ok(text) = std::fs::read_to_string(&pid_file)
            && text.ends_with('\n')
        {
            break text.trim().to_string();
        }
        assert!(Instant::now() < deadline, "the fake claude never started");
        std::thread::sleep(Duration::from_millis(20));
    };

    cancel.cancel();
    let result = thread.join().unwrap();
    assert!(result.unwrap_err().is_cancelled());
    assert!(!process_exists(&pid));
}

#[test]
fn a_cancelled_token_never_starts_the_process() {
    let temp = tempfile::tempdir().unwrap();
    let marker = temp.path().join("ran");
    let claude = fake_claude(
        temp.path(),
        &format!("touch '{}'", marker.display()),
        "claude",
    );
    let cancel = CancelToken::new();
    cancel.cancel();
    let err = run_claude(&base_request(&claude), &cancel).unwrap_err();
    assert!(err.is_cancelled());
    assert!(!marker.exists());
}

// -- parse_response --

#[test]
fn prefers_structured_output_and_falls_back_to_the_requested_model() {
    let response = parse_response(
        &json!({"structured_output": {"what": "x"}, "result": "ignored"}).to_string(),
        "m",
        true,
    )
    .unwrap();
    assert_eq!(response.output, json!({"what": "x"}));
    assert_eq!(response.model, "m");
    assert_eq!(response.text, "ignored");
}

#[test]
fn rejects_errors_and_output_that_is_not_the_requested_json() {
    let err = parse_response(
        &json!({"is_error": true, "result": "Credit balance too low"}).to_string(),
        "m",
        true,
    )
    .unwrap_err();
    assert!(err.to_string().contains("Credit balance too low"));
    assert!(parse_response("not json", "m", true).is_err());
    let err = parse_response(&json!({"result": "plain text"}).to_string(), "m", true).unwrap_err();
    assert!(err.to_string().contains("wasn't the requested JSON"));
}

#[test]
fn parse_response_edge_cases() {
    let message = |text: &str| parse_response(text, "m", true).unwrap_err().to_string();
    assert!(message("[1, 2]").contains("unexpected reply"));
    assert!(message("{}").contains("no result"));
    assert!(message("").contains("something other than JSON"));
    assert!(message(&json!({"is_error": true}).to_string()).contains("reported an error: unknown"));
}

#[test]
fn parse_response_reads_the_model_only_when_exactly_one_answered() {
    let one = parse_response(
        &json!({"result": "{}", "modelUsage": {"claude-x": {}}}).to_string(),
        "m",
        true,
    )
    .unwrap();
    assert_eq!(one.model, "claude-x");
    let two = parse_response(
        &json!({"result": "{}", "modelUsage": {"a": {}, "b": {}}}).to_string(),
        "m",
        true,
    )
    .unwrap();
    assert_eq!(two.model, "m");
}

#[test]
fn parse_response_as_text() {
    let response =
        parse_response(&json!({"result": "plain text"}).to_string(), "m", false).unwrap();
    assert_eq!(response.output, json!("plain text"));
}
