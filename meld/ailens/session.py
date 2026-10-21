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

"""One open comparison: its files, their Why cards, explaining and builds

Every method here blocks on git or Claude Code, so the UI calls them on a
worker thread (meld.ailens.worker). A session is used by one thread at a
time per method; its caches are guarded by a lock so that an explain job and
a file load can overlap.
"""

import datetime
import logging
import os
import threading
from collections.abc import Callable
from dataclasses import dataclass, field
from typing import Literal

from meld.ailens.agent import detect_agent
from meld.ailens.agent_setup import (
    AgentSetupOptions,
    plan_agent_setup,
    write_agent_setup,
)
from meld.ailens.attribution import ComparisonAttributor
from meld.ailens.build_facts import (
    BuildChange,
    BuildCommit,
    BuildFacts,
    BuildFile,
    BuildFileEntry,
    BuildRevision,
    BuildStats,
    merge_ranges,
)
from meld.ailens.build_file import BuildHeader
from meld.ailens.build_index import (
    find_builds,
    read_build_headers,
    resolve_build_folder,
)
from meld.ailens.build_narrative import write_narrative
from meld.ailens.cards import Card
from meld.ailens.claude import ClaudeNotFound
from meld.ailens.comparison import changed_files_with_stats, resolve_comparison
from meld.ailens.explanation import (
    DEFAULT_MESSAGE_FORMAT,
    MessageFormat,
    ParsedNote,
    explain_commit,
    find_change_explanations,
    parse_note,
)
from meld.ailens.generate_build import write_build
from meld.ailens.git import GitRepository
from meld.ailens.ignore import create_ignore_rules, is_ignored
from meld.ailens.infer import (
    IgnoredFile,
    NothingToExplain,
    compose_inferred_note,
    infer_explanation,
    infer_uncommitted_explanation,
)
from meld.ailens.models import (
    AiLensError,
    ChangeGroup,
    Comparison,
    FileAttribution,
    GitCommit,
    GitFileChange,
    Revision,
    RevisionKind,
)
from meld.ailens.picker import (
    COMMIT_PAGE_SIZE,
    PickerEntry,
    build_entries,
    short_sha,
)
from meld.ailens.process import CancelToken
from meld.ailens.uncommitted import UncommittedExplanations
from meld.conf import _

log = logging.getLogger(__name__)

#: Larger files are shown as "too large" instead of compared
MAX_FILE_BYTES = 8 * 1024 * 1024

#: How many bytes are checked for NUL to call a file binary, as git does
BINARY_SNIFF_BYTES = 8000

Progress = Callable[[str], None]


@dataclass(frozen=True)
class SessionConfig:
    claude_path: str = "claude"
    infer_enabled: bool = True
    infer_model: str = "claude-sonnet-5-5"
    build_model: str = "claude-opus-5-5"
    build_folder: str = "agent"
    notes_ref: str = "ai-lens"
    message_format: MessageFormat = DEFAULT_MESSAGE_FORMAT
    author_pattern: str | None = None
    generator: str = "meld"
    #: Replaces the Claude Code call, for tests
    run_claude: Callable | None = None


@dataclass(frozen=True)
class FileEntry:
    change: GitFileChange
    additions: int | None
    deletions: int | None

    @property
    def binary(self) -> bool:
        return self.additions is None and self.deletions is None


@dataclass
class FileView:
    """A file ready to compare: both versions and its cards"""

    entry: FileEntry
    #: Paths in the base and target versions, for labels and file names
    base_name: str
    target_name: str
    #: Contents; None when that side has no such file
    base_text: bytes | None
    target_text: bytes | None
    base_label: str
    target_label: str
    #: The file on disk, when the target is this worktree's working tree
    target_disk_path: str | None
    attribution: FileAttribution | None
    cards: list[Card] = field(default_factory=list)
    #: Shown in the Why column when there are no cards
    cards_placeholder: str = ""
    #: Shown instead of the comparison (binary, too large, ...), if set
    message: str = ""

    @property
    def change(self) -> GitFileChange:
        return self.entry.change


def _is_binary(data: bytes | None) -> bool:
    return data is not None and b"\0" in data[:BINARY_SNIFF_BYTES]


