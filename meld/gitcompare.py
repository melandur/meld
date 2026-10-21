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

"""Git Compare: compare any two revisions, with why each change was made

A tab with the changed files on the left and a two-way file comparison on
the right, extended with the Why column (meld.ui.whycolumn). All git and
Claude Code work runs on worker threads (meld.ailens.worker) and comes back
to the main loop; nothing here blocks the UI.
"""

import atexit
import logging
import os
import shutil
import stat
import tempfile
import weakref
from collections.abc import Callable

from gi.repository import Adw, Gdk, Gio, GLib, GObject, Gtk, Pango

from meld.ailens.cards import Card
from meld.ailens.models import AiLensError, Cancelled, Revision, RevisionKind
from meld.ailens.picker import PickerEntry, apply_pick
from meld.ailens.session import (
    CompareSession,
    FileEntry,
    FileView,
    RepositoryInfo,
    SessionConfig,
)
from meld.ailens.worker import Worker
from meld.conf import _
from meld.filediff import FileDiff
from meld.melddoc import MeldDoc
from meld.settings import get_settings
from meld.ui.revisionpicker import RevisionPicker
from meld.ui.whycolumn import WhyColumn

log = logging.getLogger(__name__)

#: Status letters and their names, as Source Control shows them
STATUS_NAMES = {
    "A": _("Added"),
    "C": _("Copied"),
    "D": _("Deleted"),
    "M": _("Modified"),
    "R": _("Renamed"),
    "T": _("Type changed"),
    "U": _("Untracked"),
    "X": _("Unknown"),
}

#: How long to wait after the repository changes before reloading, in ms
RELOAD_DELAY = 400

# Temp folders of open comparisons, removed at exit if a tab couldn't
_temp_dirs: "weakref.WeakSet[GitCompare]" = weakref.WeakSet()


def _cleanup_all() -> None:
    for doc in list(_temp_dirs):
        doc._remove_temp_dir()


atexit.register(_cleanup_all)


def config_from_settings() -> SessionConfig:
    settings = get_settings()
    if settings is None:
        return SessionConfig()

    def get(key: str, getter: str, default):
        schema = settings.props.settings_schema
        if schema is None or not schema.has_key(key):
            return default
        return getattr(settings, getter)(key)

    defaults = SessionConfig()
    return SessionConfig(
        claude_path=get("ailens-claude-path", "get_string", defaults.claude_path)
        or defaults.claude_path,
        infer_enabled=get("ailens-infer-enabled", "get_boolean", True),
        infer_model=get("ailens-infer-model", "get_string", defaults.infer_model)
        or defaults.infer_model,
        build_model=get("ailens-build-model", "get_string", defaults.build_model)
        or defaults.build_model,
        build_folder=get("ailens-build-folder", "get_string", defaults.build_folder)
        or defaults.build_folder,
        notes_ref=get("ailens-notes-ref", "get_string", defaults.notes_ref)
        or defaults.notes_ref,
        author_pattern=get("ailens-author-pattern", "get_string", "") or None,
    )


class _FileRow(GObject.Object):
    """A folder or file in the changed-files tree"""

    def __init__(self, name: str, entry: FileEntry | None, children: list):
        super().__init__()
        self.name = name
        self.entry = entry
        self.children = children


def build_file_tree(entries: list[FileEntry]) -> list[_FileRow]:
    """Folders and files, folders first; chains of single folders compacted"""
    root: dict = {}
    for entry in entries:
        parts = entry.change.path.split("/")
        node = root
        for part in parts[:-1]:
            node = node.setdefault(part + "/", {})
        name = parts[-1]
        if name in node:
            # A file deleted from the index but still on disk is listed
            # twice: deleted, and untracked
            name += " (%s)" % (
                _("untracked") if entry.change.untracked else entry.change.status
            )
        node[name] = entry

    def rows(node: dict) -> list[_FileRow]:
        folders, files = [], []
        for name, value in sorted(node.items(), key=lambda kv: kv[0].lower()):
            if isinstance(value, dict):
                label = name.rstrip("/")
                # Compact a/b/c into one row while each has only one folder
                while len(value) == 1:
                    (child_name, child), *_rest = value.items()
                    if not isinstance(child, dict):
                        break
                    label += "/" + child_name.rstrip("/")
                    value = child
                folders.append(_FileRow(label, None, rows(value)))
            else:
                files.append(_FileRow(name, value, []))
        return folders + files

    return rows(root)


