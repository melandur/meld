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

"""The git commands AI Lens needs, with parsed results

git runs as a child process, never through a shell, with its output
pinned to the shape the parsers expect whatever the user's config says.
Every method that runs git takes an optional :class:`CancelToken` and
raises :class:`~meld.ailens.models.Cancelled` when it fires, and
:class:`GitError` when git fails.

Paths are relative to the worktree root, ``/``-separated, as git prints
them. Text read from git is UTF-8; paths and file contents are decoded
with ``surrogateescape`` so that any path survives the round trip.
"""

import dataclasses
import datetime
import os
import stat
from collections.abc import Iterable, Mapping, Sequence

from meld.ailens.models import (
    AiLensError,
    GitBlameLine,
    GitCommit,
    GitFileChange,
    GitFileDiff,
    GitRef,
    GitWorktree,
    UncommittedLayer,
)
from meld.ailens.parsers import (
    REF_FORMAT,
    GitFileStats,
    log_format,
    parse_blame,
    parse_diff,
    parse_log,
    parse_name_status,
    parse_numstat,
    parse_patch_ids,
    parse_refs,
    parse_rev_list_parents,
    parse_worktrees,
)
from meld.ailens.process import (
    CancelToken,
    ProcessNotFound,
    ProcessResult,
    run_process,
)

#: Keeps git's output in the shape the parsers expect, whatever the
#: user's git config says
_OUTPUT_CONFIG = (
    "core.quotePath=false",
    "color.ui=false",
    "log.showSignature=false",
    "i18n.logOutputEncoding=UTF-8",
    "diff.noprefix=false",
    "diff.mnemonicPrefix=false",
    "diff.relative=false",
    "diff.suppressBlankEmpty=false",
    "diff.submodule=short",
    "diff.renames=true",
    "blame.ignoreRevsFile=",
    "gc.auto=0",
    "maintenance.auto=false",
)

#: Variables that point git at a repository, index or object store; they
#: are set when Meld is started by git (difftool, hooks) and must not
#: leak into commands for other repositories or worktrees
_LOCAL_ENV_VARS = (
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_COMMON_DIR",
    "GIT_CONFIG",
    "GIT_CONFIG_COUNT",
    "GIT_CONFIG_PARAMETERS",
    "GIT_DIR",
    "GIT_GRAFT_FILE",
    "GIT_IMPLICIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_NO_REPLACE_OBJECTS",
    "GIT_OBJECT_DIRECTORY",
    "GIT_PREFIX",
    "GIT_REPLACE_REF_BASE",
    "GIT_SHALLOW_FILE",
    "GIT_WORK_TREE",
)

#: Commands that reject ``--literal-pathspecs`` (they take paths, not
#: pathspecs)
_NO_LITERAL_PATHSPECS = frozenset({"check-ignore"})

#: How much of a file git checks for a NUL byte to decide it's binary
BINARY_CHECK_BYTES = 8000
#: git's default ``core.bigFileThreshold``: diff treats larger files as
#: binary
BIG_FILE_THRESHOLD = 512 * 1024 * 1024

_READ_CHUNK = 64 * 1024


class GitError(AiLensError):
    """git failed, or could not be run"""

    def __init__(
        self,
        message: str,
        args: Sequence[str] = (),
        returncode: int | None = None,
        stderr: str = "",
    ) -> None:
        super().__init__(message)
        self.command = list(args)
        self.returncode = returncode
        self.stderr = stderr


class GitNotFound(GitError):
    """The git executable doesn't exist or can't be run"""


def _text(data: bytes) -> str:
    """Paths and file contents"""
    return data.decode("utf-8", "surrogateescape")


def _message(data: bytes) -> str:
    """Commit messages, notes, ref subjects"""
    return data.decode("utf-8", "replace")


def _notes_ref(ref: str) -> str:
    """``ai-lens`` -> ``refs/notes/ai-lens``, as git expands notes refs"""
    if ref.startswith("refs/"):
        return ref
    if ref.startswith("notes/"):
        return "refs/" + ref
    return "refs/notes/" + ref


def path_sort_key(path: str) -> bytes:
    """git's path order: by bytes, not by locale"""
    return path.encode("utf-8", "surrogateescape")


