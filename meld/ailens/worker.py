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

"""Runs blocking jobs on threads and hands their results to the UI thread"""

import functools
import logging
import threading
from collections.abc import Callable
from concurrent.futures import ThreadPoolExecutor
from typing import Any

from meld.ailens.process import CancelToken

log = logging.getLogger(__name__)

#: Calls a function on the UI thread later; GLib.idle_add by default
Dispatch = Callable[[Callable[[], Any]], Any]


def _idle_dispatch(callback: Callable[[], Any]) -> None:
    from gi.repository import GLib

    def run() -> bool:
        callback()
        return GLib.SOURCE_REMOVE

    GLib.idle_add(run)


class Worker:
    """A small pool of threads for one tab

    Each job gets its own :class:`CancelToken`. Submitting an ``exclusive``
    job cancels the running one of the same name, whose result is then never
    delivered, so callbacks only ever see the newest answer. After
    :meth:`shutdown` no callback runs at all.
    """

    def __init__(self, max_workers: int = 3, dispatch: Dispatch | None = None):
        self._executor = ThreadPoolExecutor(
            max_workers=max_workers, thread_name_prefix="meld-ailens"
        )
        self._dispatch = dispatch or _idle_dispatch
        self._lock = threading.Lock()
        self._jobs: dict[str, tuple[int, CancelToken]] = {}
        self._tokens: set[CancelToken] = set()
        self._serial = 0
        self._closed = False

    def submit(
        self,
        name: str,
        work: Callable[[CancelToken], Any],
        on_done: Callable[[Any], None],
        on_failed: Callable[[Exception], None],
        *,
        exclusive: bool = True,
    ) -> CancelToken:
        token = CancelToken()
        with self._lock:
            if self._closed:
                token.cancel()
                return token
            self._serial += 1
            serial = self._serial
            if exclusive:
                previous = self._jobs.get(name)
                if previous is not None:
                    previous[1].cancel()
                self._jobs[name] = (serial, token)
            self._tokens.add(token)

        def current() -> bool:
            with self._lock:
                if self._closed or token.cancelled:
                    return False
                if exclusive:
                    job = self._jobs.get(name)
                    return job is not None and job[0] == serial
                return True

        def finish(callback: Callable[[], None]) -> None:
            def deliver() -> None:
                # Checked again on the UI thread: a newer job or shutdown
                # may have come in while this was queued
                if current():
                    try:
                        callback()
                    finally:
                        self._forget(name, serial, token, exclusive)
                else:
                    self._forget(name, serial, token, exclusive)

            self._dispatch(deliver)

        def run() -> None:
            try:
                if token.cancelled:
                    raise _Skipped
                result = work(token)
            except _Skipped:
                self._forget(name, serial, token, exclusive)
                return
            except Exception as err:
                finish(functools.partial(on_failed, err))
                return
            finish(functools.partial(on_done, result))

        try:
            self._executor.submit(run)
        except RuntimeError:
            # The executor shut down in between
            token.cancel()
        return token

    def _forget(self, name: str, serial: int, token: CancelToken, exclusive: bool):
        with self._lock:
            self._tokens.discard(token)
            if exclusive and self._jobs.get(name, (None,))[0] == serial:
                del self._jobs[name]

    def cancel(self, name: str) -> None:
        with self._lock:
            job = self._jobs.pop(name, None)
        if job is not None:
            job[1].cancel()

    def cancel_all(self) -> None:
        with self._lock:
            tokens = list(self._tokens)
            self._jobs.clear()
        for token in tokens:
            token.cancel()

    @property
    def busy(self) -> bool:
        with self._lock:
            return bool(self._tokens)

    def shutdown(self) -> None:
        """Cancel everything; no callback runs after this returns"""
        with self._lock:
            self._closed = True
        self.cancel_all()
        self._executor.shutdown(wait=False, cancel_futures=True)


class _Skipped(Exception):
    pass
