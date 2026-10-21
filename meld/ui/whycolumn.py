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

"""The Why column: a card per change, level with the change's lines"""

import datetime
import logging

from gi.repository import GLib, GObject, Graphene, Gsk, Gtk, Pango

from meld.ailens.cards import Card, layout_cards, reflow
from meld.ailens.picker import parse_iso_date, relative_date
from meld.conf import _

log = logging.getLogger(__name__)

#: Number of commit colors defined in style.css (.ailens-color-N)
COLOR_COUNT = 8

DEFAULT_WIDTH = 300
MIN_WIDTH = 160
MAX_WIDTH = 900


def _label(text: str, *classes: str, wrap: bool = True, xalign: float = 0) -> Gtk.Label:
    label = Gtk.Label(label=text, xalign=xalign, wrap=wrap, selectable=False)
    if wrap:
        label.set_wrap_mode(Pango.WrapMode.WORD_CHAR)
        label.set_natural_wrap_mode(Gtk.NaturalWrapMode.WORD)
    else:
        # Anything that doesn't wrap must still shrink to the column width
        label.set_ellipsize(Pango.EllipsizeMode.END)
        label.set_width_chars(1)
    for css_class in classes:
        label.add_css_class(css_class)
    return label


def _translate_y(source: Gtk.Widget, dest: Gtk.Widget) -> float | None:
    """Where ``source``'s top is in ``dest``; None if they aren't related

    PyGObject versions disagree on whether the success flag is returned.
    """
    result = source.translate_coordinates(dest, 0, 0)
    if result is None:
        return None
    if len(result) == 3:
        ok, _x, y = result
        return y if ok else None
    return result[1]


def _place(widget: Gtk.Widget, width: int, height: float, top: float) -> None:
    transform = Gsk.Transform().translate(Graphene.Point().init(0, top))
    widget.allocate(int(width), int(max(height, 0)), -1, transform)


