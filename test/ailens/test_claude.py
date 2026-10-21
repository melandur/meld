import json
import os
import sys
import tempfile
import threading
import time
from pathlib import Path

import pytest

from meld.ailens.claude import (
    ClaudeError,
    ClaudeNotFound,
    ClaudeRequest,
    claude_args,
    parse_response,
    run_claude,
)
from meld.ailens.models import AiLensError, Cancelled
from meld.ailens.process import CancelToken

REQUEST = ClaudeRequest(
    prompt="the prompt",
    model="claude-sonnet-5-5",
    system_prompt="the system prompt",
    json_schema={"type": "object"},
)


def fake_claude(tmp_path: Path, body: str, name: str = "claude") -> str:
    """A stand-in ``claude`` that runs ``body`` with ``args`` (argv after the
    script), ``input`` (stdin) and ``cwd``
    """
    path = tmp_path / name
    path.write_text(
        f"#!{sys.executable}\n"
        "import json, os, sys, time\n"
        "args = sys.argv[1:]\n"
        "input = sys.stdin.read()\n"
        "cwd = os.getcwd()\n" + body + "\n"
    )
    path.chmod(0o755)
    return str(path)


def request(claude_path: str, **changes) -> ClaudeRequest:
    fields = {**REQUEST.__dict__, "claude_path": claude_path, **changes}
    return ClaudeRequest(**fields)


ECHO = """
print(json.dumps({
    "is_error": False,
    "result": json.dumps({"args": args, "input": input, "cwd": cwd}),
    "modelUsage": {"claude-sonnet-5-5": {}},
    "total_cost_usd": 0.01,
}))
"""


def test_passes_the_prompt_on_stdin_with_tools_mcp_settings_and_sessions_off(tmp_path):
    response = run_claude(request(fake_claude(tmp_path, ECHO)))

    assert response.model == "claude-sonnet-5-5"
    assert response.cost_usd == 0.01
    assert response.output["input"] == "the prompt"
    assert os.path.realpath(response.output["cwd"]) == os.path.realpath(
        tempfile.gettempdir()
    )
    assert response.output["args"] == [
        "-p",
        "--output-format",
        "json",
        "--model",
        "claude-sonnet-5-5",
        "--system-prompt",
        "the system prompt",
        "--json-schema",
        '{"type":"object"}',
        "--tools",
        "",
        "--strict-mcp-config",
        "--setting-sources",
        "",
        "--no-session-persistence",
    ]
    assert json.loads(response.text) == response.output


def test_runs_in_the_requested_folder_with_extra_environment(tmp_path):
    claude = fake_claude(
        tmp_path,
        'print(json.dumps({"result": json.dumps([cwd, os.environ["EXTRA"]])}))',
    )
    work = tmp_path / "work"
    work.mkdir()
    response = run_claude(request(claude, cwd=str(work), env={"EXTRA": "yes"}))
    assert response.output == [str(work), "yes"]


def test_unicode_prompt_and_answer(tmp_path):
    claude = fake_claude(
        tmp_path,
        'print(json.dumps({"structured_output": {"echo": input + " ✓"}}, ensure_ascii=False))',
    )
    response = run_claude(request(claude, prompt="Grüße ☃ 雪"))
    assert response.output == {"echo": "Grüße ☃ 雪 ✓"}


def test_without_a_schema_the_answer_is_text(tmp_path):
    claude = fake_claude(tmp_path, 'print(json.dumps({"result": " ".join(args)}))')
    response = run_claude(request(claude, json_schema=None, system_prompt=None))
    assert response.output == response.text
    assert "--json-schema" not in response.text
    assert "--system-prompt" not in response.text


