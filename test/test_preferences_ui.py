import xml.etree.ElementTree as ET
from pathlib import Path

import pytest

UI = Path(__file__).parent.parent / "meld" / "resources" / "ui" / "preferences.ui"
SCHEMA = Path(__file__).parent.parent / "data" / "org.gnome.Meld.gschema.xml"


def ui_ids():
    return {el.get("id") for el in ET.parse(UI).iter("object") if el.get("id")}


def test_every_template_child_is_in_the_ui():
    from gi.repository import Gtk

    from meld.preferences import PreferencesDialog

    children = {
        name
        for name, value in vars(PreferencesDialog).items()
        if isinstance(value, Gtk.Template.Child)
    }
    assert children
    assert children <= ui_ids()


@pytest.mark.parametrize(
    "key",
    [
        "ailens-file-row-spacing",
        "ailens-file-list-width",
        "ailens-infer-enabled",
        "ailens-claude-path",
        "ailens-infer-model",
        "ailens-build-model",
        "ailens-build-folder",
        "ailens-notes-ref",
        "ailens-author-pattern",
    ],
)
def test_git_compare_keys_exist_and_are_bound(key):
    keys = {
        el.get("name")
        for schema in ET.parse(SCHEMA).iter("schema")
        if schema.get("id") == "org.gnome.meld"
        for el in schema.iter("key")
    }
    assert key in keys
    source = (Path(__file__).parent.parent / "meld" / "preferences.py").read_text()
    assert f'"{key}"' in source