def _display_path(path: str) -> str:
    """Paths are decoded with surrogateescape; show such bytes as �"""
    return path.encode("utf-8", "surrogateescape").decode("utf-8", "replace")


class RepositoryInfo:
    """A repository (or worktree) a Git Compare tab is open on

    Outlives the tab's comparisons, so explanations of uncommitted hunks,
    which can't be saved, last until the tab is closed.
    """

    def __init__(self, repo: GitRepository) -> None:
        self.repo = repo
        self.root = repo.root
        self.uncommitted = UncommittedExplanations()
        #: HEAD of each worktree (None: this one), as the pickers last saw it
        self.worktree_heads: dict[str | None, str] = {}

    @classmethod
    def open(
        cls, path: str, config: SessionConfig, cancel: CancelToken | None = None
    ) -> "RepositoryInfo":
        if not os.path.exists(path):
            raise AiLensError(_("“%s” doesn’t exist.") % path)
        if os.path.isfile(path):
            path = os.path.dirname(path) or "."
        return cls(GitRepository.discover(path, cancel=cancel))

    @property
    def display_name(self) -> str:
        return os.path.basename(self.root.rstrip(os.sep)) or self.root

    def build_folder(self, config: SessionConfig) -> str:
        return resolve_build_folder(self.root, config.build_folder)

    def ignore_rules(self) -> str | None:
        try:
            with open(
                os.path.join(self.root, ".ailensignore"),
                encoding="utf-8",
                errors="replace",
            ) as handle:
                return handle.read()
        except FileNotFoundError:
            return None
        except OSError as err:
            log.warning("Can't read .ailensignore: %s", err)
            return None

    def picker_entries(
        self, side: Literal["base", "target"], cancel: CancelToken | None = None
    ) -> list[PickerEntry]:
        repo = self.repo
        worktrees = repo.worktrees(cancel=cancel)
        refs = repo.refs(cancel=cancel)
        head_sha = repo.head(cancel=cancel)
        head = repo.commit(head_sha, cancel=cancel) if head_sha else None
        commits = (
            repo.log(["HEAD"], all_refs=True, max_count=COMMIT_PAGE_SIZE, cancel=cancel)
            if head_sha or refs
            else []
        )
        head_branch = next((r.name for r in refs if r.current), None)
        heads: dict[str | None, str] = {}
        for worktree in worktrees:
            if worktree.head:
                same = os.path.realpath(worktree.path) == os.path.realpath(self.root)
                heads[None if same else worktree.path] = worktree.head
        self.worktree_heads = heads
        return build_entries(
            refs=refs,
            worktrees=worktrees,
            commits=commits,
            current_worktree=self.root,
            head=head,
            head_branch=head_branch,
            side=side,
        )

    def search_commits(
        self, query: str, cancel: CancelToken | None = None
    ) -> list[PickerEntry]:
        """Commits anywhere in history whose message or author matches"""
        query = query.strip()
        if not query:
            return []
        commits = self.repo.log(
            [], all_refs=True, grep=query, max_count=COMMIT_PAGE_SIZE, cancel=cancel
        )
        return build_entries(refs=[], commits=commits)

    def watch_paths(self) -> list[str]:
        """Files and folders whose changes may change the comparison"""
        paths = []
        try:
            git_dir = self.repo.git_dir()
            result = self.repo.run(
                ["rev-parse", "--path-format=absolute", "--git-common-dir"]
            )
            common = result.stdout.decode("utf-8", "surrogateescape").strip()
        except AiLensError:
            return [self.root]
        common = common or git_dir
        for folder in {git_dir, common}:
            for name in ("HEAD", "index", "packed-refs", "refs/heads", "refs/tags"):
                path = os.path.join(folder, name)
                if os.path.exists(path):
                    paths.append(path)
        notes = os.path.join(common, "refs", "notes")
        if os.path.isdir(notes):
            paths.append(notes)
        return sorted(set(paths))

    def set_up_agents(
        self, config: SessionConfig, cancel: CancelToken | None = None
    ) -> list[str]:
        """Write the CLAUDE.md section and the /why skill; returns the
        changed files relative to the repository"""
        options = AgentSetupOptions(
            build_folder=config.build_folder, notes_ref=config.notes_ref
        )
        files = plan_agent_setup(self.root, options)
        if cancel is not None:
            cancel.raise_if_cancelled()
        write_agent_setup(files)
        return [os.path.relpath(f.path, self.root) for f in files if f.changed]