class WhyCard(Gtk.Box):
    """One change: its commit, author or agent, and What and Why"""

    __gtype_name__ = "WhyCard"

    def __init__(self, card: Card, column: "WhyColumn") -> None:
        super().__init__(orientation=Gtk.Orientation.VERTICAL, spacing=2)
        self.card = card
        self._column = column
        self.add_css_class("ailens-card")
        if card.kind == "commit" and card.color_index >= 0:
            self.add_css_class(f"ailens-color-{card.color_index % COLOR_COUNT}")
        else:
            self.add_css_class(f"ailens-{card.kind}")
        self.set_focusable(False)

        header = Gtk.Box(orientation=Gtk.Orientation.HORIZONTAL, spacing=6)
        header.add_css_class("ailens-card-header")
        if card.kind == "commit" and card.sha:
            sha = Gtk.Button(label=card.short_sha)
            sha.add_css_class("flat")
            sha.add_css_class("ailens-sha")
            sha.set_tooltip_text(_("Show the full commit message"))
            sha.connect("clicked", self._on_sha_clicked)
            header.append(sha)
        else:
            header.append(
                _label(
                    {
                        "staged": _("Staged"),
                        "unstaged": _("Unstaged"),
                    }.get(card.kind, _("Commit unknown")),
                    "ailens-layer",
                    wrap=False,
                )
            )

        if card.agent:
            badge = _label(_("Claude"), "ailens-badge", "ailens-agent", wrap=False)
            badge.set_tooltip_text(card.agent_name)
            header.append(badge)
            if card.agent_model:
                header.append(_label(card.agent_model, "dim-label", wrap=False))
        elif card.author:
            author = _label(card.author, "dim-label", wrap=False)
            author.set_ellipsize(Pango.EllipsizeMode.END)
            header.append(author)

        date = parse_iso_date(card.date) if card.date else None
        if date is not None:
            when = _label(
                relative_date(date, datetime.datetime.now(datetime.timezone.utc)),
                "dim-label",
                wrap=False,
                xalign=1,
            )
            when.set_hexpand(True)
            when.set_tooltip_text(date.astimezone().strftime("%c"))
            header.append(when)
        self.append(header)

        if card.what:
            self.append(_label(reflow(card.what), "ailens-what"))
        if card.why:
            if card.source == "inferred":
                self.append(
                    _label(
                        _("Inferred by %s, not the author’s words")
                        % (card.inferred_by or _("a model")),
                        "ailens-badge",
                        "ailens-inferred",
                        wrap=False,
                    )
                )
            elif card.source:
                self.append(
                    _label(
                        {"commit": _("from the commit"), "note": _("from a note")}.get(
                            card.source, card.source
                        ),
                        "ailens-source",
                        "dim-label",
                        wrap=False,
                    )
                )
            why = _label(reflow(card.why), "ailens-why")
            why.set_selectable(True)
            self.append(why)

        if card.change_what or card.change_why:
            change = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=2)
            change.add_css_class("ailens-change")
            change.append(
                _label(
                    _("This change · inferred by %s")
                    % (card.change_model or _("a model")),
                    "ailens-badge",
                    "ailens-inferred",
                    wrap=False,
                )
            )
            if card.change_what:
                change.append(_label(card.change_what, "ailens-what"))
            if card.change_why:
                why = _label(reflow(card.change_why), "ailens-why")
                why.set_selectable(True)
                change.append(why)
            self.append(change)

        self._explain_box = Gtk.Box(orientation=Gtk.Orientation.HORIZONTAL, spacing=6)
        self._spinner = Gtk.Spinner(visible=False)
        self._explain_button = None
        if card.explain is not None:
            label = {
                "commit": _("Explain with Claude"),
                "changes": _("Explain changes"),
                "uncommitted": _("Explain with Claude"),
            }[card.explain]
            self._explain_button = Gtk.Button(label=label)
            self._explain_button.add_css_class("ailens-explain")
            self._explain_button.connect("clicked", self._on_explain_clicked)
            self._explain_box.append(self._explain_button)
        self._explain_box.append(self._spinner)
        self._error = _label("", "error")
        self._error.set_visible(False)
        self.append(self._explain_box)
        self.append(self._error)
        self._explain_box.set_visible(card.explain is not None)

        for build in card.builds:
            link = Gtk.LinkButton(label=_("Build %s") % build.removesuffix(".md"))
            link.set_halign(Gtk.Align.START)
            link.add_css_class("ailens-build")
            link.connect("activate-link", self._on_build_link, build)
            self.append(link)

        motion = Gtk.EventControllerMotion()
        motion.connect("enter", lambda *a: column.emit("card-hovered", card.key))
        motion.connect("leave", lambda *a: column.emit("card-hovered", ""))
        self.add_controller(motion)

    def set_busy(self, busy: bool) -> None:
        self._spinner.set_visible(busy)
        self._spinner.set_spinning(busy)
        if self._explain_button is not None:
            self._explain_button.set_sensitive(not busy)
        if busy:
            self.set_error("")

    def set_error(self, message: str) -> None:
        self._error.set_label(message)
        self._error.set_visible(bool(message))

    def _on_sha_clicked(self, button: Gtk.Button) -> None:
        self._column.emit("commit-activated", self.card.sha or "", button)

    def _on_explain_clicked(self, _button: Gtk.Button) -> None:
        self._column.emit("explain-requested", self.card.key)

    def _on_build_link(self, _link: Gtk.LinkButton, build: str) -> bool:
        self._column.emit("build-activated", build)
        return True