class GitCompare(Gtk.Box, MeldDoc):
    """Compare two revisions of a git repository, with the Why column"""

    __gtype_name__ = "GitCompare"

    close_signal = MeldDoc.close_signal
    create_diff_signal = MeldDoc.create_diff_signal
    file_changed_signal = MeldDoc.file_changed_signal
    tab_state_changed = MeldDoc.tab_state_changed

    tab_title = GObject.Property(type=str, nick="Title used for tab labels")
    tab_tooltip = GObject.Property(type=str, nick="Tooltip used for tab labels")

    def __init__(self, *, config: SessionConfig | None = None) -> None:
        super().__init__()
        # Gtk.Box's __init__ chains to MeldDoc's without arguments; see VcView
        MeldDoc.__init__(self)
        self.set_orientation(Gtk.Orientation.VERTICAL)
        self.config = config or config_from_settings()
        self.worker = Worker()
        self.repo_info: RepositoryInfo | None = None
        self.session: CompareSession | None = None
        self.base = Revision.commit("HEAD")
        self.target = Revision.working_tree()
        self._entries: list[FileEntry] = []
        self._current: FileEntry | None = None
        self._view: FileView | None = None
        self._temp_dir: str | None = None
        self._monitors: list[Gio.FileMonitor] = []
        self._reload_id = 0
        self._closed = False
        self._hovered = ""
        self._busy: set[str] = set()
        self._scroll_pending = False
        self.tab_title = _("Git Compare")

        self._build_header()
        self._build_body()
        self._update_actions()
        _temp_dirs.add(self)

    # -- widgets --

    def _build_header(self) -> None:
        bar = Gtk.Box(orientation=Gtk.Orientation.HORIZONTAL, spacing=6)
        bar.add_css_class("ailens-toolbar")
        bar.set_margin_top(6)
        bar.set_margin_bottom(6)
        bar.set_margin_start(6)
        bar.set_margin_end(6)

        self.repo_button = Gtk.Button()
        self.repo_button.add_css_class("flat")
        self.repo_label = Gtk.Label(label=_("No repository"))
        self.repo_label.set_ellipsize(Pango.EllipsizeMode.START)
        self.repo_label.set_max_width_chars(24)
        self.repo_button.set_child(self.repo_label)
        self.repo_button.set_tooltip_text(_("Choose a repository or worktree"))
        self.repo_button.connect("clicked", self._on_choose_repository)
        bar.append(self.repo_button)

        self.base_picker = RevisionPicker(_("Base"))
        self.base_picker.connect("picked", self._on_picked, "base")
        self.base_picker.connect("search", self._on_search, "base")
        bar.append(self.base_picker)

        self.swap_button = Gtk.Button.new_from_icon_name(
            "object-flip-horizontal-symbolic"
        )
        self.swap_button.set_tooltip_text(_("Swap base and target"))
        self.swap_button.connect("clicked", lambda *a: self.swap())
        bar.append(self.swap_button)

        self.target_picker = RevisionPicker(_("Target"))
        self.target_picker.connect("picked", self._on_picked, "target")
        self.target_picker.connect("search", self._on_search, "target")
        bar.append(self.target_picker)

        self.relation_label = Gtk.Label(xalign=0)
        self.relation_label.add_css_class("dim-label")
        self.relation_label.set_ellipsize(Pango.EllipsizeMode.END)
        self.relation_label.set_hexpand(True)
        bar.append(self.relation_label)

        self.spinner = Gtk.Spinner()
        bar.append(self.spinner)

        self.refresh_button = Gtk.Button.new_from_icon_name("view-refresh-symbolic")
        self.refresh_button.set_tooltip_text(_("Reload the comparison"))
        self.refresh_button.connect("clicked", lambda *a: self.reload())
        bar.append(self.refresh_button)

        self.explain_button = Gtk.Button(label=_("Explain"))
        self.explain_button.set_tooltip_text(
            _("Explain every change that isn’t explained yet, with Claude Code")
        )
        self.explain_button.connect("clicked", lambda *a: self.explain_all())
        bar.append(self.explain_button)

        self.build_button = Gtk.Button(label=_("Explain & Build"))
        self.build_button.add_css_class("suggested-action")
        self.build_button.connect("clicked", lambda *a: self.generate_build())
        bar.append(self.build_button)

        menu = Gio.Menu()
        menu.append(_("Set Up Agent Instructions"), "ailens.setup-agents")
        menu.append(_("Open Builds Folder"), "ailens.open-builds")
        menu.append(_("Open File in New Tab"), "ailens.open-file")
        self.menu_button = Gtk.MenuButton(icon_name="view-more-symbolic")
        self.menu_button.set_menu_model(menu)
        self.menu_button.set_tooltip_text(_("More AI Lens actions"))
        bar.append(self.menu_button)

        self.ailens_actions = Gio.SimpleActionGroup()
        for name, callback in (
            ("setup-agents", lambda *a: self.setup_agents()),
            ("open-builds", lambda *a: self.open_builds_folder()),
            ("open-file", lambda *a: self.open_current_file()),
        ):
            action = Gio.SimpleAction.new(name, None)
            action.connect("activate", callback)
            self.ailens_actions.add_action(action)
        self.insert_action_group("ailens", self.ailens_actions)

        self.append(bar)

        self.banner = Adw.Banner()
        self.banner.connect("button-clicked", self._on_banner_button)
        self._banner_action: Callable[[], None] | None = None
        self.append(self.banner)

    def _build_body(self) -> None:
        self.paned = Gtk.Paned(orientation=Gtk.Orientation.HORIZONTAL)
        self.paned.set_vexpand(True)
        self.paned.set_position(300)
        self.paned.set_shrink_start_child(False)

        # Changed files, as a tree
        self.file_store = Gio.ListStore(item_type=_FileRow)
        self.tree_model = Gtk.TreeListModel.new(
            self.file_store, False, True, self._child_model
        )
        self.selection = Gtk.SingleSelection(model=self.tree_model)
        self.selection.set_autoselect(False)
        self.selection.connect("notify::selected-item", self._on_file_selected)
        factory = Gtk.SignalListItemFactory()
        factory.connect("setup", self._on_row_setup)
        factory.connect("bind", self._on_row_bind)
        self.file_list = Gtk.ListView(model=self.selection, factory=factory)
        self.file_list.add_css_class("ailens-files")
        self.file_list.set_single_click_activate(False)
        # Enter on a folder opens or closes it
        self.file_list.connect("activate", self._on_file_activated)
        files_scroller = Gtk.ScrolledWindow()
        files_scroller.set_policy(Gtk.PolicyType.NEVER, Gtk.PolicyType.AUTOMATIC)
        files_scroller.set_child(self.file_list)
        self.files_title = Gtk.Label(xalign=0)
        self.files_title.add_css_class("heading")
        self.files_title.set_margin_start(8)
        self.files_title.set_margin_top(4)
        self.files_title.set_margin_bottom(2)
        self.files_title.set_ellipsize(Pango.EllipsizeMode.END)
        files_box = Gtk.Box(orientation=Gtk.Orientation.VERTICAL)
        files_box.append(self.files_title)
        files_box.append(files_scroller)
        files_scroller.set_vexpand(True)
        self.paned.set_start_child(files_box)

        # The file comparison and its Why column
        self.filediff = FileDiff(2)
        self.filediff.set_vexpand(True)
        self.scheduler.add_scheduler(self.filediff.scheduler)
        self.why = WhyColumn()
        self.why.connect("explain-requested", self._on_explain_card)
        self.why.connect("card-hovered", self._on_card_hovered)
        self.why.connect("commit-activated", self._on_commit_activated)
        self.why.connect("build-activated", self._on_build_activated)
        self.why.bind_views(self.filediff.textview[0], self.filediff.textview[1])
        self.filediff.linediffer.connect("diffs-changed", self._on_diffs_changed)

        handle = Gtk.Separator(orientation=Gtk.Orientation.VERTICAL)
        handle.add_css_class("ailens-why-handle")
        handle.set_cursor(Gdk.Cursor.new_from_name("col-resize"))
        drag = Gtk.GestureDrag()
        drag.connect("drag-begin", self._on_handle_drag_begin)
        drag.connect("drag-update", self._on_handle_drag_update)
        handle.add_controller(drag)
        click = Gtk.GestureClick()
        click.connect("pressed", self._on_handle_pressed)
        handle.add_controller(click)
        why_title = Gtk.Label(label=_("Why"), xalign=0)
        why_title.add_css_class("heading")
        why_title.set_margin_start(8)
        self.filediff.grid.attach(handle, 10, 0, 1, 4)
        self.filediff.grid.attach(why_title, 11, 0, 1, 1)
        self.filediff.grid.attach(self.why, 11, 1, 1, 1)

        self.placeholder = Adw.StatusPage()
        self.placeholder.set_icon_name("emblem-documents-symbolic")
        self._choose_button = Gtk.Button(label=_("Choose Folder…"))
        self._choose_button.set_halign(Gtk.Align.CENTER)
        self._choose_button.add_css_class("pill")
        self._choose_button.add_css_class("suggested-action")
        self._choose_button.connect("clicked", self._on_choose_repository)
        self.content = Gtk.Stack()
        self.content.add_named(self.filediff, "diff")
        self.content.add_named(self.placeholder, "message")
        self.content.set_visible_child_name("message")
        self.paned.set_end_child(self.content)
        self.append(self.paned)

        # File navigation, next to the file comparison's own actions
        group = self.filediff.view_action_group
        for name, callback in (
            ("ailens-next-file", lambda *a: self.select_relative_file(1)),
            ("ailens-previous-file", lambda *a: self.select_relative_file(-1)),
            ("refresh", lambda *a: self.reload()),
        ):
            if group.lookup_action(name):
                group.remove_action(name)
            action = Gio.SimpleAction.new(name, None)
            action.connect("activate", callback)
            group.add_action(action)

    def _child_model(self, item: _FileRow):
        if not item.children:
            return None
        store = Gio.ListStore(item_type=_FileRow)
        for child in item.children:
            store.append(child)
        return store

    def _on_row_setup(self, _factory, list_item: Gtk.ListItem) -> None:
        expander = Gtk.TreeExpander()
        expander.set_indent_for_icon(True)
        box = Gtk.Box(orientation=Gtk.Orientation.HORIZONTAL, spacing=4)
        # Clicking a folder's name opens or closes it, not only its arrow
        click = Gtk.GestureClick()
        click.connect("released", self._on_row_clicked, list_item)
        box.add_controller(click)
        name = Gtk.Label(xalign=0, hexpand=True)
        name.set_ellipsize(Pango.EllipsizeMode.MIDDLE)
        stats = Gtk.Label()
        stats.add_css_class("dim-label")
        stats.add_css_class("caption")
        letter = Gtk.Label(width_chars=2)
        letter.add_css_class("ailens-status")
        box.append(name)
        box.append(stats)
        box.append(letter)
        expander.set_child(box)
        list_item.set_child(expander)

    def _on_row_clicked(self, _gesture, n_press, _x, _y, list_item) -> None:
        tree_row = list_item.get_item()
        if n_press == 1 and tree_row is not None and tree_row.is_expandable():
            tree_row.set_expanded(not tree_row.get_expanded())

    def _on_file_activated(self, _list, position: int) -> None:
        tree_row = self.tree_model.get_row(position)
        if tree_row is not None and tree_row.is_expandable():
            tree_row.set_expanded(not tree_row.get_expanded())

    def _on_row_bind(self, _factory, list_item: Gtk.ListItem) -> None:
        tree_row = list_item.get_item()
        row: _FileRow = tree_row.get_item()
        expander: Gtk.TreeExpander = list_item.get_child()
        expander.set_list_row(tree_row)
        name, stats, letter = list(_children(expander.get_child()))
        name.set_label(row.name)
        for css in list(letter.get_css_classes()):
            if css.startswith("ailens-status-"):
                letter.remove_css_class(css)
        if row.entry is None:
            stats.set_label("")
            letter.set_label("")
            expander.set_tooltip_text(row.name)
            return
        change = row.entry.change
        code = "U" if change.untracked else change.status
        letter.set_label(code)
        letter.add_css_class(f"ailens-status-{code}")
        if row.entry.binary:
            stats.set_label(_("binary"))
        elif row.entry.additions is not None:
            stats.set_label(f"+{row.entry.additions} −{row.entry.deletions or 0}")
        else:
            stats.set_label("")
        tooltip = f"{change.path} · {STATUS_NAMES.get(code, code)}"
        if change.original_path and change.original_path != change.path:
            tooltip += "\n" + _("from %s") % change.original_path
        expander.set_tooltip_text(tooltip)

    # -- MeldDoc --

    @property
    def toolbar_actions(self):
        return self.filediff.toolbar_actions

    def on_container_switch_in_event(self, window) -> None:
        MeldDoc.on_container_switch_in_event(self, window)
        window.insert_action_group("view", self.filediff.view_action_group)
        if self.filediff.focus_pane is None:
            self.filediff.focus_pane = self.filediff.textview[1]

    def on_file_changed(self, filename: str) -> None:
        if self.repo_info and filename.startswith(self.repo_info.root + os.sep):
            self._queue_reload()

    def get_comparison(self):
        # Recent comparisons can't express revisions; leave them out
        return None, []

    def action_stop(self, *args) -> None:
        self.worker.cancel_all()
        self._set_busy(None)

    def request_close(self) -> None:
        self._shutdown()
        MeldDoc.request_close(self)

    def do_unroot(self) -> None:
        # Closing the tab with its close button doesn't call request_close
        self._shutdown()
        Gtk.Box.do_unroot(self)

    def _shutdown(self) -> None:
        if self._closed:
            return
        self._closed = True
        if self._reload_id:
            GLib.source_remove(self._reload_id)
            self._reload_id = 0
        for monitor in self._monitors:
            monitor.cancel()
        self._monitors = []
        self.worker.shutdown()
        self.why.unbind_views()
        self.scheduler.remove_scheduler(self.filediff.scheduler)
        self._remove_temp_dir()

    def _remove_temp_dir(self) -> None:
        temp, self._temp_dir = self._temp_dir, None
        if temp and os.path.isdir(temp):
            # Revision files are read-only; make them removable first
            for folder, _dirs, files in os.walk(temp):
                for name in files:
                    try:
                        os.chmod(
                            os.path.join(folder, name), stat.S_IWUSR | stat.S_IRUSR
                        )
                    except OSError:
                        pass
            shutil.rmtree(temp, ignore_errors=True)

    def show_no_repository(self) -> None:
        self.placeholder.set_title(_("Choose a Git repository or worktree"))
        self.placeholder.set_description(
            _(
                "Then pick any branch, tag or commit, the staged or unstaged "
                "changes, or another worktree to compare."
            )
        )
        self.placeholder.set_icon_name("folder-symbolic")
        self.placeholder.set_child(self._choose_button)
        self.content.set_visible_child_name("message")

    # -- loading --

    def set_location(
        self,
        path: str,
        base: Revision | None = None,
        target: Revision | None = None,
    ) -> None:
        """Open the repository that ``path`` is in and compare base → target

        Defaults: HEAD → the working tree.
        """
        if base is not None:
            self.base = base
        if target is not None:
            self.target = target
        self.repo_label.set_label(path)
        self._show_message(_("Opening repository…"), "folder-symbolic", spinner=True)

        def done(info: RepositoryInfo) -> None:
            self.repo_info = info
            self.repo_label.set_label(info.display_name)
            self.repo_button.set_tooltip_text(
                _("%s\nClick to choose another repository") % info.root
            )
            self._set_up_monitors()
            self._load_picker_entries()
            self.reload()

        self._run(
            "open",
            lambda cancel: RepositoryInfo.open(path, self.config, cancel),
            done,
            self._show_error,
        )

    def _load_picker_entries(self) -> None:
        info = self.repo_info
        if info is None:
            return

        def done(result) -> None:
            base_entries, target_entries = result
            self.base_picker.set_entries(base_entries)
            self.target_picker.set_entries(target_entries)

        self._run(
            "picker",
            lambda cancel: (
                info.picker_entries("base", cancel),
                info.picker_entries("target", cancel),
            ),
            done,
            lambda err: self.base_picker.set_status(str(err)),
            quiet=True,
        )

    def _on_search(self, picker: RevisionPicker, query: str, side: str) -> None:
        info = self.repo_info
        if info is None:
            return
        self._run(
            f"search-{side}",
            lambda cancel: info.search_commits(query, cancel),
            lambda entries: picker.set_search_results(query, entries),
            lambda err: picker.set_status(str(err)),
            quiet=True,
        )

    def _on_picked(self, _picker, entry: PickerEntry, side: str) -> None:
        heads = self.repo_info.worktree_heads if self.repo_info else {}
        self.base, self.target = apply_pick(self.base, self.target, entry, side, heads)
        self.reload()

    def swap(self) -> None:
        if self.base.kind is RevisionKind.WORKING_TREE or (
            self.target.uncommitted and self.base.kind is RevisionKind.COMMIT
        ):
            self._show_banner(
                _("Only the target can be the working tree or the index"),
            )
            return
        if self.base.kind is RevisionKind.INDEX:
            self._show_banner(_("Staged → Working Tree can’t be swapped"))
            return
        self.base, self.target = self.target, self.base
        self.reload()

    def reload(self) -> None:
        """Resolve base and target again and reload the files, keeping the
        selected file if it is still changed"""
        info = self.repo_info
        if info is None or self._closed:
            return
        keep = self._current.change.path if self._current else None
        base, target = self.base, self.target
        self._update_picker_labels(None)
        self._set_busy(_("Comparing…"))

        def load(cancel):
            session = CompareSession.open(info, base, target, self.config, cancel)
            return session, session.files(cancel)

        def done(result) -> None:
            session, entries = result
            self.session = session
            self._update_picker_labels(session)
            self._set_entries(entries, keep)
            self._set_busy(None)
            self.banner.set_revealed(False)
            self._update_actions()

        def failed(err: Exception) -> None:
            self.session = None
            self._set_entries([], None)
            self._set_busy(None)
            self._update_actions()
            self._show_error(err)

        self._run("compare", load, done, failed)

    def _queue_reload(self) -> None:
        if self._reload_id or self._closed:
            return

        def fire() -> bool:
            self._reload_id = 0
            if self.session is not None and self.session.follows_repository_changes:
                self.reload()
            return GLib.SOURCE_REMOVE

        self._reload_id = GLib.timeout_add(RELOAD_DELAY, fire)

    def _set_up_monitors(self) -> None:
        for monitor in self._monitors:
            monitor.cancel()
        self._monitors = []
        if self.repo_info is None:
            return
        for path in self.repo_info.watch_paths():
            try:
                gfile = Gio.File.new_for_path(path)
                if os.path.isdir(path):
                    monitor = gfile.monitor_directory(Gio.FileMonitorFlags.NONE, None)
                else:
                    monitor = gfile.monitor_file(Gio.FileMonitorFlags.NONE, None)
            except GLib.Error as err:
                log.debug("Can't watch %s: %s", path, err.message)
                continue
            monitor.connect("changed", lambda *a: self._queue_reload())
            self._monitors.append(monitor)

    def _update_picker_labels(self, session: CompareSession | None) -> None:
        if session is None:
            labels = (self.base_picker, self.base), (self.target_picker, self.target)
            for picker, revision in labels:
                picker.set_label_text(_describe_revision(revision))
            self.relation_label.set_label("")
            return
        self.base_picker.set_label_text(session.base_label, session.base_tooltip)
        self.target_picker.set_label_text(session.target_label, session.target_tooltip)
        self.relation_label.set_label(session.relation_text)
        self.relation_label.set_tooltip_text(session.relation_text)
        title = f"{session.base_label} → {session.target_label}"
        self.tab_title = title
        self.tab_tooltip = GLib.markup_escape_text(
            f"{self.repo_info.root if self.repo_info else ''}\n{title}"
        )
        self.files_title.set_label(title)

    def _set_entries(self, entries: list[FileEntry], keep: str | None) -> None:
        if entries and entries == self._entries and self._current is not None:
            # Same files (e.g. something only touched the index): keep the
            # tree as it is, folders and scroll included, and refresh the
            # open file in place
            current = next((e for e in self._entries if e == self._current), None)
            if current is not None:
                self._load_file(current)
                return
        self._entries = entries
        self.file_store.remove_all()
        for row in build_file_tree(entries):
            self.file_store.append(row)
        self.tree_model.set_autoexpand(True)
        if not entries:
            self._current = None
            self._view = None
            if self.session is not None:
                self._show_message(
                    _("No changes between %(base)s and %(target)s")
                    % {
                        "base": self.session.base_label,
                        "target": self.session.target_label,
                    },
                    "object-select-symbolic",
                )
            return
        wanted = next(
            (e for e in entries if keep and e.change.path == keep), entries[0]
        )
        self._select_entry(wanted)

    def _select_entry(self, entry: FileEntry) -> None:
        for position in range(self.tree_model.get_n_items()):
            row = self.tree_model.get_row(position)
            if row is not None and row.get_item().entry is entry:
                self.selection.set_selected(position)
                # Keep the folders above the first file in view
                first_file = all(
                    self.tree_model.get_row(i).get_item().entry is None
                    for i in range(position)
                )
                self.file_list.scroll_to(
                    0 if first_file else position, Gtk.ListScrollFlags.NONE, None
                )
                return
        self._load_file(entry)

    def _ordered_entries(self) -> list[FileEntry]:
        """Files in the order the tree shows them"""

        def walk(rows: list[_FileRow]):
            for row in rows:
                if row.entry is not None:
                    yield row.entry
                yield from walk(row.children)

        return list(walk(build_file_tree(self._entries)))

    def select_relative_file(self, step: int) -> None:
        ordered = self._ordered_entries()
        if not ordered:
            return
        try:
            index = ordered.index(self._current) if self._current else -1
        except ValueError:
            index = -1
        index = max(0, min(len(ordered) - 1, index + step))
        self._select_entry(ordered[index])

    def _on_file_selected(self, selection, _pspec) -> None:
        item = selection.get_selected_item()
        if item is None:
            return
        row: _FileRow = item.get_item()
        if row.entry is not None and row.entry is not self._current:
            self._load_file(row.entry)

    def _load_file(self, entry: FileEntry) -> None:
        session = self.session
        if session is None:
            return
        self._current = entry
        self._set_busy(_("Loading %s…") % entry.change.path)

        def done(view: FileView) -> None:
            self._set_busy(None)
            if entry is not self._current:
                return
            self._show_file(view)

        def failed(err: Exception) -> None:
            self._set_busy(None)
            if entry is self._current:
                self._show_message(str(err), "dialog-error-symbolic")

        self._run(
            "file", lambda cancel: session.file_view(entry.change, cancel), done, failed
        )

    def _show_file(self, view: FileView) -> None:
        previous, self._view = self._view, view
        if (
            previous is not None
            and not view.message
            and not previous.message
            and previous.change == view.change
            and previous.base_text == view.base_text
            and previous.target_text == view.target_text
            and self.content.get_visible_child_name() == "diff"
        ):
            # Same contents: only the cards may have changed. Reloading the
            # files would lose the scroll position.
            self._show_cards()
            return
        if view.message:
            self._show_message(view.message, "dialog-information-symbolic")
            return
        try:
            paths = self._write_temp_files(view)
        except OSError as err:
            self._show_message(
                _("Couldn’t write temporary files: %s") % err.strerror,
                "dialog-error-symbolic",
            )
            return
        self.why.set_cards([], "")
        self.filediff.set_files([Gio.File.new_for_path(p) for p in paths])
        self.filediff.set_meta(
            {
                "labels": [view.base_label, view.target_label],
                "tablabel": self.tab_title,
                "parent": self,
                "prompt_resolve": False,
            }
        )
        self.content.set_visible_child_name("diff")
        self._scroll_pending = True
        self._show_cards()

    def _show_cards(self) -> None:
        view = self._view
        if view is None:
            return
        self.why.set_cards(view.cards, view.cards_placeholder)
        for key in self._busy:
            self.why.set_card_busy(key, True)

    def _write_temp_files(self, view: FileView) -> list[str]:
        if self._temp_dir is None or not os.path.isdir(self._temp_dir):
            self._temp_dir = tempfile.mkdtemp(prefix="meld-gitcompare-")
        # A fresh folder per file view: FileDiff may still be reading the
        # previous files, and both sides can have the same name
        folder = tempfile.mkdtemp(dir=self._temp_dir)
        paths = []
        for side, name, content in (
            ("base", view.base_name, view.base_text),
            ("target", view.target_name, view.target_text),
        ):
            side_dir = os.path.join(folder, side)
            os.mkdir(side_dir)
            # Keep the file's own name so its language is recognised
            path = os.path.join(side_dir, os.path.basename(name) or "file")
            with open(path, "wb") as handle:
                handle.write(content or b"")
            os.chmod(path, stat.S_IRUSR | stat.S_IRGRP)
            paths.append(path)
        return paths

    def _on_diffs_changed(self, *args) -> None:
        self.why.queue_allocate()
        if self._scroll_pending and self.filediff.linediffer.diff_count():
            self._scroll_pending = False
            # FileDiff moves the cursor to the first change while the views
            # may not be laid out yet; scroll again once they are
            GLib.timeout_add(80, self._scroll_to_first_change)

    def _scroll_to_first_change(self) -> bool:
        if not self._closed and self.filediff.linediffer.diff_count():
            chunk, _prev, next_ = self.filediff.linediffer.locate_chunk(1, 0)
            target = chunk if chunk is not None else next_
            if target is not None:
                self.filediff.go_to_chunk(target, centered=True)
        return GLib.SOURCE_REMOVE

    # -- explaining --

    def _on_explain_card(self, _why, key: str) -> None:
        session, view = self.session, self._view
        if session is None or view is None:
            return
        self._busy.add(key)
        self.why.set_card_busy(key, True)

        def done(_result) -> None:
            self._busy.discard(key)
            self._refresh_cards(view)

        def failed(err: Exception) -> None:
            self._busy.discard(key)
            self.why.set_card_error(key, str(err))

        self._run(
            f"explain-{key}",
            lambda cancel: session.explain_card(view, key, cancel),
            done,
            failed,
            quiet=True,
            exclusive=False,
        )

    def _refresh_cards(self, view: FileView) -> None:
        session = self.session
        if session is None:
            return

        def done(cards: list[Card]) -> None:
            if self._view is view:
                view.cards = cards
                self._show_cards()

        self._run(
            "cards",
            lambda cancel: session.cards(view, cancel),
            done,
            self._show_error,
            quiet=True,
        )

    def explain_all(self) -> None:
        session = self.session
        if session is None:
            return
        self._set_busy(_("Explaining changes with Claude Code…"))

        def progress(text: str) -> None:
            GLib.idle_add(self._set_busy_text, text)

        def done(count: int) -> None:
            self._set_busy(None)
            self._show_banner(
                _("Explained %d commits and changes") % count
                if count
                else _("Everything is explained already")
            )
            if self._view is not None:
                self._refresh_cards(self._view)

        def failed(err: Exception) -> None:
            self._set_busy(None)
            self._show_error(err)
            if self._view is not None:
                self._refresh_cards(self._view)

        self._run(
            "explain-all",
            lambda cancel: session.explain_all(cancel, progress),
            done,
            failed,
        )

    def generate_build(self) -> None:
        session = self.session
        if session is None:
            return
        if not session.can_build:
            self._show_banner(session.build_unavailable_reason)
            return
        self._set_busy(_("Writing the build…"))

        def progress(text: str) -> None:
            GLib.idle_add(self._set_busy_text, text)

        def done(path: str) -> None:
            self._set_busy(None)
            self._show_banner(
                _("Wrote %s") % os.path.relpath(path, session.root),
                button=_("Open"),
                action=lambda: self._open_path(path),
            )
            if self._view is not None:
                self._refresh_cards(self._view)

        def failed(err: Exception) -> None:
            self._set_busy(None)
            self._show_error(err)

        self._run(
            "build",
            lambda cancel: session.generate_build(cancel, progress),
            done,
            failed,
        )

    def setup_agents(self) -> None:
        info = self.repo_info
        if info is None:
            return

        def done(changed: list[str]) -> None:
            if changed:
                self._show_banner(_("Updated %s") % ", ".join(changed))
            else:
                self._show_banner(_("Agent instructions are already set up"))

        self._run(
            "setup",
            lambda cancel: info.set_up_agents(self.config, cancel),
            done,
            self._show_error,
        )

    def open_builds_folder(self) -> None:
        if self.repo_info is None:
            return
        folder = self.repo_info.build_folder(self.config)
        if not os.path.isdir(folder):
            self._show_banner(_("There are no builds yet"))
            return
        self._open_path(folder)

    def open_current_file(self) -> None:
        """Open the current file in a regular, editable comparison tab"""
        view, session = self._view, self.session
        if view is None or session is None:
            return
        paths = []
        for name, content, real in (
            (view.base_name, view.base_text, None),
            (view.target_name, view.target_text, view.target_disk_path),
        ):
            if real:
                paths.append(real)
                continue
            fd, path = tempfile.mkstemp(
                prefix="meld-tmp-", suffix="-" + os.path.basename(name)
            )
            with os.fdopen(fd, "wb") as handle:
                handle.write(content or b"")
            os.chmod(path, stat.S_IRUSR | stat.S_IRGRP)
            from meld.vcview import _temp_files

            _temp_files.append(path)
            paths.append(path)
        self.create_diff_signal.emit(
            [Gio.File.new_for_path(p) for p in paths],
            {
                "meta": {
                    "labels": [view.base_label, view.target_label],
                    "parent": self,
                    "prompt_resolve": False,
                }
            },
        )

    def _open_path(self, path: str) -> None:
        launcher = Gtk.FileLauncher.new(Gio.File.new_for_path(path))
        launcher.launch(self.get_root(), None, None)

    # -- cards --

    def _on_card_hovered(self, _why, key: str) -> None:
        self._hovered = key
        card = self.why.find_card(key) if key else None
        for index, side in ((0, "deleted"), (1, "added")):
            buf = self.filediff.textbuffer[index]
            tag = _hover_tag(buf)
            buf.remove_tag(tag, buf.get_start_iter(), buf.get_end_iter())
            if card is None:
                continue
            for line_range in getattr(card.card, side):
                start = buf.get_iter_at_line(line_range.start - 1)
                end = buf.get_iter_at_line(line_range.end)
                start_iter = start[1] if isinstance(start, tuple) else start
                end_iter = end[1] if isinstance(end, tuple) else end
                if line_range.end >= buf.get_line_count():
                    end_iter = buf.get_end_iter()
                buf.apply_tag(tag, start_iter, end_iter)

    def _on_commit_activated(self, _why, sha: str, button: Gtk.Widget) -> None:
        session = self.session
        if session is None or not sha:
            return

        def done(message: str) -> None:
            popover = Gtk.Popover()
            box = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=6)
            box.set_margin_top(6)
            box.set_margin_bottom(6)
            box.set_margin_start(6)
            box.set_margin_end(6)
            text = Gtk.Label(label=message, xalign=0, selectable=True, wrap=True)
            text.set_max_width_chars(72)
            scroller = Gtk.ScrolledWindow()
            scroller.set_policy(Gtk.PolicyType.NEVER, Gtk.PolicyType.AUTOMATIC)
            scroller.set_propagate_natural_height(True)
            scroller.set_max_content_height(400)
            scroller.set_child(text)
            box.append(scroller)
            compare = Gtk.Button(label=_("Compare with Parent"))

            def on_compare(*args) -> None:
                popover.popdown()
                self.base = Revision.commit(sha + "^")
                self.target = Revision.commit(sha)
                self.reload()

            compare.connect("clicked", on_compare)
            box.append(compare)
            popover.set_child(box)
            popover.set_parent(button)
            popover.connect("closed", lambda p: GLib.idle_add(p.unparent))
            popover.popup()

        self._run(
            "message",
            lambda cancel: session.commit_message(sha, cancel),
            done,
            self._show_error,
            quiet=True,
        )

    def _on_build_activated(self, _why, name: str) -> None:
        if self.repo_info is not None:
            self._open_path(
                os.path.join(self.repo_info.build_folder(self.config), name)
            )

    # -- Why column width --

    def _on_handle_drag_begin(self, _gesture, _x, _y) -> None:
        self._drag_width = self.why.get_preferred_width()

    def _on_handle_drag_update(self, _gesture, dx, _dy) -> None:
        self.why.set_preferred_width(self._drag_width - dx)

    def _on_handle_pressed(self, _gesture, n_press, _x, _y) -> None:
        if n_press == 2:
            from meld.ui.whycolumn import DEFAULT_WIDTH

            self.why.set_preferred_width(DEFAULT_WIDTH)

    # -- status --

    def _run(self, name, work, done, failed, *, quiet=False, exclusive=True) -> None:
        """Run ``work(cancel)`` on a worker thread, then ``done(result)`` or
        ``failed(error)`` on the main loop; dropped once the tab is closed or
        when a newer job of the same name started (if ``exclusive``)"""
        if self._closed:
            return

        def on_done(result) -> None:
            if not self._closed:
                done(result)

        def on_failed(err: Exception) -> None:
            if self._closed or isinstance(err, Cancelled):
                return
            if not isinstance(err, AiLensError):
                log.exception("Git Compare job %r failed", name, exc_info=err)
            failed(err)

        self.worker.submit(name, work, on_done, on_failed, exclusive=exclusive)

    def _set_busy(self, text: str | None) -> None:
        self.spinner.set_spinning(text is not None)
        self.spinner.set_tooltip_text(text or "")
        busy = text is not None
        for widget in (self.explain_button, self.build_button):
            widget.set_sensitive(not busy and self.session is not None)
        if not busy:
            self._update_actions()

    def _set_busy_text(self, text: str) -> bool:
        if self.spinner.get_spinning():
            self.spinner.set_tooltip_text(text)
            self.relation_label.set_label(text)
        return GLib.SOURCE_REMOVE

    def _update_actions(self) -> None:
        session = self.session
        has_session = session is not None
        infer = self.config.infer_enabled
        self.explain_button.set_visible(infer)
        self.explain_button.set_sensitive(has_session)
        self.build_button.set_label(
            _("Explain & Build") if infer else _("Generate Build")
        )
        self.build_button.set_sensitive(has_session and session.can_build)
        self.build_button.set_tooltip_text(
            (
                session.build_unavailable_reason
                or _("Write a build file for this comparison")
            )
            if has_session
            else _("Pick a base and a target first")
        )
        if has_session:
            self.relation_label.set_label(session.relation_text)

    def _show_message(self, text: str, icon: str, spinner: bool = False) -> None:
        self.placeholder.set_child(None)
        self.placeholder.set_description(None)
        self.placeholder.set_title(GLib.markup_escape_text(text))
        self.placeholder.set_icon_name(icon)
        self.content.set_visible_child_name("message")
        self.spinner.set_spinning(spinner)

    def _show_error(self, err: Exception) -> None:
        message = str(err) or err.__class__.__name__
        if self.session is None:
            self._show_message(message, "dialog-error-symbolic")
        self._show_banner(message)

    def _show_banner(
        self,
        text: str,
        button: str | None = None,
        action: Callable[[], None] | None = None,
    ) -> None:
        self.banner.set_title(GLib.markup_escape_text(text))
        self.banner.set_button_label(button or _("Dismiss"))
        self._banner_action = action
        self.banner.set_revealed(True)

    def _on_banner_button(self, banner: Adw.Banner) -> None:
        banner.set_revealed(False)
        action, self._banner_action = self._banner_action, None
        if action is not None:
            action()

    def _on_choose_repository(self, _button) -> None:
        dialog = Gtk.FileDialog()
        dialog.set_title(_("Choose a Git Repository or Worktree"))
        if self.repo_info is not None:
            dialog.set_initial_folder(Gio.File.new_for_path(self.repo_info.root))

        def on_done(dialog, result) -> None:
            try:
                folder = dialog.select_folder_finish(result)
            except GLib.Error:
                return
            if folder is not None and folder.get_path():
                self.session = None
                self.base = Revision.commit("HEAD")
                self.target = Revision.working_tree()
                self.set_location(folder.get_path())

        dialog.select_folder(self.get_root(), None, on_done)


def _children(widget: Gtk.Widget):
    child = widget.get_first_child()
    while child is not None:
        yield child
        child = child.get_next_sibling()


def _describe_revision(revision: Revision) -> str:
    suffix = f" ({os.path.basename(revision.worktree)})" if revision.worktree else ""
    if revision.kind is RevisionKind.INDEX:
        return _("Staged") + suffix
    if revision.kind is RevisionKind.WORKING_TREE:
        return _("Working Tree") + suffix
    return revision.ref or "HEAD"


def _hover_tag(buf: Gtk.TextBuffer) -> Gtk.TextTag:
    table = buf.get_tag_table()
    tag = table.lookup("ailens-hover")
    if tag is None:
        tag = buf.create_tag(
            "ailens-hover", paragraph_background="rgba(53,132,228,0.18)"
        )
    return tag


GitCompare.set_css_name("meld-git-compare")