def test_claude_args_without_schema_or_system_prompt():
    args = claude_args(ClaudeRequest(prompt="p", model="m", claude_path="/x/claude"))
    assert args == [
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


def test_schema_is_compact_json_with_unicode_kept():
    args = claude_args(
        ClaudeRequest(prompt="p", model="m", json_schema={"a": [1, "é"]})
    )
    assert args[args.index("--json-schema") + 1] == '{"a":[1,"é"]}'


def test_reports_a_missing_cli_with_a_hint_to_set_the_path(tmp_path):
    missing = str(tmp_path / "missing")
    with pytest.raises(ClaudeNotFound) as raised:
        run_claude(request(missing))
    assert raised.value.reason == "not-found"
    assert missing in str(raised.value)
    assert "path" in str(raised.value)
    assert isinstance(raised.value, ClaudeError)
    assert isinstance(raised.value, AiLensError)


def test_reports_a_file_that_is_not_executable_as_missing(tmp_path):
    path = tmp_path / "claude"
    path.write_text("not a program")
    with pytest.raises(ClaudeNotFound):
        run_claude(request(str(path)))


def test_reports_a_failing_run_with_its_stderr(tmp_path):
    claude = fake_claude(tmp_path, 'sys.stderr.write("something broke"); sys.exit(1)')
    with pytest.raises(ClaudeError, match=r"exited with 1: something broke") as raised:
        run_claude(request(claude))
    assert raised.value.reason == "failed"


def test_a_failing_run_without_stderr_shows_its_stdout_or_says_there_was_none(tmp_path):
    claude = fake_claude(tmp_path, 'print("partial"); sys.exit(2)')
    with pytest.raises(ClaudeError, match=r"exited with 2: partial"):
        run_claude(request(claude))

    silent = fake_claude(tmp_path, "sys.exit(3)", name="silent")
    with pytest.raises(ClaudeError, match=r"exited with 3: no output"):
        run_claude(request(silent))


def test_a_run_killed_by_a_signal(tmp_path):
    claude = fake_claude(
        tmp_path, "import signal; os.kill(os.getpid(), signal.SIGTERM)"
    )
    with pytest.raises(ClaudeError, match=r"exited with signal 15"):
        run_claude(request(claude))


def test_says_how_to_sign_in_when_not_logged_in(tmp_path):
    claude = fake_claude(
        tmp_path, 'sys.stderr.write("Invalid API key · Please run /login"); sys.exit(1)'
    )
    with pytest.raises(ClaudeError, match=r"(?s)Please run /login.*isn't signed in"):
        run_claude(request(claude))


def test_reports_a_json_error_before_the_exit_status(tmp_path):
    claude = fake_claude(
        tmp_path,
        'print(json.dumps({"is_error": True, "result": "API Error: 429 rate_limit_error"}))\n'
        "sys.exit(1)",
    )
    with pytest.raises(ClaudeError) as raised:
        run_claude(request(claude))
    message = str(raised.value)
    assert message.startswith(
        "Claude Code reported an error: API Error: 429 rate_limit_error"
    )
    assert "rate limited" in message
    assert "exited" not in message
    assert raised.value.reason == "failed"


def test_reports_an_is_error_reply_with_exit_status_zero(tmp_path):
    claude = fake_claude(
        tmp_path,
        'print(json.dumps({"is_error": True, "result": "Credit balance too low"}))',
    )
    with pytest.raises(ClaudeError, match="Credit balance too low"):
        run_claude(request(claude))


def test_reports_garbage_output(tmp_path):
    claude = fake_claude(tmp_path, 'print("Here is your answer!")')
    with pytest.raises(
        ClaudeError, match="something other than JSON: Here is your answer"
    ) as raised:
        run_claude(request(claude))
    assert raised.value.reason == "invalid-output"


def test_garbage_output_with_a_failing_exit_reports_the_exit(tmp_path):
    claude = fake_claude(
        tmp_path, 'print("<html>"); sys.stderr.write("boom"); sys.exit(4)'
    )
    with pytest.raises(ClaudeError, match=r"exited with 4: boom") as raised:
        run_claude(request(claude))
    assert raised.value.reason == "failed"


def test_reports_a_timeout(tmp_path):
    claude = fake_claude(tmp_path, "time.sleep(10)")
    started = time.monotonic()
    with pytest.raises(ClaudeError, match="took too long") as raised:
        run_claude(request(claude, timeout=0.3))
    assert raised.value.reason == "timeout"
    assert time.monotonic() - started < 5


def test_cancelling_kills_the_process(tmp_path):
    pid_file = tmp_path / "pid"
    claude = fake_claude(
        tmp_path,
        f"open({str(pid_file)!r}, 'w').write(str(os.getpid()))\ntime.sleep(30)",
    )
    cancel = CancelToken()
    raised: list[BaseException] = []

    def run() -> None:
        try:
            run_claude(request(claude), cancel)
        except BaseException as err:
            raised.append(err)

    thread = threading.Thread(target=run)
    thread.start()
    deadline = time.monotonic() + 10
    while not (pid_file.exists() and pid_file.read_text()):
        assert time.monotonic() < deadline, "the fake claude never started"
        time.sleep(0.02)
    pid = int(pid_file.read_text())

    cancel.cancel()
    thread.join(5)
    assert not thread.is_alive()
    assert len(raised) == 1 and isinstance(raised[0], Cancelled)
    with pytest.raises(ProcessLookupError):
        os.kill(pid, 0)


def test_a_cancelled_token_never_starts_the_process(tmp_path):
    marker = tmp_path / "ran"
    claude = fake_claude(tmp_path, f"open({str(marker)!r}, 'w')")
    cancel = CancelToken()
    cancel.cancel()
    with pytest.raises(Cancelled):
        run_claude(request(claude), cancel)
    assert not marker.exists()


# -- parse_response --


def test_prefers_structured_output_and_falls_back_to_the_requested_model():
    response = parse_response(
        json.dumps({"structured_output": {"what": "x"}, "result": "ignored"}), "m"
    )
    assert response.output == {"what": "x"}
    assert response.model == "m"
    assert response.cost_usd is None
    assert response.text == "ignored"


def test_rejects_errors_and_output_that_is_not_the_requested_json():
    with pytest.raises(ClaudeError, match="Credit balance too low"):
        parse_response(
            json.dumps({"is_error": True, "result": "Credit balance too low"}), "m"
        )
    with pytest.raises(ClaudeError):
        parse_response("not json", "m")
    with pytest.raises(ClaudeError, match="wasn't the requested JSON"):
        parse_response(json.dumps({"result": "plain text"}), "m")


def test_parse_response_edge_cases():
    with pytest.raises(ClaudeError, match="unexpected reply"):
        parse_response("[1, 2]", "m")
    with pytest.raises(ClaudeError, match="no result"):
        parse_response("{}", "m")
    with pytest.raises(ClaudeError, match="something other than JSON"):
        parse_response("", "m")
    with pytest.raises(ClaudeError, match="reported an error: unknown"):
        parse_response(json.dumps({"is_error": True}), "m")


def test_parse_response_reads_the_model_only_when_exactly_one_answered():
    one = parse_response(
        json.dumps({"result": "{}", "modelUsage": {"claude-x": {}}}), "m"
    )
    assert one.model == "claude-x"
    two = parse_response(
        json.dumps({"result": "{}", "modelUsage": {"a": {}, "b": {}}}), "m"
    )
    assert two.model == "m"


def test_parse_response_ignores_a_cost_that_is_not_a_number():
    assert (
        parse_response(
            json.dumps({"result": "{}", "total_cost_usd": "1"}), "m"
        ).cost_usd
        is None
    )
    assert (
        parse_response(
            json.dumps({"result": "{}", "total_cost_usd": True}), "m"
        ).cost_usd
        is None
    )
    assert (
        parse_response(json.dumps({"result": "{}", "total_cost_usd": 2}), "m").cost_usd
        == 2.0
    )


def test_parse_response_as_text():
    response = parse_response(
        json.dumps({"result": "plain text"}), "m", json_output=False
    )
    assert response.output == "plain text"