class CompareSession:
    """A resolved comparison, its files, and their explanations"""

    def __init__(
        self, info: RepositoryInfo, comparison: Comparison, config: SessionConfig
    ) -> None:
        self.info = info
        self.repo = info.repo
        self.comparison = comparison
        self.config = config
        self.root = info.root
        self._attributor = ComparisonAttributor(self.repo, comparison)
        self._lock = threading.RLock()
        self._entries: list[FileEntry] | None = None
        self._commits: dict[str, GitCommit] = {}
        self._range: list[GitCommit] | None = None
        self._builds: list[BuildHeader] | None = None
        self._patch_ids: dict[str, str | None] = {}
        self._labels: dict[str, str] = {}

    @classmethod
    def open(
        cls,
        info: RepositoryInfo,
        base: Revision,
        target: Revision,
        config: SessionConfig,
        cancel: CancelToken | None = None,
    ) -> "CompareSession":
        comparison = resolve_comparison(info.repo, base, target, cancel=cancel)
        session = cls(info, comparison, config)
        session._labels = {
            "base": session._label(comparison.base, comparison.base_sha, cancel),
            "target": session._label(comparison.target, comparison.target_sha, cancel),
        }
        return session

    # -- labels --

    def _label(self, revision: Revision, sha: str, cancel: CancelToken | None) -> str:
        suffix = ""
        if revision.worktree:
            suffix = " (%s)" % os.path.basename(revision.worktree.rstrip(os.sep))
        if revision.kind is RevisionKind.INDEX:
            return _("Staged") + suffix
        if revision.kind is RevisionKind.WORKING_TREE:
            return _("Working Tree") + suffix
        ref = revision.ref or sha
        if sha.startswith(ref) or ref == sha:
            return self.repo.short_sha(sha, cancel=cancel)
        for prefix in ("refs/heads/", "refs/remotes/", "refs/tags/"):
            if ref.startswith(prefix):
                return ref[len(prefix) :]
        return ref

    @property
    def base_label(self) -> str:
        return self._labels.get("base", "")

    @property
    def target_label(self) -> str:
        return self._labels.get("target", "")

    @property
    def base_tooltip(self) -> str:
        c = self.comparison
        if c.base.kind is RevisionKind.INDEX:
            return _("The index of %s") % c.worktree_path
        return f"{self.base_label}\n{c.base_sha}"

    @property
    def target_tooltip(self) -> str:
        c = self.comparison
        if c.target.kind is RevisionKind.INDEX:
            return _("Staged changes in %s") % c.worktree_path
        if c.target.kind is RevisionKind.WORKING_TREE:
            return _("Files on disk in %s, untracked ones included") % c.worktree_path
        return f"{self.target_label}\n{c.target_sha}"

    @property
    def relation_text(self) -> str:
        c = self.comparison
        if c.base.kind is RevisionKind.INDEX:
            return _("Unstaged changes")
        if c.relation == "diverged" and c.merge_base:
            return _("Diverged: showing changes since the merge base %s") % short_sha(
                c.merge_base
            )
        if c.relation == "reversed":
            return _(
                "The target is older than the base; swap them to see what was added"
            )
        if c.relation == "unrelated":
            return _("No shared history: comparing the trees directly")
        if c.relation == "same" and not c.uncommitted:
            return _("Base and target are the same commit")
        return ""

    @property
    def follows_repository_changes(self) -> bool:
        """Whether index, worktree or ref changes can change this comparison"""
        c = self.comparison
        if c.uncommitted or c.base.uncommitted:
            return True
        return any(
            r.kind is RevisionKind.COMMIT
            and r.ref is not None
            and not (len(r.ref) >= 7 and all(ch in "0123456789abcdef" for ch in r.ref))
            for r in (c.base, c.target)
        )

    @property
    def can_build(self) -> bool:
        return not self.build_unavailable_reason

    @property
    def build_unavailable_reason(self) -> str:
        c = self.comparison
        if c.uncommitted or c.base.uncommitted:
            return _("Builds cover commits only, not staged or unstaged changes")
        if c.relation == "same":
            return _("Base and target are the same commit")
        return ""

    # -- files --

    def files(self, cancel: CancelToken | None = None) -> list[FileEntry]:
        with self._lock:
            if self._entries is not None:
                return self._entries
        changes, stats = changed_files_with_stats(
            self.repo, self.comparison, cancel=cancel
        )
        by_path = {s.path: s for s in stats}
        entries = []
        for change in changes:
            stat = by_path.get(change.path)
            entries.append(
                FileEntry(
                    change,
                    stat.additions if stat else None,
                    stat.deletions if stat else None,
                )
            )
        with self._lock:
            self._entries = entries
        return entries

    def _read_side(
        self, side: Literal["base", "target"], path: str, cancel: CancelToken | None
    ) -> bytes | None:
        c = self.comparison
        revision = c.base if side == "base" else c.target
        worktree = c.worktree_path
        if revision.kind is RevisionKind.INDEX:
            return self.repo.read_index_file(path, worktree=worktree, cancel=cancel)
        if revision.kind is RevisionKind.WORKING_TREE:
            return self.repo.read_worktree_file(path, worktree=worktree, cancel=cancel)
        rev = c.from_sha if side == "base" else c.target_sha
        return self.repo.read_blob(rev, path, cancel=cancel)

    def file_view(
        self, change: GitFileChange, cancel: CancelToken | None = None
    ) -> FileView:
        entry = next(
            (e for e in self.files(cancel) if e.change == change),
            FileEntry(change, None, None),
        )
        c = self.comparison
        base_path = change.original_path or change.path
        base_text = (
            None if change.status == "A" else self._read_side("base", base_path, cancel)
        )
        target_text = (
            None
            if change.status == "D"
            else self._read_side("target", change.path, cancel)
        )
        base_label = f"{_display_path(base_path)} — {self.base_label}"
        if (
            c.relation == "diverged"
            and c.merge_base
            and c.base.kind is RevisionKind.COMMIT
        ):
            base_label = f"{_display_path(base_path)} — " + _(
                "merge base %s"
            ) % short_sha(c.merge_base)
        target_label = f"{_display_path(change.path)} — {self.target_label}"
        disk = None
        if c.target.kind is RevisionKind.WORKING_TREE and change.status != "D":
            candidate = os.path.join(c.worktree_path, change.path)
            if os.path.isfile(candidate) and not os.path.islink(candidate):
                disk = candidate

        view = FileView(
            entry=entry,
            base_name=base_path,
            target_name=change.path,
            base_text=base_text,
            target_text=target_text,
            base_label=base_label,
            target_label=target_label,
            target_disk_path=disk,
            attribution=None,
        )

        sizes = [len(t) for t in (base_text, target_text) if t is not None]
        if change.untracked and change.path.endswith("/"):
            view.message = _("%s is a nested repository or submodule") % change.path
            return view
        if sizes and max(sizes) > MAX_FILE_BYTES:
            view.message = _("%s is too large to compare here (%d MB)") % (
                _display_path(change.path),
                max(sizes) // (1024 * 1024),
            )
        attribution = self._attributor.attribute(change, cancel=cancel)
        view.attribution = attribution
        binary = (
            (attribution.diff is not None and attribution.diff.binary)
            or _is_binary(base_text)
            or _is_binary(target_text)
        )
        submodule = attribution.diff is not None and any(
            line.content.startswith("Subproject commit ")
            for hunk in attribution.diff.hunks
            for line in hunk.lines
        )
        if submodule:
            view.message = _("%s is a submodule; its commit changed") % change.path
        elif binary and not view.message:
            view.message = _("%s is a binary file") % _display_path(change.path)
        view.cards = self._build_cards(view, cancel)
        if not view.cards:
            if attribution.diff is None or not attribution.diff.hunks:
                view.cards_placeholder = _("No text changes (e.g. only the mode)")
            else:
                view.cards_placeholder = _(
                    "Git can’t tell which commit made these changes"
                )
        if view.message and attribution.touched_by:
            names = ", ".join(short_sha(s) for s in attribution.touched_by[:8])
            view.message += "\n" + _("Changed by %s") % names
        return view

    # -- commits --

    def range_commits(self, cancel: CancelToken | None = None) -> list[GitCommit]:
        """The comparison's commits, oldest first"""
        with self._lock:
            if self._range is not None:
                return self._range
        c = self.comparison
        if c.relation in ("same", "reversed") or c.base.uncommitted:
            commits = []
        elif c.relation == "unrelated":
            commits = self.repo.log(
                [c.target_sha], notes_ref=self.config.notes_ref, cancel=cancel
            )
        else:
            commits = self.repo.log(
                [f"{c.from_sha}..{c.target_sha}"],
                notes_ref=self.config.notes_ref,
                cancel=cancel,
            )
        commits = list(reversed(commits))
        with self._lock:
            self._range = commits
            for commit in commits:
                self._commits.setdefault(commit.sha, commit)
        return commits

    def get_commit(self, sha: str, cancel: CancelToken | None = None) -> GitCommit:
        with self._lock:
            commit = self._commits.get(sha)
        if commit is not None:
            return commit
        commit = self.repo.commit(sha, notes_ref=self.config.notes_ref, cancel=cancel)
        with self._lock:
            self._commits[sha] = commit
        return commit

    def _refresh_commit(self, sha: str, cancel: CancelToken | None) -> None:
        commit = self.repo.commit(sha, notes_ref=self.config.notes_ref, cancel=cancel)
        with self._lock:
            self._commits[sha] = commit
            if self._range is not None:
                self._range = [commit if c.sha == sha else c for c in self._range]

    def commit_message(self, sha: str, cancel: CancelToken | None = None) -> str:
        commit = self.get_commit(sha, cancel)
        lines = [
            f"commit {commit.sha}",
            f"Author: {commit.author.name} <{commit.author.email}>",
            f"Date:   {commit.author.date}",
            "",
            commit.subject,
        ]
        if commit.body.strip():
            lines += ["", commit.body.rstrip()]
        if commit.note:
            lines += [
                "",
                _("Notes (%s):") % self.config.notes_ref,
                commit.note.rstrip(),
            ]
        return "\n".join(lines)

    def _builds_for(
        self, shas: list[str], cancel: CancelToken | None
    ) -> dict[str, list[str]]:
        with self._lock:
            headers = self._builds
        if headers is None:
            try:
                folder = self.info.build_folder(self.config)
                headers = read_build_headers(folder) if os.path.isdir(folder) else []
            except AiLensError as err:
                log.info("Can't read builds: %s", err)
                headers = []
            with self._lock:
                self._builds = headers
        if not headers:
            return {}
        known = {commit.sha for header in headers for commit in header.commits}
        with_patch_ids = any(c.patch_id for h in headers for c in h.commits)
        missing = [s for s in shas if s not in known and s not in self._patch_ids]
        if with_patch_ids and missing:
            ids = self.repo.patch_ids(missing, cancel=cancel)
            with self._lock:
                for sha in missing:
                    self._patch_ids[sha] = ids.get(sha)
        return {
            sha: find_builds(headers, sha, self._patch_ids.get(sha)) for sha in shas
        }

    # -- cards --

    def _note(self, commit: GitCommit) -> ParsedNote | None:
        return parse_note(commit.note) if commit.note else None

    def cards(self, view: FileView, cancel: CancelToken | None = None) -> list[Card]:
        """The view's cards again, e.g. after an explanation was added"""
        return self._build_cards(view, cancel)

    def _build_cards(self, view: FileView, cancel: CancelToken | None) -> list[Card]:
        attribution = view.attribution
        if attribution is None or attribution.diff is None or attribution.diff.binary:
            return []
        range_order = {c.sha: i for i, c in enumerate(self.range_commits(cancel))}
        shas = sorted({g.sha for g in attribution.groups if g.sha})
        builds = self._builds_for(shas, cancel)
        layers = attribution.layers or {}
        placed = {
            layer: self.info.uncommitted.place(layer, layers.get(layer, []))
            for layer in ("staged", "unstaged")
        }
        infer = self.config.infer_enabled
        cards = []
        for index, group in enumerate(attribution.groups):
            cards.append(
                self._card(
                    group, index, range_order, builds, layers, placed, infer, cancel
                )
            )
        return cards

    def _card(
        self,
        group: ChangeGroup,
        index: int,
        range_order: dict[str, int],
        builds: dict[str, list[str]],
        layers: dict,
        placed: dict,
        infer: bool,
        cancel: CancelToken | None,
    ) -> Card:
        key = "%d:%s:%s" % (
            group.hunk_index,
            group.uncommitted or group.sha or "unknown",
            index,
        )
        common = {
            "key": key,
            "added": tuple(group.added),
            "deleted": tuple(group.deleted),
        }
        if group.uncommitted is not None:
            layer = group.uncommitted
            changes = find_change_explanations(group.origins, placed[layer])
            explained = all(
                self.info.uncommitted.is_explained(layer, diff)
                for diff in layers.get(layer, [])
            )
            return Card(
                kind=layer,
                change_what=_join_texts(c.what for c in changes),
                change_why=_join_texts(c.why for c in changes),
                change_model=next((c.model for c in changes), ""),
                explain="uncommitted" if infer and not explained else None,
                **common,
            )
        if group.sha is None:
            return Card(kind="unknown", **common)

        commit = self.get_commit(group.sha, cancel)
        explanation = explain_commit(commit, self.config.message_format)
        agent = detect_agent(commit, self.config.author_pattern)
        note = self._note(commit)
        changes = find_change_explanations(group.origins, note.changes) if note else []
        changes_explained = note is not None and (
            note.change_model is not None or bool(note.changes)
        )
        explain = None
        if infer:
            if explanation.why is None:
                explain = "commit"
            elif not changes_explained:
                explain = "changes"
        message = commit.subject + ("\n\n" + commit.body if commit.body else "")
        return Card(
            kind="commit",
            sha=commit.sha,
            short_sha=short_sha(commit.sha),
            subject=commit.subject,
            message=message,
            author=commit.author.name,
            date=commit.author.date,
            agent_name=agent.display_name if agent else "",
            agent_model=(agent.model or "") if agent else "",
            what=explanation.what,
            why=explanation.why or "",
            source=explanation.source,
            inferred_by=explanation.model or "",
            change_what=_join_texts(c.what for c in changes),
            change_why=_join_texts(c.why for c in changes),
            change_model=(note.change_model or "") if note and changes else "",
            explain=explain,
            color_index=range_order.get(commit.sha, -1),
            builds=tuple(builds.get(commit.sha, ())),
            **common,
        )

    # -- explaining --

    def _require_infer(self) -> None:
        if not self.config.infer_enabled:
            raise AiLensError(
                _("Explaining with Claude Code is turned off in the preferences.")
            )

    def explain_card(
        self, view: FileView, key: str, cancel: CancelToken | None = None
    ) -> None:
        card = next((c for c in view.cards if c.key == key), None)
        if card is None:
            raise AiLensError(_("That change isn’t in this file any more."))
        if card.kind == "commit" and card.sha:
            self.explain_commit(card.sha, cancel)
        elif card.kind in ("staged", "unstaged"):
            self.explain_uncommitted(view, card.kind, cancel)
        else:
            raise AiLensError(_("Git can’t tell which commit made this change."))

    def explain_commit(self, sha: str, cancel: CancelToken | None = None) -> None:
        """Have Claude Code explain a commit and its changes; saved as a note"""
        self._require_infer()
        commit = self.get_commit(sha, cancel)
        files = self.repo.commit_diffs(sha, cancel=cancel)
        kwargs = {}
        if self.config.run_claude is not None:
            kwargs["run"] = self.config.run_claude
        inferred = infer_explanation(
            commit,
            files,
            model=self.config.infer_model,
            ignore_rules=self.info.ignore_rules(),
            claude_path=self.config.claude_path,
            cancel=cancel,
            **kwargs,
        )
        note = compose_inferred_note(commit, inferred, self.config.message_format)
        if cancel is not None:
            cancel.raise_if_cancelled()
        self.repo.write_note(sha, note, notes_ref=self.config.notes_ref, cancel=cancel)
        self._refresh_commit(sha, cancel)

    def explain_uncommitted(
        self, view: FileView, layer: str, cancel: CancelToken | None = None
    ) -> int:
        """Have Claude Code explain a file's staged or unstaged hunks

        Kept in memory only; returns how many hunks were explained.
        """
        self._require_infer()
        attribution = view.attribution or self._attributor.attribute(
            view.change, cancel=cancel
        )
        kwargs = {}
        if self.config.run_claude is not None:
            kwargs["run"] = self.config.run_claude
        kept = 0
        for diff in (attribution.layers or {}).get(layer, []):
            if self.info.uncommitted.is_explained(layer, diff):
                continue
            result = infer_uncommitted_explanation(
                layer,
                diff,
                model=self.config.infer_model,
                ignore_rules=self.info.ignore_rules(),
                claude_path=self.config.claude_path,
                cancel=cancel,
                **kwargs,
            )
            kept += self.info.uncommitted.remember(
                layer, diff, result.changes, result.model
            )
        return kept

    def _commit_needs_explaining(self, commit: GitCommit) -> bool:
        explanation = explain_commit(commit, self.config.message_format)
        note = self._note(commit)
        changes_explained = note is not None and (
            note.change_model is not None or bool(note.changes)
        )
        return explanation.why is None or not changes_explained

    def explain_all(
        self,
        cancel: CancelToken | None = None,
        progress: Progress | None = None,
        *,
        commits_only: bool = False,
    ) -> int:
        """Explain every commit and uncommitted hunk not explained yet

        Keeps going past single failures; a missing Claude Code or a
        cancel stops at once. Returns how many were explained, or raises
        an error naming what failed.
        """
        self._require_infer()
        progress = progress or (lambda text: None)
        failures: list[str] = []
        done = 0
        todo = [
            c for c in self.range_commits(cancel) if self._commit_needs_explaining(c)
        ]
        for index, commit in enumerate(todo, 1):
            if cancel is not None:
                cancel.raise_if_cancelled()
            progress(
                _("Explaining commit %(sha)s (%(n)d of %(total)d)…")
                % {"sha": short_sha(commit.sha), "n": index, "total": len(todo)}
            )
            try:
                self.explain_commit(commit.sha, cancel)
                done += 1
            except ClaudeNotFound:
                raise
            except AiLensError as err:
                failures.append(f"{short_sha(commit.sha)}: {err}")

        if not commits_only and (
            self.comparison.uncommitted or self.comparison.base.uncommitted
        ):
            entries = self.files(cancel)
            for index, entry in enumerate(entries, 1):
                if cancel is not None:
                    cancel.raise_if_cancelled()
                attribution = self._attributor.attribute(entry.change, cancel=cancel)
                for layer in ("staged", "unstaged"):
                    diffs = (attribution.layers or {}).get(layer, [])
                    if all(self.info.uncommitted.is_explained(layer, d) for d in diffs):
                        continue
                    progress(
                        _(
                            "Explaining %(layer)s changes in %(path)s (%(n)d of %(total)d)…"
                        )
                        % {
                            "layer": _("staged")
                            if layer == "staged"
                            else _("unstaged"),
                            "path": _display_path(entry.change.path),
                            "n": index,
                            "total": len(entries),
                        }
                    )
                    view = FileView(
                        entry,
                        entry.change.path,
                        entry.change.path,
                        None,
                        None,
                        "",
                        "",
                        None,
                        attribution,
                    )
                    try:
                        if self.explain_uncommitted(view, layer, cancel):
                            done += 1
                    except (IgnoredFile, NothingToExplain):
                        continue
                    except ClaudeNotFound:
                        raise
                    except AiLensError as err:
                        failures.append(f"{_display_path(entry.change.path)}: {err}")

        if failures:
            raise AiLensError(
                _("Explained %(done)d; %(failed)d failed. First: %(first)s")
                % {"done": done, "failed": len(failures), "first": failures[0]}
            )
        return done

    # -- builds --

    def gather_build_facts(
        self, cancel: CancelToken | None = None, progress: Progress | None = None
    ) -> BuildFacts:
        if not self.can_build:
            raise AiLensError(self.build_unavailable_reason)
        progress = progress or (lambda text: None)
        rules = create_ignore_rules(self.info.ignore_rules())
        entries = self.files(cancel)
        files: list[BuildFile] = []
        for index, entry in enumerate(entries):
            if cancel is not None:
                cancel.raise_if_cancelled()
            progress(
                _("Reading the changes (%(n)d/%(total)d files)…")
                % {"n": index, "total": len(entries)}
            )
            change = entry.change
            ignored = is_ignored(rules, change.path, change.original_path)
            attribution = (
                None if ignored else self._attributor.attribute(change, cancel=cancel)
            )
            build_entries_: list[BuildFileEntry] = []
            binary = bool(attribution and attribution.diff and attribution.diff.binary)
            if attribution is not None and not binary:
                view = FileView(
                    entry,
                    change.path,
                    change.path,
                    None,
                    None,
                    "",
                    "",
                    None,
                    attribution,
                )
                for card in self._build_cards(view, cancel):
                    changes = None
                    if card.change_what or card.change_why:
                        changes = [BuildChange(card.change_what, card.change_why)]
                    found = next(
                        (
                            e
                            for e in build_entries_
                            if e.sha == card.sha and e.changes == changes
                        ),
                        None,
                    )
                    if found is None:
                        found = BuildFileEntry(sha=card.sha, changes=changes)
                        build_entries_.append(found)
                    found.added = merge_ranges([*found.added, *card.added])
                    found.deleted = merge_ranges([*found.deleted, *card.deleted])
            files.append(
                BuildFile(
                    path=change.path,
                    original_path=change.original_path,
                    status=change.status,
                    additions=entry.additions,
                    deletions=entry.deletions,
                    binary=binary or entry.binary,
                    ignored=ignored,
                    entries=build_entries_,
                    touched_by=list(attribution.touched_by) if attribution else [],
                )
            )

        commits = self.range_commits(cancel)
        patch_ids = self.repo.patch_ids([c.sha for c in commits], cancel=cancel)
        build_commits = []
        for commit in commits:
            explanation = explain_commit(commit, self.config.message_format)
            agent = detect_agent(commit, self.config.author_pattern)
            note = self._note(commit)
            changes_explained = note is not None and (
                note.change_model is not None or bool(note.changes)
            )
            build_commits.append(
                BuildCommit(
                    sha=commit.sha,
                    short_sha=self.repo.short_sha(commit.sha, cancel=cancel),
                    patch_id=patch_ids.get(commit.sha),
                    author=commit.author.name,
                    agent="claude-code" if agent else "human",
                    agent_model=agent.model if agent else None,
                    agent_name=agent.display_name if agent else None,
                    what=explanation.what,
                    why=explanation.why,
                    why_source=explanation.source,
                    inferred_by=explanation.model
                    if explanation.source == "inferred"
                    else None,
                    changes_inferred_by=(note.change_model or _("a model"))
                    if changes_explained
                    else None,
                )
            )

        c = self.comparison

        def revision(rev: Revision, sha: str, label: str) -> BuildRevision:
            ref = None if sha.startswith(label) or label == short_sha(sha) else label
            return BuildRevision(sha, self.repo.short_sha(sha, cancel=cancel), ref)

        return BuildFacts(
            base=revision(c.base, c.base_sha, self.base_label),
            target=revision(c.target, c.target_sha, self.target_label),
            merge_base=c.merge_base,
            relation=c.relation,
            stats=BuildStats(
                files=len(files),
                insertions=sum(f.additions or 0 for f in files),
                deletions=sum(f.deletions or 0 for f in files),
                commits=len(build_commits),
            ),
            commits=build_commits,
            files=files,
        )

    def generate_build(
        self, cancel: CancelToken | None = None, progress: Progress | None = None
    ) -> str:
        """Explain what isn't yet (if allowed), then write the build file

        Returns the build file's path.
        """
        if not self.can_build:
            raise AiLensError(self.build_unavailable_reason)
        progress = progress or (lambda text: None)
        if self.config.infer_enabled:
            self.explain_all(cancel, progress, commits_only=True)
        facts = self.gather_build_facts(cancel, progress)
        progress(_("Writing the summary with %s…") % self.config.build_model)
        narrative = write_narrative(
            facts,
            self.config.build_model,
            claude_path=self.config.claude_path,
            cancel=cancel,
            run=self.config.run_claude,
        )
        if cancel is not None:
            cancel.raise_if_cancelled()
        built = write_build(
            self.root,
            self.config.build_folder,
            facts,
            narrative,
            self.config.generator,
            datetime.datetime.now(datetime.timezone.utc),
        )
        with self._lock:
            self._builds = None
        return built.path


def _join_texts(texts) -> str:
    return "\n\n".join(t.strip() for t in texts if t and t.strip())
