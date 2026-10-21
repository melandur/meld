import pytest
from gi.repository import Gio, Gtk, GtkSource

from meld.languageguess import (
    choose_pane_languages,
    guess_language_from_text,
    interpreter_from_shebang,
    language_id_from_text,
    language_name_from_modeline,
    normalise_language_name,
)

MANAGER = GtkSource.LanguageManager()
KNOWN_IDS = set(MANAGER.get_language_ids())


@pytest.mark.parametrize(
    "line, expected",
    [
        ("#!/bin/bash", "bash"),
        ("#!/bin/sh -e", "sh"),
        ("#! /usr/bin/perl -w", "perl"),
        ("#!/usr/bin/env python3", "python3"),
        ("#!/usr/bin/env python3.12", "python3.12"),
        ("#!/usr/bin/env -S python3 -u", "python3"),
        ("#!/usr/bin/env -S LANG=C ruby --disable-gems", "ruby"),
        ("#!/usr/bin/env -u PYTHONPATH python3", "python3"),
        ("#!/usr/local/bin/node", "node"),
        ("#!/usr/bin/env", None),
        ("#!", None),
        ("# just a comment", None),
        ("", None),
    ],
)
def test_interpreter_from_shebang(line, expected):
    assert interpreter_from_shebang(line) == expected


@pytest.mark.parametrize(
    "name, expected",
    [
        ("python", "python3"),
        ("Python", "python3"),
        ("python3", "python3"),
        ("python3.12", "python3"),
        ("python2", "python"),
        ("python2.7", "python"),
        ("bash", "sh"),
        ("zsh", "sh"),
        ("sh", "sh"),
        ("node", "js"),
        ("javascript", "js"),
        ("perl", "perl"),
        ("perl5", "perl"),
        ("lua5.4", "lua"),
        ("c++", "cpp"),
        ("C++", "cpp"),
        ("python-mode", "python3"),
        ("rust", "rust"),
        ("yml", "yaml"),
        ("make", "makefile"),
        ("fundamental", None),
        ("", None),
        ("not-a-language", None),
    ],
)
def test_normalise_language_name(name, expected):
    assert normalise_language_name(name, KNOWN_IDS) == expected


@pytest.mark.parametrize(
    "text, expected",
    [
        # Vim modelines, at the start or end of a file
        ("# vim: ft=python\nx = 1\n", "python"),
        ("# vim: set filetype=sh :\n", "sh"),
        ("/* vim:ft=c ts=4 sw=4 */\n", "c"),
        ("x\n" * 20 + "# vi: set ts=4 syntax=ruby:\n", "ruby"),
        ("<!-- vim: set noet ft=xml: -->\n", "xml"),
        # Not modelines
        ("x\n" * 6 + "# vim: ft=python\n" + "x\n" * 20, None),
        ("# environment: ft=python\n", None),
        ("# vim: noft=python\n", None),
        # Emacs modelines
        ("# -*- mode: python; coding: utf-8 -*-\n", "python"),
        ("#!/bin/sh\n# -*- mode: perl -*-\n", "perl"),
        ("/* -*- C++ -*- */\n", "C++"),
        ("# -*- coding: utf-8 -*-\n", None),
        ("x\n" * 10 + "# Local Variables:\n# mode: lua\n# End:\n", "lua"),
        ("plain text\n", None),
        ("", None),
    ],
)
def test_language_name_from_modeline(text, expected):
    assert language_name_from_modeline(text) == expected


@pytest.mark.parametrize(
    "text, expected",
    [
        ("#!/usr/bin/env python3\nprint('hi')\n", "python3"),
        ("#!/usr/bin/env python\nprint('hi')\n", "python3"),
        ("#!/usr/bin/python2.7\nprint 'hi'\n", "python"),
        ("#!/bin/bash\necho hi\n", "sh"),
        ("#!/bin/zsh\necho hi\n", "sh"),
        ("#!/usr/bin/env -S node --experimental\n", "js"),
        ("#!/usr/bin/perl -w\n", "perl"),
        # Modelines win over shebangs
        ("#!/bin/sh\n# vim: ft=python\n", "python3"),
        # Unknown modeline names fall back to the shebang
        ("#!/bin/sh\n# vim: ft=nonsense\n", "sh"),
        ("#!/usr/bin/unknown-interpreter\n", None),
        ("Just some text\n", None),
        ("", None),
    ],
)
def test_language_id_from_text(text, expected):
    assert language_id_from_text(text, KNOWN_IDS) == expected


