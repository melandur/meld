"""Deterministic git repositories for the AI Lens tests

Authors, dates and config are fixed and isolated from the user's git
config, so the same steps always produce the same SHAs.
"""

import os
import subprocess
from collections.abc import Mapping, Sequence
from pathlib import Path

HUMAN = ("Jane Doe", "jane@example.com")

#: 2023-11-14T22:13:20Z; each commit is one minute after the previous one
START_EPOCH = 1_700_000_000


def co_authored_by_claude(model: str) -> tuple[str, str]:
    """The trailer Claude Code adds, e.g. ``co_authored_by_claude("Opus 5.5")``"""
    return ("Co-Authored-By", f"Claude {model} <noreply@anthropic.com>")


def isolated_git_env() -> dict[str, str]:
    env = dict(os.environ)
    for key in list(env):
        if key.startswith("GIT_"):
            del env[key]
    env.update(
        {
            "GIT_CONFIG_GLOBAL": os.devnull,
            "GIT_CONFIG_NOSYSTEM": "1",
            "LC_ALL": "C",
            "HOME": env.get("HOME", "/nonexistent"),
        }
    )
    return env


class FixtureRepo:
    """A git repo in ``path``, built step by step

    >>> repo = FixtureRepo(tmp_path)
    >>> repo.commit("Adds a", {"a.txt": "a\\n"}, label="first")
    >>> repo.shas["first"]
    """

    def __init__(self, path: Path, *, initial_branch: str = "main") -> None:
        self.path = Path(os.path.realpath(path))
        self.path.mkdir(parents=True, exist_ok=True)
        self.env = isolated_git_env()
        self.shas: dict[str, str] = {}
        self._tick = 0
        self.git("init", "--quiet", f"--initial-branch={initial_branch}")
        self.git("config", "user.name", HUMAN[0])
        self.git("config", "user.email", HUMAN[1])
        self.git("config", "commit.gpgsign", "false")
        self.git("config", "tag.gpgsign", "false")
        self.git("config", "core.autocrlf", "false")

    # -- plumbing --

    def git(
        self,
        *args: str,
        env: Mapping[str, str] | None = None,
        cwd: Path | None = None,
        input: bytes | None = None,
    ) -> str:
        result = subprocess.run(
            ["git", *args],
            cwd=cwd or self.path,
            env=dict(env or self.env),
            input=input,
            capture_output=True,
            check=False,
        )
        if result.returncode != 0:
            raise RuntimeError(
                f"git {' '.join(args)} failed: {result.stderr.decode(errors='replace')}"
            )
        return result.stdout.decode()

    def _commit_env(self, author: tuple[str, str]) -> dict[str, str]:
        date = f"@{START_EPOCH + 60 * self._tick} +0000"
        self._tick += 1
        env = dict(self.env)
        env.update(
            {
                "GIT_AUTHOR_NAME": author[0],
                "GIT_AUTHOR_EMAIL": author[1],
                "GIT_AUTHOR_DATE": date,
                "GIT_COMMITTER_NAME": author[0],
                "GIT_COMMITTER_EMAIL": author[1],
                "GIT_COMMITTER_DATE": date,
            }
        )
        return env

    def _record(self, label: str | None) -> str:
        sha = self.git("rev-parse", "HEAD").strip()
        if label is not None:
            if label in self.shas:
                raise ValueError(f"Duplicate fixture label {label!r}")
            self.shas[label] = sha
        return sha

    def resolve(self, ref: str) -> str:
        return self.shas.get(ref, ref)

    def write_files(
        self, files: Mapping[str, str | bytes | None], *, root: Path | None = None
    ) -> None:
        """Write files on disk only; None deletes one"""
        root = root or self.path
        for name, content in files.items():
            target = root / name
            if content is None:
                target.unlink()
                continue
            target.parent.mkdir(parents=True, exist_ok=True)
            if isinstance(content, bytes):
                target.write_bytes(content)
            else:
                target.write_text(content, encoding="utf-8", newline="")

    # -- steps --

    def commit(
        self,
        message: str,
        files: Mapping[str, str | bytes | None] | None = None,
        *,
        label: str | None = None,
        author: tuple[str, str] = HUMAN,
        renames: Mapping[str, str] | None = None,
        trailers: Sequence[tuple[str, str]] = (),
        cwd: Path | None = None,
    ) -> str:
        """Commit ``files`` (None deletes); renames are applied first"""
        root = cwd or self.path
        for old, new in (renames or {}).items():
            (root / new).parent.mkdir(parents=True, exist_ok=True)
            self.git("mv", old, new, cwd=root)
        for name, content in (files or {}).items():
            if content is None:
                self.git("rm", "--quiet", "--", name, cwd=root)
            else:
                self.write_files({name: content}, root=root)
        if trailers:
            message += "\n\n" + "\n".join(f"{k}: {v}" for k, v in trailers)
        self.git("add", "--all", cwd=root)
        self.git(
            "commit",
            "--quiet",
            "--allow-empty",
            "--no-verify",
            "-m",
            message,
            env=self._commit_env(author),
            cwd=root,
        )
        sha = self.git("rev-parse", "HEAD", cwd=root).strip()
        if label is not None:
            if label in self.shas:
                raise ValueError(f"Duplicate fixture label {label!r}")
            self.shas[label] = sha
        return sha

    def branch(self, name: str, start: str = "HEAD") -> None:
        """Create a branch at ``start`` and check it out"""
        self.git("checkout", "--quiet", "-b", name, self.resolve(start))

    def checkout(self, ref: str) -> None:
        self.git("checkout", "--quiet", self.resolve(ref))

    def tag(self, name: str, ref: str = "HEAD", *, message: str | None = None):
        if message is None:
            self.git("tag", name, self.resolve(ref))
        else:
            self.git(
                "tag",
                "-a",
                name,
                "-m",
                message,
                self.resolve(ref),
                env=self._commit_env(HUMAN),
            )

    def merge(
        self,
        ref: str,
        message: str,
        *,
        label: str | None = None,
        author: tuple[str, str] = HUMAN,
        strategy_option: str | None = None,
    ) -> str:
        """Always creates a merge commit (``--no-ff``)"""
        args = ["merge", "--quiet", "--no-ff", "--no-edit", "-m", message]
        if strategy_option:
            args += ["-X", strategy_option]
        self.git(*args, self.resolve(ref), env=self._commit_env(author))
        return self._record(label)

    def stage(self, *paths: str) -> None:
        if paths:
            self.git("add", "--", *paths)
        else:
            self.git("add", "--all")

    def note(self, ref: str, text: str, notes_ref: str = "ai-lens") -> None:
        self.git(
            "notes",
            f"--ref={notes_ref}",
            "add",
            "-f",
            "-m",
            text,
            self.resolve(ref),
            env=self._commit_env(HUMAN),
        )

    def worktree(self, path: Path, branch: str, start: str = "HEAD") -> Path:
        """Add a worktree at ``path`` on a new ``branch``"""
        self.git(
            "worktree", "add", "--quiet", "-b", branch, str(path), self.resolve(start)
        )
        return Path(os.path.realpath(path))