class WhyColumn(Gtk.Widget):
    """Lays its cards out level with their lines in two linked text views

    Added lines are found in the target view, deleted lines in the base view.
    A card is pushed down only to clear the one above it, and the column is
    re-laid out whenever either view scrolls or changes size.
    """

    __gtype_name__ = "WhyColumn"

    @GObject.Signal(arg_types=(str,))
    def explain_requested(self, key: str) -> None: ...

    @GObject.Signal(arg_types=(str,))
    def card_hovered(self, key: str) -> None: ...

    @GObject.Signal(arg_types=(str, Gtk.Widget))
    def commit_activated(self, sha: str, button: Gtk.Widget) -> None: ...

    @GObject.Signal(arg_types=(str,))
    def build_activated(self, name: str) -> None: ...

    def __init__(self) -> None:
        super().__init__()
        self.set_overflow(Gtk.Overflow.HIDDEN)
        self.add_css_class("ailens-why-column")
        self.set_hexpand(False)
        self.set_vexpand(True)
        self._width = DEFAULT_WIDTH
        self._cards: list[WhyCard] = []
        self._views: dict[str, Gtk.TextView] = {}
        self._handlers: list[tuple[GObject.Object, int]] = []
        self._placeholder = _label("", "dim-label", xalign=0.5)
        self._placeholder.set_parent(self)
        self._placeholder.set_margin_top(24)
        self._placeholder.set_margin_start(12)
        self._placeholder.set_margin_end(12)
        self._relayout_id = 0

    # -- public API --

    @property
    def cards(self) -> list[WhyCard]:
        return list(self._cards)

    def get_preferred_width(self) -> int:
        return self._width

    def set_preferred_width(self, width: int) -> None:
        width = max(MIN_WIDTH, min(MAX_WIDTH, int(width)))
        if width != self._width:
            self._width = width
            self.queue_resize()

    def bind_views(self, base: Gtk.TextView, target: Gtk.TextView) -> None:
        """Follow the base and target views; re-binding replaces them"""
        self.unbind_views()
        self._views = {"old": base, "new": target}
        for view in (base, target):
            for adjustment in (view.get_vadjustment(),):
                if adjustment is None:
                    continue
                for signal in ("value-changed", "changed"):
                    handler = adjustment.connect(signal, self._queue_relayout)
                    self._handlers.append((adjustment, handler))
            handler = view.connect("notify::vadjustment", self._on_vadjustment)
            self._handlers.append((view, handler))
        self.queue_allocate()

    def unbind_views(self) -> None:
        for obj, handler in self._handlers:
            if obj.handler_is_connected(handler):
                obj.disconnect(handler)
        self._handlers = []
        self._views = {}

    def set_cards(self, cards: list[Card], placeholder: str = "") -> None:
        for card in self._cards:
            card.unparent()
        self._cards = [WhyCard(card, self) for card in cards]
        for card in self._cards:
            card.set_parent(self)
        self._placeholder.set_label(placeholder)
        self._placeholder.set_visible(bool(placeholder) and not cards)
        self.queue_resize()

    def find_card(self, key: str) -> WhyCard | None:
        return next((c for c in self._cards if c.card.key == key), None)

    def set_card_busy(self, key: str, busy: bool) -> None:
        if card := self.find_card(key):
            card.set_busy(busy)

    def set_card_error(self, key: str, message: str) -> None:
        if card := self.find_card(key):
            card.set_busy(False)
            card.set_error(message)

    # -- layout --

    def _on_vadjustment(self, view: Gtk.TextView, _pspec) -> None:
        # The view's scrolled window swapped adjustments; follow the new one
        base, target = self._views.get("old"), self._views.get("new")
        if base is not None and target is not None:
            GLib.idle_add(lambda: self.bind_views(base, target) and False)

    def _queue_relayout(self, *args) -> None:
        self.queue_allocate()

    def _line_top(self, side: str, line: int) -> float | None:
        view = self._views.get(side)
        if view is None or view.get_root() is None:
            return None
        buf = view.get_buffer()
        line = max(0, min(line - 1, buf.get_line_count()))
        if hasattr(view, "get_y_for_line_num"):
            y = view.get_y_for_line_num(line)
        else:
            _found, it = buf.get_iter_at_line(line)
            y, _height = view.get_line_yrange(it)
        visible = view.get_visible_rect()
        offset = _translate_y(view, self)
        if offset is None:
            return None
        return y - visible.y + offset

    def _card_tops(self) -> list[float]:
        tops = []
        last = 0.0
        for card in self._cards:
            side, line = card.card.anchor
            top = self._line_top(side, line)
            if top is None:
                top = last
            tops.append(top)
            last = top
        return tops

    def do_measure(self, orientation, for_size):
        if orientation == Gtk.Orientation.HORIZONTAL:
            return (MIN_WIDTH, self._width, -1, -1)
        # Never ask for height: the cards scroll with the views, so the
        # column takes whatever the grid row gives it
        return (0, 0, -1, -1)

    def do_size_allocate(self, width: int, height: int, baseline: int) -> None:
        heights = []
        for card in self._cards:
            _min, natural, _b1, _b2 = card.measure(Gtk.Orientation.VERTICAL, width)
            heights.append(natural)
        wanted = self._card_tops()
        # Lay out top to bottom: cards anchored in the base pane (deletions)
        # and the target pane can interleave
        order = sorted(range(len(self._cards)), key=lambda i: (wanted[i], i))
        placed = layout_cards([wanted[i] for i in order], [heights[i] for i in order])
        tops = [0.0] * len(self._cards)
        for index, top in zip(order, placed, strict=True):
            tops[index] = top
        for card, top, card_height in zip(self._cards, tops, heights, strict=True):
            # Off-screen cards are still allocated (and clipped) so that
            # focus and hover keep working while scrolling
            _place(card, width, card_height, top)
        if self._placeholder.get_visible():
            _min, natural, _b1, _b2 = self._placeholder.measure(
                Gtk.Orientation.VERTICAL, width
            )
            _place(self._placeholder, width, natural, 0)

    def do_dispose(self) -> None:
        self.unbind_views()
        for card in self._cards:
            card.unparent()
        self._cards = []
        if self._placeholder is not None:
            self._placeholder.unparent()
            self._placeholder = None
        Gtk.Widget.do_dispose(self)


WhyColumn.set_css_name("ailens-why-column")
