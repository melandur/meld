//! Runs `claude -p`, reusing the user's Claude Code login (port of
//! `claude.py`)

use std::collections::HashMap;
use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::{Arc, LazyLock};
use std::time::Duration;

use regex::Regex;
use serde_json::Value;

use crate::error::{ClaudeErrorReason, Error, ProcessErrorKind, Result};
use crate::process::{CancelToken, ProcessOptions, format_g, run_process};

pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(3 * 60);

#[derive(Debug, Clone, PartialEq)]
pub struct ClaudeRequest {
    pub prompt: String,
    pub model: String,
    pub system_prompt: Option<String>,
    /// JSON Schema the reply must match; the parsed reply is returned.
    /// Without one, the reply is plain text.
    pub json_schema: Option<Value>,
    /// The Claude Code CLI; `claude` on `PATH` by default
    pub claude_path: String,
    /// Where it runs; the temp folder by default, so no project CLAUDE.md is
    /// loaded
    pub cwd: Option<PathBuf>,
    pub timeout: Duration,
    /// Added to the environment
    pub env: Option<HashMap<String, String>>,
}

impl ClaudeRequest {
    pub fn new(prompt: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            prompt: prompt.into(),
            model: model.into(),
            system_prompt: None,
            json_schema: None,
            claude_path: "claude".into(),
            cwd: None,
            timeout: DEFAULT_TIMEOUT,
            env: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ClaudeResponse {
    /// The reply: parsed JSON when a schema was given, else its text (as a
    /// JSON string)
    pub output: Value,
    /// The reply's text, as Claude Code returned it
    pub text: String,
    /// The model that actually answered, as Claude Code reports it
    pub model: String,
    pub cost_usd: Option<f64>,
}

impl ClaudeResponse {
    pub fn new(output: Value, text: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            output,
            text: text.into(),
            model: model.into(),
            cost_usd: None,
        }
    }
}

/// [`run_claude`], or a stand-in for tests
pub type RunClaude =
    Arc<dyn Fn(&ClaudeRequest, &CancelToken) -> Result<ClaudeResponse> + Send + Sync>;

/// The real Claude Code client as a [`RunClaude`]
pub fn default_runner() -> RunClaude {
    Arc::new(run_claude)
}

static SIGNED_OUT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)not (?:logged|signed) in|\blog ?in\b|/login|authenticat|unauthori[sz]ed|invalid api key|oauth token|\b401\b",
    )
    .expect("valid regex")
});
static RATE_LIMITED: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)rate.?limit|usage limit|too many requests|\b429\b|overloaded|credit balance|quota",
    )
    .expect("valid regex")
});

/// `message`, and what to do about the failures users can fix
fn with_hint(message: String) -> String {
    if SIGNED_OUT.is_match(&message) {
        return format!(
            "{message}\nClaude Code isn't signed in. Run “claude” in a terminal and sign in \
             with /login, then try again."
        );
    }
    if RATE_LIMITED.is_match(&message) {
        return format!("{message}\nClaude Code is rate limited or out of usage. Try again later.");
    }
    message
}

/// The first `count` characters of `text`
pub(crate) fn prefix(text: &str, count: usize) -> &str {
    match text.char_indices().nth(count) {
        Some((index, _)) => &text[..index],
        None => text,
    }
}

/// The command line for `request`; the prompt goes on stdin
///
/// It runs with no tools, MCP servers, hooks or settings, and saves no
/// session: the prompt is the only context.
pub fn claude_args(request: &ClaudeRequest) -> Vec<String> {
    let mut args = vec![
        request.claude_path.clone(),
        "-p".into(),
        "--output-format".into(),
        "json".into(),
        "--model".into(),
        request.model.clone(),
    ];
    if let Some(system_prompt) = &request.system_prompt {
        args.extend(["--system-prompt".into(), system_prompt.clone()]);
    }
    if let Some(schema) = &request.json_schema {
        args.extend(["--json-schema".into(), schema.to_string()]);
    }
    args.extend([
        "--tools".into(),
        String::new(),
        "--strict-mcp-config".into(),
        "--setting-sources".into(),
        String::new(),
        "--no-session-persistence".into(),
    ]);
    args
}

