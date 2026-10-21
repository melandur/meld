import sys
import threading
import time

import pytest

from meld.ailens.cards import Card, layout_cards
from meld.ailens.models import Cancelled, LineRange
from meld.ailens.process import (
    CancelToken,
    ProcessNotFound,
    ProcessTimeout,
    run_process,
)
from meld.ailens.worker import Worker


class QueueDispatch:
    """Collects callbacks like GLib.idle_add, run when the test says so"""

    def __init__(self):
        self.pending = []
        self.lock = threading.Lock()

    def __call__(self, callback):
        with self.lock:
            self.pending.append(callback)

    def drain(self, timeout=5.0):
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            with self.lock:
                pending, self.pending = self.pending, []
            for callback in pending:
                callback()
            if pending:
                return
            time.sleep(0.01)


@pytest.fixture
def dispatch():
    return QueueDispatch()


@pytest.fixture
def worker(dispatch):
    w = Worker(dispatch=dispatch)
    yield w
    w.shutdown()


def test_worker_delivers_result(worker, dispatch):
    results = []
    worker.submit("job", lambda cancel: 42, results.append, pytest.fail)
    dispatch.drain()
    assert results == [42]
    assert not worker.busy


def test_worker_delivers_error(worker, dispatch):
    errors = []

    def boom(cancel):
        raise ValueError("nope")

    worker.submit("job", boom, pytest.fail, errors.append)
    dispatch.drain()
    assert [str(e) for e in errors] == ["nope"]


def test_newer_exclusive_job_wins(worker, dispatch):
    started = threading.Event()
    release = threading.Event()
    results = []

    def slow(cancel):
        started.set()
        release.wait(5)
        return "old"

    first = worker.submit("job", slow, results.append, results.append)
    started.wait(5)
    worker.submit("job", lambda cancel: "new", results.append, results.append)
    assert first.cancelled
    release.set()
    dispatch.drain()
    time.sleep(0.05)
    dispatch.drain(timeout=0.2)
    assert results == ["new"]


def test_non_exclusive_jobs_all_deliver(worker, dispatch):
    results = []
    for i in range(3):
        worker.submit(
            "job", lambda cancel, i=i: i, results.append, pytest.fail, exclusive=False
        )
    for _ in range(3):
        dispatch.drain()
    assert sorted(results) == [0, 1, 2]


def test_shutdown_drops_pending_callbacks(dispatch):
    worker = Worker(dispatch=dispatch)
    results = []
    worker.submit("job", lambda cancel: 1, results.append, results.append)
    time.sleep(0.1)
    worker.shutdown()
    dispatch.drain(timeout=0.2)
    assert results == []
    # And nothing new runs
    token = worker.submit("job", lambda cancel: 2, results.append, results.append)
    assert token.cancelled


def test_cancel_all_cancels_tokens(worker, dispatch):
    seen = []
    started = threading.Event()

    def wait(cancel):
        started.set()
        cancel.wait(5)
        seen.append(cancel.cancelled)
        cancel.raise_if_cancelled()

    worker.submit("job", wait, pytest.fail, lambda err: seen.append(err))
    started.wait(5)
    worker.cancel_all()
    time.sleep(0.1)
    dispatch.drain(timeout=0.2)
    assert seen == [True]


# -- process --


def test_run_process_output(tmp_path):
    result = run_process(
        [sys.executable, "-c", "import sys; print(sys.stdin.read().upper())"],
        input=b"hello",
        cwd=str(tmp_path),
    )
    assert result.returncode == 0
    assert result.stdout.strip() == b"HELLO"


def test_run_process_exit_code_is_not_an_error():
    result = run_process([sys.executable, "-c", "import sys; sys.exit(3)"])
    assert result.returncode == 3


def test_run_process_missing_binary(tmp_path):
    with pytest.raises(ProcessNotFound):
        run_process([str(tmp_path / "does-not-exist")])


def test_run_process_timeout():
    with pytest.raises(ProcessTimeout):
        run_process([sys.executable, "-c", "import time; time.sleep(10)"], timeout=0.2)


def test_run_process_cancel_kills():
    token = CancelToken()
    threading.Timer(0.2, token.cancel).start()
    start = time.monotonic()
    with pytest.raises(Cancelled):
        run_process([sys.executable, "-c", "import time; time.sleep(10)"], cancel=token)
    assert time.monotonic() - start < 5


def test_run_process_already_cancelled():
    token = CancelToken()
    token.cancel()
    with pytest.raises(Cancelled):
        run_process([sys.executable, "-c", "pass"], cancel=token)


def test_cancel_token_callbacks():
    token = CancelToken()
    calls = []
    remove = token.on_cancel(lambda: calls.append("a"))
    token.on_cancel(lambda: calls.append("b"))
    remove()
    token.cancel()
    token.cancel()
    assert calls == ["b"]
    # Registering after cancel calls at once
    token.on_cancel(lambda: calls.append("c"))
    assert calls == ["b", "c"]


# -- cards --


def test_layout_cards_keeps_level_and_avoids_overlap():
    assert layout_cards([], []) == []
    assert layout_cards([10, 100], [20, 20]) == [10, 100]
    assert layout_cards([10, 15, 16], [20, 20, 5], gap=4) == [10, 34, 58]
    # Cards above the viewport (negative tops) keep their place
    assert layout_cards([-50, -40], [20, 20], gap=0) == [-50, -30]


def test_card_anchor():
    added = Card(
        key="a",
        kind="commit",
        added=(LineRange(5, 7), LineRange(2, 2)),
        deleted=(LineRange(1, 1),),
    )
    assert added.anchor == ("new", 2)
    deleted = Card(key="d", kind="commit", added=(), deleted=(LineRange(9, 9),))
    assert deleted.anchor == ("old", 9)
    empty = Card(key="e", kind="unknown", added=(), deleted=())
    assert empty.anchor == ("new", 1)


@pytest.mark.parametrize(
    ("text", "expected"),
    [
        ("", ""),
        ("one line", "one line"),
        ("wrapped\nprose here", "wrapped prose here"),
        ("para one\nwrapped\n\npara two", "para one wrapped\n\npara two"),
        (
            "Intro:\n- item one\n  continues\n- item two",
            "Intro:\n- item one continues\n- item two",
        ),
        ("1. first\n2. second", "1. first\n2. second"),
        ("crlf\r\nline", "crlf line"),
        ("  padded  \n\n\n\n  text ", "padded\n\ntext"),
    ],
)
def test_reflow(text, expected):
    from meld.ailens.cards import reflow

    assert reflow(text) == expected