@pytest.mark.parametrize(
    "text, expected",
    [
        ("#!/usr/bin/env python3\nprint('hi')\n", "python3"),
        ("#!/bin/bash\necho hi\n", "sh"),
        ("# vim: ft=rust\nfn main() {}\n", "rust"),
        # Content sniffing by GIO
        ('<?xml version="1.0" encoding="UTF-8"?>\n<root/>\n', "xml"),
        ("Just some text\nwith no language\n", None),
        ("", None),
    ],
)
def test_guess_language_from_text(text, expected):
    lang = guess_language_from_text(text, MANAGER)
    assert (lang.get_id() if lang else None) == expected


@pytest.mark.parametrize(
    "file_languages, text_languages, expected",
    [
        # Extensions agree
        (["c", "c"], [None, None], ["c", "c"]),
        # One known extension applies to all panes, even if the text
        # suggests otherwise
        (["python3", None], [None, "sh"], ["python3", "python3"]),
        ([None, "python3", None], [None, None, None], ["python3"] * 3),
        # Conflicting extensions are kept per pane
        (["c", "chdr"], [None, None], ["c", "chdr"]),
        # ... with unknown panes guessed from their text
        (["c", "chdr", None], [None, None, "cpp"], ["c", "chdr", "cpp"]),
        (["c", "chdr", None], [None, None, None], ["c", "chdr", None]),
        # No extensions, so we guess from text
        ([None, None], ["sh", None], ["sh", "sh"]),
        ([None, None], ["sh", "sh"], ["sh", "sh"]),
        ([None, None], ["sh", "perl"], ["sh", "perl"]),
        ([None, None], [None, None], [None, None]),
        ([None], ["ruby"], ["ruby"]),
        ([], [], []),
    ],
)
def test_choose_pane_languages(file_languages, text_languages, expected):
    calls = []

    def guess(pane):
        calls.append(pane)
        return text_languages[pane]

    assert choose_pane_languages(file_languages, guess) == expected
    # We only guess from text for panes that need it
    for pane in calls:
        assert file_languages[pane] is None


def test_choose_pane_languages_is_lazy():
    def guess(pane):
        raise AssertionError("Text guessing shouldn't be needed")

    assert choose_pane_languages(["js", None], guess) == ["js", "js"]


def test_language_from_file_name_is_unchanged():
    from meld.sourceview import LanguageManager

    gfile = Gio.File.new_for_path("/nonexistent/meld-test/example.py")
    # Missing files give no language, as before
    assert LanguageManager.get_language_from_file(gfile) is None
    assert LanguageManager.get_language_from_file(None) is None
    lang = LanguageManager.get_language_from_text("#!/bin/bash\n")
    assert lang.get_id() == "sh"


@pytest.mark.parametrize(
    "text",
    [
        "",
        "#!/bin/sh\necho\n",
        "# vim: ft=python\n" + "x = 1\n" * 1000 + "# vim: ft=ruby\n",
        "# vim: ft=python\n" + "x = 1\n" * 1000 + "# vim: ft=ruby",
        "line\n" * 50,
        "no newline at end",
    ],
    ids=["empty", "short", "long", "long-no-final-newline", "medium", "one-line"],
)
def test_language_sample_text(text):
    from meld.filediff import (
        LANGUAGE_SAMPLE_HEAD_LINES,
        LANGUAGE_SAMPLE_TAIL_LINES,
        get_language_sample_text,
    )

    buf = Gtk.TextBuffer()
    buf.set_text(text)
    sample = get_language_sample_text(buf)

    lines = text.splitlines()
    limit = LANGUAGE_SAMPLE_HEAD_LINES + LANGUAGE_SAMPLE_TAIL_LINES
    if len(lines) <= limit:
        assert sample == text
    else:
        sample_lines = sample.splitlines()
        assert sample_lines[:5] == lines[:5]
        assert sample_lines[-5:] == lines[-5:]
        assert len(sample_lines) <= limit
        # Modelines at either end are still found
        assert language_name_from_modeline(sample) == "python"
        assert language_name_from_modeline(sample.split("\n", 1)[1]) == "ruby"