class GitRepository:
    """One worktree of a git repository

    ``root`` is the top folder of the worktree; commands about commits run
    there, and commands about the index or the files on disk run in the
    worktree they are given (this one by default). Other worktrees of the
    same repository share its commits, refs and notes.

    Instances hold no state besides their settings and can be used from
    any thread.
    """

    def __init__(
        self,
        root: str,
        *,
        git_path: str = "git",
        env: Mapping[str, str] | None = None,
    ) -> None:
        self.root = os.path.abspath(root)
        self.git_path = git_path
        base = dict(os.environ if env is None else env)
        for name in _LOCAL_ENV_VARS:
            base.pop(name, None)
        base.update(
            {
                "LC_ALL": "C",
                "GIT_OPTIONAL_LOCKS": "0",
                "GIT_TERMINAL_PROMPT": "0",
            }
        )
        self._env = base

    def __repr__(self) -> str:
        return f"GitRepository({self.root!r})"

    @classmethod
    def discover(
        cls,
        path: str,
        *,
        git_path: str = "git",
        env: Mapping[str, str] | None = None,
        cancel: CancelToken | None = None,
    ) -> "GitRepository":
        """The repository of the worktree that contains ``path``

        ``path`` may be any file or folder inside a worktree. Raises
        :class:`AiLensError` with a message for the user when it isn't in
        one (or git is missing, or the repository is bare).
        """
        path = os.path.abspath(path)
        if not os.path.exists(path):
            raise AiLensError(f"“{path}” doesn't exist.")
        cwd = path if os.path.isdir(path) else os.path.dirname(path)

        probe = cls(cwd, git_path=git_path, env=env)
        result = probe.run(
            ["rev-parse", "--is-bare-repository", "--is-inside-work-tree"],
            ok=(0, 128),
            cancel=cancel,
        )
        if result.returncode == 128:
            stderr = _message(result.stderr).strip()
            if "not a git repository" in stderr:
                raise AiLensError(f"“{path}” isn't in a Git repository.")
            raise GitError(
                f"Git can't open the repository at “{path}”: {stderr}",
                ["rev-parse"],
                128,
                stderr,
            )
        bare, inside = _message(result.stdout).split()
        if bare == "true":
            raise AiLensError(
                f"“{path}” is a bare Git repository; AI Lens needs one with "
                "a working tree."
            )
        if inside != "true":
            raise AiLensError(
                f"“{path}” is inside a Git folder, not in a working tree."
            )

        top = probe.run(["rev-parse", "--show-toplevel"], cancel=cancel)
        return cls(_text(top.stdout).rstrip("\n"), git_path=git_path, env=env)

    # -- running git --

    def run(
        self,
        args: Sequence[str],
        *,
        worktree: str | None = None,
        input: bytes | None = None,
        ok: Iterable[int] = (0,),
        cancel: CancelToken | None = None,
    ) -> ProcessResult:
        """Run ``git <args>`` in ``worktree`` (default: :attr:`root`)

        Raises :class:`GitError` unless git exits with a code in ``ok``.
        """
        cwd = worktree or self.root
        full = [self.git_path, "--no-pager"]
        if not args or args[0] not in _NO_LITERAL_PATHSPECS:
            # Paths are never patterns
            full.append("--literal-pathspecs")
        for setting in _OUTPUT_CONFIG:
            full += ["-c", setting]
        full += args
        try:
            result = run_process(
                full,
                cwd=cwd,
                input=input,
                env=self._env,
                inherit_env=False,
                cancel=cancel,
            )
        except ProcessNotFound as err:
            # A missing working folder is reported like a missing program
            if not os.path.isdir(cwd):
                raise GitError(f"The folder “{cwd}” doesn't exist.", args) from err
            raise GitNotFound(
                f"Git wasn't found at “{self.git_path}”. Install Git, or add "
                "it to PATH.",
                args,
            ) from err

        if result.returncode not in tuple(ok):
            stderr = _message(result.stderr).strip()
            command = args[0] if args else ""
            raise GitError(
                f"git {command} failed: {stderr or f'exit status {result.returncode}'}",
                args,
                result.returncode,
                stderr,
            )
        return result

    def _lines(self, args: Sequence[str], **kwargs) -> list[str]:
        out = _text(self.run(args, **kwargs).stdout)
        return [line for line in out.split("\n") if line]

    # -- revisions --

    def resolve_commit(
        self, ref: str, *, cancel: CancelToken | None = None
    ) -> str | None:
        """Full SHA of the commit ``ref`` names (tags are peeled), or None"""
        if not ref:
            return None
        result = self.run(
            [
                "rev-parse",
                "--verify",
                "--quiet",
                "--end-of-options",
                f"{ref}^{{commit}}",
            ],
            ok=(0, 1, 128),
            cancel=cancel,
        )
        if result.returncode != 0:
            return None
        return _text(result.stdout).strip() or None

    def short_sha(self, sha: str, *, cancel: CancelToken | None = None) -> str:
        """The shortest unique abbreviation of ``sha``, at least 7 long"""
        result = self.run(
            ["rev-parse", "--verify", "--short=7", "--end-of-options", sha],
            cancel=cancel,
        )
        return _text(result.stdout).strip()

    def head(
        self, worktree: str | None = None, *, cancel: CancelToken | None = None
    ) -> str | None:
        """SHA of HEAD in ``worktree``; None on an unborn branch"""
        result = self.run(
            ["rev-parse", "--verify", "--quiet", "HEAD^{commit}"],
            worktree=worktree,
            ok=(0, 1, 128),
            cancel=cancel,
        )
        if result.returncode != 0:
            return None
        return _text(result.stdout).strip() or None

    def git_dir(
        self, worktree: str | None = None, *, cancel: CancelToken | None = None
    ) -> str:
        """Absolute path of the worktree's git folder (holds its index)"""
        result = self.run(
            ["rev-parse", "--absolute-git-dir"], worktree=worktree, cancel=cancel
        )
        return _text(result.stdout).rstrip("\n")

    def empty_tree(self, *, cancel: CancelToken | None = None) -> str:
        """ID of the empty tree in this repository's hash"""
        result = self.run(
            ["hash-object", "-t", "tree", "--stdin"], input=b"", cancel=cancel
        )
        return _text(result.stdout).strip()

    def is_shallow(self, *, cancel: CancelToken | None = None) -> bool:
        """A shallow clone lacks older history, so blame stops at its edge"""
        result = self.run(["rev-parse", "--is-shallow-repository"], cancel=cancel)
        return _text(result.stdout).strip() == "true"

    def merge_base(
        self, a: str, b: str, *, cancel: CancelToken | None = None
    ) -> str | None:
        """None when the commits have no common ancestor"""
        result = self.run(
            ["merge-base", "--end-of-options", a, b], ok=(0, 1), cancel=cancel
        )
        if result.returncode != 0:
            return None
        return _text(result.stdout).strip() or None

    def is_ancestor(
        self, ancestor: str, descendant: str, *, cancel: CancelToken | None = None
    ) -> bool:
        """Whether ``ancestor`` is reachable from ``descendant`` (or equal)"""
        result = self.run(
            ["merge-base", "--is-ancestor", "--end-of-options", ancestor, descendant],
            ok=(0, 1),
            cancel=cancel,
        )
        return result.returncode == 0

    # -- refs and worktrees --

    def worktrees(self, *, cancel: CancelToken | None = None) -> list[GitWorktree]:
        """Every worktree of the repository, the main one first"""
        result = self.run(["worktree", "list", "--porcelain", "-z"], cancel=cancel)
        return parse_worktrees(_text(result.stdout))

    def refs(self, *, cancel: CancelToken | None = None) -> list[GitRef]:
        """Local branches, remote branches (not ``*/HEAD``) and tags

        Annotated tags are peeled to their commit. Branches checked out in
        a worktree have :attr:`GitRef.worktree` set; the one checked out in
        this worktree is :attr:`GitRef.current`.
        """
        result = self.run(
            ["for-each-ref", REF_FORMAT, "refs/heads", "refs/remotes", "refs/tags"],
            cancel=cancel,
        )
        refs = parse_refs(_message(result.stdout))

        checked_out: dict[str, str] = {}
        for worktree in self.worktrees(cancel=cancel):
            if worktree.branch and not worktree.bare:
                checked_out.setdefault(worktree.branch, worktree.path)

        own = os.path.realpath(self.root)
        updated = []
        for ref in refs:
            path = checked_out.get(ref.name) if ref.type == "branch" else None
            current = path is not None and os.path.realpath(path) == own
            updated.append(dataclasses.replace(ref, current=current, worktree=path))
        return updated

    # -- commits --

    def log(
        self,
        revs: Sequence[str],
        *,
        max_count: int | None = None,
        skip: int = 0,
        grep: str | None = None,
        all_refs: bool = False,
        notes_ref: str | None = None,
        cancel: CancelToken | None = None,
    ) -> list[GitCommit]:
        """Commits selected by ``revs`` (e.g. ``["A..B"]``), newest first

        ``all_refs`` adds every branch, remote branch and tag, plus HEAD
        (not ``--all``, which would include notes and stash commits).
        ``grep`` keeps commits whose message, author name or author email
        contains it, ignoring case. ``notes_ref`` fills in each commit's
        :attr:`GitCommit.note` from that notes ref. With no revisions on an
        unborn branch, there are no commits.
        """
        if grep is None:
            return self._log(revs, max_count, skip, None, all_refs, notes_ref, cancel)

        # git ANDs --grep with --author, so search each and merge
        limit = None if max_count is None else skip + max_count
        by_message = self._log(
            revs, limit, 0, f"--grep={grep}", all_refs, notes_ref, cancel
        )
        by_author = self._log(
            revs, limit, 0, f"--author={grep}", all_refs, notes_ref, cancel
        )
        seen = {commit.sha for commit in by_message}
        extra = [commit for commit in by_author if commit.sha not in seen]
        merged = _merge_by_date(by_message, extra)
        end = None if max_count is None else skip + max_count
        return merged[skip:end]

    def _log(
        self,
        revs: Sequence[str],
        max_count: int | None,
        skip: int,
        match: str | None,
        all_refs: bool,
        notes_ref: str | None,
        cancel: CancelToken | None,
    ) -> list[GitCommit]:
        args = ["log", "-z", log_format(notes_ref is not None), "--no-notes"]
        if notes_ref is not None:
            args.append(f"--notes={_notes_ref(notes_ref)}")
        if max_count is not None:
            args.append(f"--max-count={max_count}")
        if skip:
            args.append(f"--skip={skip}")
        if match is not None:
            args += ["--regexp-ignore-case", "--fixed-strings", match]
        rev_args = list(revs)
        if all_refs:
            args += ["--branches", "--remotes", "--tags"]
            if self.head(cancel=cancel) is not None:
                rev_args.insert(0, "HEAD")
        if not rev_args and not all_refs and self.head(cancel=cancel) is None:
            return []
        args += ["--end-of-options", *rev_args, "--"]
        result = self.run(args, cancel=cancel)
        return parse_log(_message(result.stdout), notes_ref is not None)

    def commits(
        self,
        shas: Sequence[str],
        *,
        notes_ref: str | None = None,
        cancel: CancelToken | None = None,
    ) -> list[GitCommit]:
        """The commits ``shas`` name, in that order, read with one git call"""
        unique = list(dict.fromkeys(shas))
        if not unique:
            return []
        args = ["log", "-z", log_format(notes_ref is not None), "--no-notes"]
        if notes_ref is not None:
            args.append(f"--notes={_notes_ref(notes_ref)}")
        args += ["--no-walk=unsorted", "--stdin"]
        revs = "".join(f"{sha}\n" for sha in unique)
        result = self.run(args, input=revs.encode(), cancel=cancel)
        return parse_log(_message(result.stdout), notes_ref is not None)

    def commit(
        self,
        sha: str,
        *,
        notes_ref: str | None = None,
        cancel: CancelToken | None = None,
    ) -> GitCommit:
        resolved = self.resolve_commit(sha, cancel=cancel)
        if resolved is None:
            raise AiLensError(f"“{sha}” isn't a commit in this repository.")
        [found] = self.commits([resolved], notes_ref=notes_ref, cancel=cancel)
        return found

    def parents(
        self, revs: Sequence[str], *, cancel: CancelToken | None = None
    ) -> dict[str, list[str]]:
        """SHA -> parent SHAs for the commits ``revs`` select"""
        result = self.run(
            ["rev-list", "--parents", "--end-of-options", *revs, "--"], cancel=cancel
        )
        return parse_rev_list_parents(_text(result.stdout))

    def commits_touching(
        self,
        revs: Sequence[str],
        paths: Sequence[str],
        *,
        full_history: bool = False,
        cancel: CancelToken | None = None,
    ) -> list[str]:
        """SHAs of the commits ``revs`` select that touched any of ``paths``

        Newest first. ``full_history`` turns off git's history
        simplification, which skips side branches a merge did not take.
        """
        args = ["rev-list"]
        if full_history:
            args.append("--full-history")
        args += ["--end-of-options", *revs, "--", *paths]
        return self._lines(args, cancel=cancel)

    def patch_ids(
        self, shas: Sequence[str], *, cancel: CancelToken | None = None
    ) -> dict[str, str]:
        """SHA -> ``git patch-id --stable``; merges and empty commits have none"""
        if not shas:
            return {}
        revs = "".join(f"{sha}\n" for sha in dict.fromkeys(shas))
        patches = self.run(
            [
                "log",
                "-p",
                "--no-color",
                "--no-ext-diff",
                "--format=commit %H",
                "--no-walk=unsorted",
                "--stdin",
            ],
            input=revs.encode(),
            cancel=cancel,
        )
        result = self.run(["patch-id", "--stable"], input=patches.stdout, cancel=cancel)
        return parse_patch_ids(_text(result.stdout))

    def commit_diffs(
        self, sha: str, *, cancel: CancelToken | None = None
    ) -> list[tuple[GitFileChange, GitFileDiff]]:
        """A commit's changes against its first parent, with renames

        A root commit is compared with the empty tree. Files git shows no
        textual change for (mode changes, binary files) get a diff without
        hunks.
        """
        parent = self.resolve_commit(f"{sha}^1", cancel=cancel)
        revs = [parent, sha] if parent else ["--root", sha]
        changes = parse_name_status(
            _text(
                self.run(
                    [
                        "diff-tree",
                        "-r",
                        "-z",
                        "-M",
                        "--name-status",
                        "--no-commit-id",
                        *revs,
                        "--",
                    ],
                    cancel=cancel,
                ).stdout
            )
        )
        if not changes:
            return []
        diffs = parse_diff(
            _text(
                self.run(
                    [
                        "diff-tree",
                        "-r",
                        "-p",
                        "-M",
                        "--no-color",
                        "--no-ext-diff",
                        "--no-commit-id",
                        *revs,
                        "--",
                    ],
                    cancel=cancel,
                ).stdout
            )
        )
        return [(change, _pick_diff(diffs, change)) for change in changes]

    # -- notes --

    def read_note(
        self,
        sha: str,
        notes_ref: str = "ai-lens",
        *,
        cancel: CancelToken | None = None,
    ) -> str | None:
        """The note on ``sha`` in ``notes_ref``, trimmed; None if there is none"""
        result = self.run(
            ["notes", f"--ref={_notes_ref(notes_ref)}", "show", sha],
            ok=(0, 1),
            cancel=cancel,
        )
        if result.returncode != 0:
            return None
        return _message(result.stdout).strip() or None

    def write_note(
        self,
        sha: str,
        text: str,
        notes_ref: str = "ai-lens",
        *,
        cancel: CancelToken | None = None,
    ) -> None:
        """Add or replace the note on ``sha`` in ``notes_ref``"""
        self.run(
            [
                "notes",
                f"--ref={_notes_ref(notes_ref)}",
                "add",
                "--force",
                "--file=-",
                sha,
            ],
            input=text.encode("utf-8"),
            cancel=cancel,
        )

    # -- diffs --

    def diff(
        self,
        revs: Sequence[str],
        paths: Sequence[str] = (),
        *,
        cached: bool = False,
        context: int = 3,
        worktree: str | None = None,
        cancel: CancelToken | None = None,
    ) -> list[GitFileDiff]:
        """``git diff -M [--cached] <revs> -- <paths>``, parsed

        With no ``revs``, compares the index with the files on disk (or,
        ``cached``, HEAD with the index) in ``worktree``. Adjacent hunks are
        never joined beyond their context.
        """
        args = [
            "diff",
            "--no-color",
            "--no-ext-diff",
            "-M",
            f"-U{context}",
            "--inter-hunk-context=0",
        ]
        if cached:
            args.append("--cached")
        if revs:
            args += ["--end-of-options", *revs]
        args += ["--", *paths]
        result = self.run(args, worktree=worktree, cancel=cancel)
        return parse_diff(_text(result.stdout))

    def file_diff(
        self,
        revs: Sequence[str],
        change: GitFileChange,
        *,
        cached: bool = False,
        context: int = 3,
        worktree: str | None = None,
        cancel: CancelToken | None = None,
    ) -> GitFileDiff | None:
        """The diff of one changed file; both paths of a rename are passed
        so git can pair them. None when git shows no difference.
        """
        if change.untracked:
            return self.untracked_diff(change.path, worktree=worktree, cancel=cancel)
        diffs = self.diff(
            revs,
            list(change.paths),
            cached=cached,
            context=context,
            worktree=worktree,
            cancel=cancel,
        )
        return _pick_diff(diffs, change) if diffs else None

    def name_status(
        self,
        revs: Sequence[str],
        *,
        cached: bool = False,
        worktree: str | None = None,
        cancel: CancelToken | None = None,
    ) -> list[GitFileChange]:
        """Files that differ, with renames; same arguments as :meth:`diff`"""
        args = ["diff", "--name-status", "-z", "-M", "--no-ext-diff"]
        if cached:
            args.append("--cached")
        if revs:
            args += ["--end-of-options", *revs]
        args.append("--")
        result = self.run(args, worktree=worktree, cancel=cancel)
        return parse_name_status(_text(result.stdout))

    def numstat(
        self,
        revs: Sequence[str],
        *,
        cached: bool = False,
        worktree: str | None = None,
        cancel: CancelToken | None = None,
    ) -> list[GitFileStats]:
        """Lines added and deleted per file; same arguments as :meth:`diff`"""
        args = ["diff", "--numstat", "-z", "-M", "--no-ext-diff"]
        if cached:
            args.append("--cached")
        if revs:
            args += ["--end-of-options", *revs]
        args.append("--")
        result = self.run(args, worktree=worktree, cancel=cancel)
        return parse_numstat(_text(result.stdout))

    def uncommitted_diffs(
        self,
        layer: UncommittedLayer,
        *,
        head: str | None = None,
        worktree: str | None = None,
        cancel: CancelToken | None = None,
    ) -> list[GitFileDiff]:
        """Every file's diff in one uncommitted layer, untracked files aside

        ``staged`` is ``head`` (default: HEAD; the empty tree on an unborn
        branch) -> index, with renames; ``unstaged`` is index -> files on
        disk.
        """
        if layer == "staged":
            return self.diff(
                [head] if head else [], cached=True, worktree=worktree, cancel=cancel
            )
        return self.diff([], worktree=worktree, cancel=cancel)

    def untracked_files(
        self, worktree: str | None = None, *, cancel: CancelToken | None = None
    ) -> list[str]:
        """Files on disk that git neither tracks nor ignores

        An untracked nested repository is listed as its folder, with a
        trailing ``/``.
        """
        result = self.run(
            ["ls-files", "-z", "--others", "--exclude-standard"],
            worktree=worktree,
            cancel=cancel,
        )
        return [path for path in _text(result.stdout).split("\0") if path]

    def ignored(
        self,
        paths: Sequence[str],
        worktree: str | None = None,
        *,
        cancel: CancelToken | None = None,
    ) -> set[str]:
        """Which of ``paths`` git ignores"""
        if not paths:
            return set()
        data = "".join(f"{path}\0" for path in paths).encode("utf-8", "surrogateescape")
        # Exits with 1 when none is ignored
        result = self.run(
            ["check-ignore", "-z", "--stdin"],
            worktree=worktree,
            input=data,
            ok=(0, 1),
            cancel=cancel,
        )
        return {path for path in _text(result.stdout).split("\0") if path}

    def untracked_diff(
        self,
        path: str,
        *,
        worktree: str | None = None,
        cancel: CancelToken | None = None,
    ) -> GitFileDiff | None:
        """An untracked file as an added file's diff; None for folders"""
        root = worktree or self.root
        full = os.path.join(root, path)
        if path.endswith("/") or (os.path.isdir(full) and not os.path.islink(full)):
            return None
        if not os.path.lexists(full):
            return None
        # Exits with 1 when the files differ, which they always do here
        result = self.run(
            [
                "diff",
                "--no-index",
                "--no-color",
                "--no-ext-diff",
                "-U3",
                "--inter-hunk-context=0",
                "--",
                "/dev/null",
                path,
            ],
            worktree=root,
            ok=(0, 1),
            cancel=cancel,
        )
        diffs = parse_diff(_text(result.stdout))
        return diffs[0] if diffs else None

    def untracked_stats(
        self,
        path: str,
        *,
        worktree: str | None = None,
        cancel: CancelToken | None = None,
    ) -> GitFileStats:
        """An untracked file's lines, all added

        Counts are None for files git treats as binary: a NUL byte in the
        first 8000 bytes, or larger than ``core.bigFileThreshold``. A file
        that vanished since it was listed counts as empty.
        """
        entry = _open_in_worktree(worktree or self.root, path)
        if entry is None:
            if os.path.isdir(os.path.join(worktree or self.root, path)):
                return GitFileStats(path, None, None)
            return GitFileStats(path, 0, 0)
        if isinstance(entry, bytes):
            return _count_lines_in(path, [entry])

        with os.fdopen(entry, "rb") as handle:
            if os.fstat(handle.fileno()).st_size > BIG_FILE_THRESHOLD:
                return GitFileStats(path, None, None)

            def chunks():
                while True:
                    if cancel is not None:
                        cancel.raise_if_cancelled()
                    chunk = handle.read(_READ_CHUNK)
                    if not chunk:
                        return
                    yield chunk

            return _count_lines_in(path, chunks())

    def blame(
        self,
        from_rev: str,
        to_rev: str,
        path: str,
        *,
        reverse: bool = False,
        cancel: CancelToken | None = None,
    ) -> list[GitBlameLine]:
        """Porcelain blame of ``path`` over ``from_rev..to_rev``

        Normally blames the file at ``to_rev``; lines older than
        ``from_rev`` are ``boundary``. With ``reverse``, blames the file at
        ``from_rev`` and reports the last commit each line existed in. A
        path the blamed version doesn't have gives no lines.
        """
        args = ["blame", "--porcelain"]
        # Root commits inside the range made their lines; without --root
        # git marks them boundary
        args.append("--reverse" if reverse else "--root")
        args += [f"{from_rev}..{to_rev}", "--", path]
        try:
            result = self.run(args, cancel=cancel)
        except GitError as err:
            if "no such path" in err.stderr:
                return []
            raise
        return parse_blame(_text(result.stdout))

    # -- file contents --

    def read_blob(
        self, rev: str, path: str, *, cancel: CancelToken | None = None
    ) -> bytes | None:
        """The file at ``path`` in commit ``rev``; None if it has no such file

        Symbolic links read as their target, as git stores them.
        """
        result = self.run(
            ["cat-file", "blob", f"{rev}:{path}"], ok=(0, 128), cancel=cancel
        )
        return result.stdout if result.returncode == 0 else None

    def read_index_file(
        self,
        path: str,
        worktree: str | None = None,
        *,
        cancel: CancelToken | None = None,
    ) -> bytes | None:
        """The staged version of ``path`` in ``worktree``'s index

        None when the index has no such file (or only conflict stages).
        """
        result = self.run(
            ["cat-file", "blob", f":{path}"],
            worktree=worktree,
            ok=(0, 128),
            cancel=cancel,
        )
        return result.stdout if result.returncode == 0 else None

    def read_worktree_file(
        self,
        path: str,
        worktree: str | None = None,
        *,
        cancel: CancelToken | None = None,
    ) -> bytes | None:
        """The file at ``path`` on disk in ``worktree``

        Never follows a symbolic link, so nothing outside the worktree is
        read: a link reads as its target, as git stores it, and a path
        through a linked folder reads as None, as does a missing file or a
        folder. Raises :class:`AiLensError` for paths that leave the
        worktree.
        """
        if cancel is not None:
            cancel.raise_if_cancelled()
        entry = _open_in_worktree(worktree or self.root, path)
        if entry is None or isinstance(entry, bytes):
            return entry
        with os.fdopen(entry, "rb") as handle:
            return handle.read()


