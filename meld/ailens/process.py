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

"""Cancellable child processes, run without a shell"""

import logging
import os
import subprocess
import threading
from collections.abc import Callable, Mapping, Sequence
from dataclasses import dataclass

from meld.ailens.models import AiLensError, Cancelled

log = logging.getLogger(__name__)

#: How often a waiting call checks whether it was cancelled, in seconds
_POLL_INTERVAL = 0.05


class CancelToken:
    """Thread-safe cancellation flag shared by every call of one operation"""

    def __init__(self) -> None:
        self._event = threading.Event()
        self._callbacks: list[Callable[[], None]] = []
        self._lock = threading.Lock()

    @property
    def cancelled(self) -> bool:
        return self._event.is_set()

    def cancel(self) -> None:
        with self._lock:
            if self._event.is_set():
                return
            self._event.set()
            callbacks, self._callbacks = self._callbacks, []
        for callback in callbacks:
            try:
                callback()
            except Exception:
                log.exception("Cancel callback failed")

    def raise_if_cancelled(self) -> None:
        if self._event.is_set():
            raise Cancelled("Cancelled")

    def wait(self, timeout: float) -> bool:
        """Sleep up to ``timeout`` seconds; True if cancelled meanwhile"""
        return self._event.wait(timeout)

    def on_cancel(self, callback: Callable[[], None]) -> Callable[[], None]:
        """Call ``callback`` once on cancel (now, if already cancelled)

        Returns a function that unregisters it.
        """
        with self._lock:
            if not self._event.is_set():
                self._callbacks.append(callback)

                def remove() -> None:
                    with self._lock:
                        if callback in self._callbacks:
                            self._callbacks.remove(callback)

                return remove
        callback()
        return lambda: None


#: A token that is never cancelled
NEVER = CancelToken()


@dataclass(frozen=True)
class ProcessResult:
    stdout: bytes
    stderr: bytes
    returncode: int


class ProcessError(AiLensError):
    def __init__(
        self,
        message: str,
        args: Sequence[str],
        returncode: int | None = None,
        stderr: str = "",
    ) -> None:
        super().__init__(message)
        self.command = list(args)
        self.returncode = returncode
        self.stderr = stderr


class ProcessNotFound(ProcessError):
    """The executable does not exist or cannot be run"""


class ProcessTimeout(ProcessError):
    pass


def run_process(
    args: Sequence[str],
    *,
    cwd: str | None = None,
    input: bytes | None = None,
    env: Mapping[str, str] | None = None,
    cancel: CancelToken | None = None,
    timeout: float | None = None,
    inherit_env: bool = True,
) -> ProcessResult:
    """Run ``args`` to completion and return its output

    Never uses a shell. Raises :class:`Cancelled` when ``cancel`` fires (the
    process is killed), :class:`ProcessTimeout` after ``timeout`` seconds and
    :class:`ProcessNotFound` when the executable can't be started. A non-zero
    exit status is not an error here: callers check ``returncode``.
    """
    cancel = cancel or NEVER
    cancel.raise_if_cancelled()

    full_env = None
    if env is not None:
        # inherit_env=False runs with exactly ``env``, so variables such as
        # GIT_DIR that Meld itself was started with can be left out
        full_env = dict(os.environ) if inherit_env else {}
        full_env.update(env)

    try:
        proc = subprocess.Popen(
            list(args),
            cwd=cwd,
            env=full_env,
            stdin=subprocess.PIPE if input is not None else subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            start_new_session=True,
        )
    except (FileNotFoundError, PermissionError, NotADirectoryError) as err:
        raise ProcessNotFound(
            f"Could not run {args[0]}: {err.strerror or err}", args
        ) from err
    except OSError as err:
        raise ProcessError(f"Could not run {args[0]}: {err}", args) from err

    def kill() -> None:
        try:
            proc.kill()
        except OSError:
            pass

    unregister = cancel.on_cancel(kill)
    try:
        try:
            stdout, stderr = proc.communicate(input=input, timeout=timeout)
        except subprocess.TimeoutExpired:
            kill()
            proc.communicate()
            if cancel.cancelled:
                raise Cancelled("Cancelled") from None
            raise ProcessTimeout(
                f"{args[0]} took longer than {timeout:g} seconds", args
            ) from None
        except BrokenPipeError:
            # The child exited before reading all of its input; collect what
            # it wrote and report its exit status as usual.
            stdout, stderr = proc.communicate()
    finally:
        unregister()

    if cancel.cancelled:
        raise Cancelled("Cancelled")
    return ProcessResult(stdout, stderr, proc.returncode)
