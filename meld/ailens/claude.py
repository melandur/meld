# Copyright (C) 2026 Meld contributors
#
# This program is free software: you can redistribute it and/or modify
# it under the terms of the GNU General Public License as published by
# the Free Software Foundation, either version 2 of the License, or (at
# your option) any later version.
#
# This program is distributed in the hope that it will be useful, but
# WITHOUT ANY WARRANTY; without even the implied warranty of
# MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the GNU
# General Public License for more details.
#
# You should have received a copy of the GNU General Public License
# along with this program.  If not, see <http://www.gnu.org/licenses/>.

"""Runs ``claude -p``, reusing the user's Claude Code login"""

import json
import re
import tempfile
from collections.abc import Mapping
from dataclasses import dataclass
from typing import Any, Literal

from meld.ailens.models import AiLensError
from meld.ailens.process import (
    CancelToken,
    ProcessNotFound,
    ProcessTimeout,
    run_process,
)

#: Seconds to wait for an answer
DEFAULT_TIMEOUT = 3 * 60.0

ClaudeErrorReason = Literal["not-found", "failed", "timeout", "invalid-output"]


@dataclass(frozen=True)
class ClaudeRequest:
    prompt: str
    model: str
    system_prompt: str | None = None
    #: JSON Schema the reply must match; the parsed reply is returned. Without
    #: one, the reply is plain text.
    json_schema: Mapping[str, Any] | None = None
    #: The Claude Code CLI; ``claude`` on ``PATH`` by default
    claude_path: str = "claude"
    #: Where it runs; a temp folder by default, so no project CLAUDE.md is
    #: loaded
    cwd: str | None = None
    timeout: float = DEFAULT_TIMEOUT
    #: Added to the environment
    env: Mapping[str, str] | None = None


@dataclass(frozen=True)
class ClaudeResponse:
    #: The reply: parsed JSON when a schema was given, else its text
    output: Any
    #: The reply's text, as Claude Code returned it
    text: str
    #: The model that actually answered, as Claude Code reports it
    model: str
    cost_usd: float | None = None


class ClaudeError(AiLensError):
    def __init__(self, message: str, reason: ClaudeErrorReason) -> None:
        super().__init__(message)
        self.reason = reason


class ClaudeNotFound(ClaudeError):
    """The Claude Code CLI isn't installed, or not at the configured path"""

    def __init__(self, message: str) -> None:
        super().__init__(message, "not-found")


_SIGNED_OUT = re.compile(
    r"not (?:logged|signed) in|\blog ?in\b|/login|authenticat|unauthori[sz]ed"
    r"|invalid api key|oauth token|\b401\b",
    re.IGNORECASE,
)
_RATE_LIMITED = re.compile(
    r"rate.?limit|usage limit|too many requests|\b429\b|overloaded"
    r"|credit balance|quota",
    re.IGNORECASE,
)


def _with_hint(message: str) -> str:
    """``message``, and what to do about the failures users can fix"""
    if _SIGNED_OUT.search(message):
        return (
            f"{message}\nClaude Code isn't signed in. Run “claude” in a "
            "terminal and sign in with /login, then try again."
        )
    if _RATE_LIMITED.search(message):
        return (
            f"{message}\nClaude Code is rate limited or out of usage. Try again later."
        )
    return message


def claude_args(request: ClaudeRequest) -> list[str]:
    """The command line for ``request``; the prompt goes on stdin

    It runs with no tools, MCP servers, hooks or settings, and saves no
    session: the prompt is the only context.
    """
    args = [
        request.claude_path,
        "-p",
        "--output-format",
        "json",
        "--model",
        request.model,
    ]
    if request.system_prompt is not None:
        args += ["--system-prompt", request.system_prompt]
    if request.json_schema is not None:
        schema = json.dumps(
            request.json_schema, separators=(",", ":"), ensure_ascii=False
        )
        args += ["--json-schema", schema]
    args += [
        "--tools",
        "",
        "--strict-mcp-config",
        "--setting-sources",
        "",
        "--no-session-persistence",
    ]
    return args


def run_claude(
    request: ClaudeRequest, cancel: CancelToken | None = None
) -> ClaudeResponse:
    """Run ``claude -p`` once and return its answer

    Blocks until Claude Code answers, so call it off the main thread.
    Raises :class:`ClaudeError` (:class:`ClaudeNotFound` when the CLI is
    missing) or :class:`~meld.ailens.models.Cancelled` when ``cancel``
    fires, which kills the process.
    """
    args = claude_args(request)
    try:
        result = run_process(
            args,
            cwd=request.cwd or tempfile.gettempdir(),
            input=request.prompt.encode("utf-8"),
            env=request.env,
            cancel=cancel,
            timeout=request.timeout,
        )
    except ProcessNotFound as err:
        raise ClaudeNotFound(
            f"Claude Code wasn't found at “{request.claude_path}”. Install "
            "it, or set the path to the Claude Code CLI in the AI Lens "
            "preferences."
        ) from err
    except ProcessTimeout as err:
        raise ClaudeError(
            f"Claude Code took too long to answer (more than "
            f"{request.timeout:g} seconds).",
            "timeout",
        ) from err

    stdout = result.stdout.decode("utf-8", errors="replace")
    try:
        return parse_response(
            stdout, request.model, json_output=request.json_schema is not None
        )
    except ClaudeError as err:
        # A failed run may still report its own error as JSON; that message
        # beats the exit status
        if result.returncode == 0 or err.reason == "failed":
            raise
        stderr = result.stderr.decode("utf-8", errors="replace").strip()
        detail = stderr or stdout.strip()[:500] or "no output"
        status = (
            result.returncode
            if result.returncode >= 0
            else f"signal {-result.returncode}"
        )
        raise ClaudeError(
            _with_hint(f"Claude Code exited with {status}: {detail}"), "failed"
        ) from None


def parse_response(
    stdout: str, requested_model: str, json_output: bool = True
) -> ClaudeResponse:
    """Parse ``claude -p --output-format json`` output

    With ``json_output``, the reply itself must be JSON too (Claude Code's
    ``structured_output``, or its ``result`` text).
    """
    try:
        parsed = json.loads(stdout)
    except ValueError:
        raise ClaudeError(
            "Claude Code returned something other than JSON: " + stdout[:200],
            "invalid-output",
        ) from None
    if not isinstance(parsed, dict):
        raise ClaudeError("Claude Code returned an unexpected reply.", "invalid-output")

    result = parsed.get("result")
    if parsed.get("is_error") is True:
        detail = result if isinstance(result, str) else "unknown"
        raise ClaudeError(
            _with_hint(f"Claude Code reported an error: {detail}"), "failed"
        )

    output = parsed.get("structured_output") if json_output else None
    text = result if isinstance(result, str) else None
    if output is None:
        if text is None:
            raise ClaudeError("Claude Code returned no result.", "invalid-output")
        if json_output:
            try:
                output = json.loads(text)
            except ValueError:
                raise ClaudeError(
                    "Claude Code's answer wasn't the requested JSON: " + text[:200],
                    "invalid-output",
                ) from None
        else:
            output = text
    if text is None:
        text = json.dumps(output, ensure_ascii=False)

    usage = parsed.get("modelUsage")
    models = list(usage) if isinstance(usage, dict) else []
    cost = parsed.get("total_cost_usd")
    return ClaudeResponse(
        output=output,
        text=text,
        # The answering model, when Claude Code reports exactly one
        model=models[0] if len(models) == 1 else requested_model,
        cost_usd=float(cost)
        if isinstance(cost, (int, float)) and not isinstance(cost, bool)
        else None,
    )