def _merge_by_date(a: list[GitCommit], b: list[GitCommit]) -> list[GitCommit]:
    """Merge two newest-first commit lists, keeping each one's order"""
    merged = []
    i = j = 0
    while i < len(a) and j < len(b):
        if _timestamp(b[j]) > _timestamp(a[i]):
            merged.append(b[j])
            j += 1
        else:
            merged.append(a[i])
            i += 1
    return merged + a[i:] + b[j:]


def _timestamp(commit: GitCommit) -> float:
    date = commit.committer.date
    if date.endswith("Z"):
        date = date[:-1] + "+00:00"
    try:
        return datetime.datetime.fromisoformat(date).timestamp()
    except ValueError:
        return 0.0


def _pick_diff(diffs: list[GitFileDiff], change: GitFileChange) -> GitFileDiff:
    """The diff of ``change`` among ``diffs``"""
    if change.status == "D":
        for diff in diffs:
            if diff.old_path == change.path and diff.new_path is None:
                return diff
    for diff in diffs:
        if diff.new_path == change.path:
            return diff
    for diff in diffs:
        if diff.old_path == change.path:
            return diff
    return (
        diffs[0]
        if diffs
        else GitFileDiff(change.original_path or change.path, change.path, False, ())
    )


def _count_lines_in(path: str, chunks: Iterable[bytes]) -> GitFileStats:
    lines = 0
    position = 0
    last = b""
    for chunk in chunks:
        if (
            position < BINARY_CHECK_BYTES
            and b"\0" in chunk[: BINARY_CHECK_BYTES - position]
        ):
            return GitFileStats(path, None, None)
        lines += chunk.count(b"\n")
        position += len(chunk)
        last = chunk[-1:]
    if last and last != b"\n":
        lines += 1
    return GitFileStats(path, lines, 0)


