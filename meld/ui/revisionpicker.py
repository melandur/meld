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

"""A searchable button for picking a revision: a ref, commit, the index,
or a worktree's files"""

from collections.abc import Sequence

from gi.repository import GLib, GObject, Gtk, Pango

from meld.ailens.picker import PickerEntry, filter_entries
from meld.conf import _

#: Most rows the list shows at once; typing narrows it down
MAX_ROWS = 400

#: How long typing must pause before searching the whole history, in ms
SEARCH_DELAY = 300


class _EntryRow(Gtk.ListBoxRow):
    def __init__(self, entry: PickerEntry) -> None:
        super().__init__()
        self.entry = entry
        box = Gtk.Box(orientation=Gtk.Orientation.HORIZONTAL, spacing=8)
        box.set_margin_top(3)
        box.set_margin_bottom(3)
        box.set_margin_start(6)
        box.set_margin_end(6)
        box.append(Gtk.Image.new_from_icon_name(entry.icon_name))
        texts = Gtk.Box(orientation=Gtk.Orientation.VERTICAL)
        label = Gtk.Label(label=entry.label, xalign=0)
        label.set_ellipsize(Pango.EllipsizeMode.END)
        texts.append(label)
        if entry.description:
            description = Gtk.Label(label=entry.description, xalign=0)
            description.set_ellipsize(Pango.EllipsizeMode.END)
            description.add_css_class("dim-label")
            description.add_css_class("caption")
            texts.append(description)
        box.append(texts)
        self.set_child(box)
        self.set_tooltip_text(
            entry.label + ("\n" + entry.description if entry.description else "")
        )


class RevisionPicker(Gtk.MenuButton):
    """Shows the picked revision; its popover filters every known revision

    The owner fills it with :meth:`set_entries`. Typing filters them, offers
    the typed text as a ref, and (after a pause) emits ``search`` so the
    owner can look further back in history and add matches with
    :meth:`set_search_results`.
    """

    __gtype_name__ = "RevisionPicker"

    @GObject.Signal(arg_types=(object,))
    def picked(self, entry: PickerEntry) -> None: ...

    @GObject.Signal(arg_types=(str,))
    def search(self, query: str) -> None: ...

    def __init__(self, title: str = "") -> None:
        super().__init__()
        self._title = title
        self._entries: list[PickerEntry] = []
        self._search_results: list[PickerEntry] = []
        self._search_id = 0

        self._button_label = Gtk.Label(label=_("Choose…"), xalign=0)
        self._button_label.set_ellipsize(Pango.EllipsizeMode.MIDDLE)
        self._button_label.set_max_width_chars(28)
        content = Gtk.Box(orientation=Gtk.Orientation.HORIZONTAL, spacing=6)
        if title:
            heading = Gtk.Label(label=title)
            heading.add_css_class("dim-label")
            content.append(heading)
        content.append(self._button_label)
        content.append(Gtk.Image.new_from_icon_name("pan-down-symbolic"))
        self.set_child(content)

        self._search_entry = Gtk.SearchEntry()
        self._search_entry.set_placeholder_text(
            _("Branch, tag, commit message, author or SHA")
        )
        self._search_entry.connect("search-changed", self._on_search_changed)
        self._search_entry.connect("activate", self._on_search_activate)
        self._search_entry.connect("stop-search", lambda *a: self.popdown())

        self._list = Gtk.ListBox()
        self._list.set_selection_mode(Gtk.SelectionMode.BROWSE)
        self._list.set_activate_on_single_click(True)
        self._list.set_header_func(self._update_header)
        self._list.connect("row-activated", self._on_row_activated)
        self._list.set_placeholder(Gtk.Label(label=_("No matching revisions")))

        scroller = Gtk.ScrolledWindow()
        scroller.set_policy(Gtk.PolicyType.NEVER, Gtk.PolicyType.AUTOMATIC)
        scroller.set_min_content_height(320)
        scroller.set_max_content_height(520)
        scroller.set_propagate_natural_height(True)
        scroller.set_min_content_width(460)
        scroller.set_child(self._list)

        self._status = Gtk.Label(xalign=0)
        self._status.add_css_class("dim-label")
        self._status.add_css_class("caption")
        self._status.set_visible(False)

        box = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=6)
        box.append(self._search_entry)
        box.append(scroller)
        box.append(self._status)

        popover = Gtk.Popover()
        popover.set_child(box)
        popover.connect("show", self._on_popover_show)
        self.set_popover(popover)

        # Keys typed into the list go to the search entry
        self._search_entry.set_key_capture_widget(self._list)

    # -- public API --

    def set_label_text(self, text: str, tooltip: str | None = None) -> None:
        self._button_label.set_label(text)
        self.set_tooltip_text(tooltip or text)

    def set_entries(self, entries: Sequence[PickerEntry]) -> None:
        self._entries = list(entries)
        self._search_results = []
        self._refilter()

    def set_search_results(self, query: str, entries: Sequence[PickerEntry]) -> None:
        """Commits found by searching for ``query``; ignored once stale"""
        if query != self._search_entry.get_text().strip():
            return
        known = {e.revision for e in self._entries}
        self._search_results = [e for e in entries if e.revision not in known]
        self._refilter()

    def set_status(self, text: str) -> None:
        self._status.set_label(text)
        self._status.set_visible(bool(text))

    # -- internals --

    def _refilter(self) -> None:
        query = self._search_entry.get_text()
        rows = filter_entries(self._entries + self._search_results, query)
        self._list.remove_all()
        for entry in rows[:MAX_ROWS]:
            self._list.append(_EntryRow(entry))
        if len(rows) > MAX_ROWS:
            self.set_status(
                _("Showing %(shown)d of %(total)d; type to narrow down")
                % {"shown": MAX_ROWS, "total": len(rows)}
            )
        else:
            self.set_status("")
        first = self._list.get_row_at_index(0)
        if first is not None:
            self._list.select_row(first)

    def _update_header(self, row: _EntryRow, before: _EntryRow | None) -> None:
        if before is not None and before.entry.section == row.entry.section:
            row.set_header(None)
            return
        header = Gtk.Label(label=row.entry.section, xalign=0)
        header.add_css_class("heading")
        header.set_margin_top(6 if before else 0)
        header.set_margin_start(6)
        row.set_header(header)

    def _on_popover_show(self, _popover) -> None:
        self._search_entry.set_text("")
        self._refilter()
        self._search_entry.grab_focus()

    def _on_search_changed(self, entry: Gtk.SearchEntry) -> None:
        self._search_results = []
        self._refilter()
        if self._search_id:
            GLib.source_remove(self._search_id)
            self._search_id = 0
        query = entry.get_text().strip()
        if query:
            self._search_id = GLib.timeout_add(SEARCH_DELAY, self._emit_search, query)

    def _emit_search(self, query: str) -> bool:
        self._search_id = 0
        self.emit("search", query)
        return GLib.SOURCE_REMOVE

    def _on_search_activate(self, _entry) -> None:
        row = self._list.get_selected_row() or self._list.get_row_at_index(0)
        if row is not None:
            self._on_row_activated(self._list, row)

    def _on_row_activated(self, _list, row: _EntryRow) -> None:
        self.popdown()
        self.emit("picked", row.entry)

    def do_unrealize(self) -> None:
        if self._search_id:
            GLib.source_remove(self._search_id)
            self._search_id = 0
        Gtk.MenuButton.do_unrealize(self)