/// Run `claude -p` once and return its answer
///
/// Blocks until Claude Code answers, so call it off the UI thread. Fails
/// with a Claude error (reason `NotFound` when the CLI is missing) or
/// `Error::Cancelled` when `cancel` fires, which kills the process.
pub fn run_claude(request: &ClaudeRequest, cancel: &CancelToken) -> Result<ClaudeResponse> {
    let args = claude_args(request);
    let cwd = request.cwd.clone().unwrap_or_else(std::env::temp_dir);
    let env: Option<HashMap<OsString, OsString>> = request.env.as_ref().map(|env| {
        env.iter()
            .map(|(k, v)| (OsString::from(k), OsString::from(v)))
            .collect()
    });
    let result = run_process(
        &args,
        &ProcessOptions {
            cwd: Some(&cwd),
            input: Some(request.prompt.as_bytes()),
            env: env.as_ref(),
            no_inherit_env: false,
            cancel: Some(cancel),
            timeout: Some(request.timeout),
        },
    );
    let result = match result {
        Ok(result) => result,
        Err(Error::Process {
            kind: ProcessErrorKind::NotFound,
            ..
        }) => {
            return Err(Error::claude_not_found(format!(
                "Claude Code wasn't found at “{}”. Install it, or set the path to the Claude \
                 Code CLI in the AI Lens preferences.",
                request.claude_path
            )));
        }
        Err(Error::Process {
            kind: ProcessErrorKind::Timeout,
            ..
        }) => {
            return Err(Error::claude(
                format!(
                    "Claude Code took too long to answer (more than {} seconds).",
                    format_g(request.timeout.as_secs_f64())
                ),
                ClaudeErrorReason::Timeout,
            ));
        }
        Err(err) => return Err(err),
    };

    let stdout = String::from_utf8_lossy(&result.stdout).into_owned();
    match parse_response(&stdout, &request.model, request.json_schema.is_some()) {
        Ok(response) => Ok(response),
        Err(err) => {
            // A failed run may still report its own error as JSON; that
            // message beats the exit status
            if result.returncode == 0 || err.claude_reason() == Some(ClaudeErrorReason::Failed) {
                return Err(err);
            }
            let stderr = String::from_utf8_lossy(&result.stderr).trim().to_string();
            let detail = if !stderr.is_empty() {
                stderr
            } else if !stdout.trim().is_empty() {
                prefix(stdout.trim(), 500).to_string()
            } else {
                "no output".to_string()
            };
            let status = if result.returncode >= 0 {
                result.returncode.to_string()
            } else {
                format!("signal {}", -result.returncode)
            };
            Err(Error::claude(
                with_hint(format!("Claude Code exited with {status}: {detail}")),
                ClaudeErrorReason::Failed,
            ))
        }
    }
}

/// Parse `claude -p --output-format json` output
///
/// With `json_output`, the reply itself must be JSON too (Claude Code's
/// `structured_output`, or its `result` text).
pub fn parse_response(
    stdout: &str,
    requested_model: &str,
    json_output: bool,
) -> Result<ClaudeResponse> {
    let parsed: Value = serde_json::from_str(stdout).map_err(|_| {
        Error::claude(
            format!(
                "Claude Code returned something other than JSON: {}",
                prefix(stdout, 200)
            ),
            ClaudeErrorReason::InvalidOutput,
        )
    })?;
    let Value::Object(parsed) = parsed else {
        return Err(Error::claude(
            "Claude Code returned an unexpected reply.",
            ClaudeErrorReason::InvalidOutput,
        ));
    };

    let result = parsed.get("result");
    if parsed.get("is_error") == Some(&Value::Bool(true)) {
        let detail = result.and_then(Value::as_str).unwrap_or("unknown");
        return Err(Error::claude(
            with_hint(format!("Claude Code reported an error: {detail}")),
            ClaudeErrorReason::Failed,
        ));
    }

    let mut output = if json_output {
        parsed
            .get("structured_output")
            .filter(|value| !value.is_null())
            .cloned()
    } else {
        None
    };
    let text = result.and_then(Value::as_str).map(str::to_string);
    if output.is_none() {
        let Some(text) = &text else {
            return Err(Error::claude(
                "Claude Code returned no result.",
                ClaudeErrorReason::InvalidOutput,
            ));
        };
        output = Some(if json_output {
            serde_json::from_str(text).map_err(|_| {
                Error::claude(
                    format!(
                        "Claude Code's answer wasn't the requested JSON: {}",
                        prefix(text, 200)
                    ),
                    ClaudeErrorReason::InvalidOutput,
                )
            })?
        } else {
            Value::String(text.clone())
        });
    }
    let output = output.unwrap_or(Value::Null);
    let text = text.unwrap_or_else(|| output.to_string());

    let models: Vec<&String> = match parsed.get("modelUsage") {
        Some(Value::Object(usage)) => usage.keys().collect(),
        _ => Vec::new(),
    };
    let cost = parsed.get("total_cost_usd").and_then(|cost| match cost {
        Value::Number(number) => number.as_f64(),
        _ => None,
    });
    Ok(ClaudeResponse {
        output,
        text,
        // The answering model, when Claude Code reports exactly one
        model: if models.len() == 1 {
            models[0].clone()
        } else {
            requested_model.to_string()
        },
        cost_usd: cost,
    })
}