def _split_relative(path: str) -> list[str]:
    parts = path.rstrip("/").split("/")
    if (
        not path
        or path.startswith("/")
        or any(part in ("", ".", "..") for part in parts)
    ):
        raise AiLensError(f"“{path}” isn't a path inside the worktree.")
    return parts


_O_BINARY = getattr(os, "O_BINARY", 0)
_O_NOFOLLOW = getattr(os, "O_NOFOLLOW", 0)
_O_DIRECTORY = getattr(os, "O_DIRECTORY", 0)
_HAVE_DIR_FD = os.open in os.supports_dir_fd and os.stat in os.supports_dir_fd


def _open_in_worktree(root: str, path: str) -> int | bytes | None:
    """A readable fd for a regular file, a link's target, or None

    Folders on the way must be real folders, not links.
    """
    parts = _split_relative(path)
    if not _HAVE_DIR_FD or not _O_NOFOLLOW:
        return _open_in_worktree_by_path(root, parts)

    dir_fd = os.open(root, os.O_RDONLY | _O_DIRECTORY)
    try:
        for part in parts[:-1]:
            try:
                fd = os.open(
                    part, os.O_RDONLY | _O_DIRECTORY | _O_NOFOLLOW, dir_fd=dir_fd
                )
            except OSError:
                # Missing, not a folder, or a link
                return None
            os.close(dir_fd)
            dir_fd = fd

        name = parts[-1]
        try:
            info = os.stat(name, dir_fd=dir_fd, follow_symlinks=False)
        except OSError:
            return None
        if stat.S_ISLNK(info.st_mode):
            return os.fsencode(os.readlink(name, dir_fd=dir_fd))
        if not stat.S_ISREG(info.st_mode):
            return None
        try:
            return os.open(name, os.O_RDONLY | _O_NOFOLLOW | _O_BINARY, dir_fd=dir_fd)
        except FileNotFoundError:
            return None
    finally:
        os.close(dir_fd)


def _open_in_worktree_by_path(root: str, parts: list[str]) -> int | bytes | None:
    """:func:`_open_in_worktree` for systems without ``dir_fd``"""
    current = root
    for part in parts[:-1]:
        current = os.path.join(current, part)
        try:
            info = os.lstat(current)
        except OSError:
            return None
        if not stat.S_ISDIR(info.st_mode):
            return None
    full = os.path.join(current, parts[-1])
    try:
        info = os.lstat(full)
    except OSError:
        return None
    if stat.S_ISLNK(info.st_mode):
        return os.fsencode(os.readlink(full))
    if not stat.S_ISREG(info.st_mode):
        return None
    try:
        return os.open(full, os.O_RDONLY | _O_NOFOLLOW | _O_BINARY)
    except FileNotFoundError:
        return None
