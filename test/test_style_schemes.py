import shutil
import subprocess
import xml.etree.ElementTree as ET
from pathlib import Path

import pytest
from gi.repository import Gdk, Gio, GtkSource

ROOT = Path(__file__).parent.parent
DATA_DIR = ROOT / "data"
STYLES_DIR = DATA_DIR / "styles"

# Bundled schemes, with their name, variant and the metadata key and
# scheme ID of their other variant
BUNDLED_SCHEMES = {
    "meld-lens-light": ("Meld Lens Light", "light", "dark-variant", "meld-lens-dark"),
    "meld-lens-dark": ("Meld Lens Dark", "dark", "light-variant", "meld-lens-light"),
    "meld-monokai": ("Monokai (PyCharm)", "dark", "light-variant", "meld-lens-light"),
}
# Schemes whose variants point back at each other
PAIRED_SCHEMES = ("meld-lens-light", "meld-lens-dark")

# Common syntax styles that the bundled schemes should define explicitly
SYNTAX_STYLES = (
    "text",
    "selection",
    "current-line",
    "line-numbers",
    "bracket-match",
    "search-match",
    "def:comment",
    "def:doc-comment",
    "def:string",
    "def:keyword",
    "def:type",
    "def:function",
    "def:number",
    "def:constant",
    "def:preprocessor",
    "def:special-char",
    "def:operator",
    "def:identifier",
    "def:error",
    "def:note",
)


def style_names(path: Path) -> set[str]:
    return {style.get("name") for style in ET.parse(path).getroot().iter("style")}


@pytest.fixture(scope="module")
def style_manager(tmp_path_factory):
    """Style scheme manager that loads freshly generated scheme files"""

    styles = tmp_path_factory.mktemp("styles")
    # This mirrors bin/meld's handling for uninstalled use
    for source in STYLES_DIR.glob("*.style-scheme.xml.in"):
        shutil.copyfile(source, styles / source.name.removesuffix(".in"))

    manager = GtkSource.StyleSchemeManager.get_default()
    manager.prepend_search_path(str(styles))
    manager.force_rescan()
    return manager


@pytest.mark.parametrize("scheme_id", BUNDLED_SCHEMES)
def test_scheme_loads(style_manager, scheme_id):
    name, variant, other_key, other_id = BUNDLED_SCHEMES[scheme_id]

    scheme = style_manager.get_scheme(scheme_id)
    assert scheme is not None
    assert scheme.get_name() == name
    assert scheme_id in style_manager.get_scheme_ids()

    # Variant metadata lets adapt_style_scheme() switch with dark mode
    assert scheme.get_metadata("variant") == variant
    assert scheme.get_metadata(other_key) == other_id
    other = style_manager.get_scheme(other_id)
    assert other is not None
    if scheme_id in PAIRED_SCHEMES:
        assert other.get_metadata(f"{variant}-variant") == scheme_id


@pytest.mark.parametrize("scheme_id", BUNDLED_SCHEMES)
def test_scheme_defines_meld_styles(style_manager, scheme_id):
    scheme = style_manager.get_scheme(scheme_id)
    base_styles = style_names(STYLES_DIR / "meld-base.style-scheme.xml.in")
    scheme_styles = style_names(STYLES_DIR / f"{scheme_id}.style-scheme.xml.in")

    meld_styles = {name for name in base_styles if name.startswith("meld:")}
    assert meld_styles
    assert meld_styles <= scheme_styles
    assert "map-overlay" in scheme_styles

    # Every colour attribute that meld-base sets must resolve to a
    # parseable colour, since meld.style looks these up directly.
    base = style_manager.get_scheme("meld-base")
    for name in meld_styles:
        base_style = base.get_style(name)
        style = scheme.get_style(name)
        assert style is not None, name
        for attribute in ("foreground", "background", "line_background"):
            if not getattr(base_style.props, attribute):
                continue
            value = getattr(style.props, attribute)
            assert value, f"{scheme_id} {name} is missing {attribute}"
            assert Gdk.RGBA().parse(value), f"{scheme_id} {name} {attribute}"


@pytest.mark.parametrize("scheme_id", BUNDLED_SCHEMES)
def test_scheme_defines_syntax_styles(style_manager, scheme_id):
    scheme = style_manager.get_scheme(scheme_id)
    for name in SYNTAX_STYLES:
        assert scheme.get_style(name) is not None, name


@pytest.mark.parametrize("scheme_id", BUNDLED_SCHEMES)
def test_scheme_diff_colours(style_manager, scheme_id):
    scheme = style_manager.get_scheme(scheme_id)

    def colour(name, attribute="background"):
        rgba = Gdk.RGBA()
        rgba.parse(getattr(scheme.get_style(name).props, attribute))
        return rgba

    # GitHub-like: inserts are green and deletes are red
    insert = colour("meld:insert", "line_background")
    assert insert.green > insert.red and insert.green > insert.blue
    delete = colour("meld:delete", "line_background")
    assert delete.red > delete.green and delete.red > delete.blue


@pytest.mark.parametrize("scheme_id", BUNDLED_SCHEMES)
def test_scheme_dark_detection(style_manager, scheme_id):
    from meld.style import should_use_dark

    scheme = style_manager.get_scheme(scheme_id)
    assert should_use_dark(scheme) == (BUNDLED_SCHEMES[scheme_id][1] == "dark")


# Colours from the PyCharm-monokai theme, as (foreground, bold, italic)
MONOKAI_STYLES = {
    "text": ("#ffffff", False, False),
    "def:keyword": ("#66d9ef", True, False),
    "def:builtin": ("#66d9ef", False, False),
    "def:comment": ("#75715e", False, False),
    "def:doc-comment": ("#e6db74", False, True),
    "def:doc-comment-element": ("#e6db74", True, True),
    "def:string": ("#e6db74", False, False),
    "def:special-char": ("#ae81ff", False, False),
    "def:number": ("#ae81ff", False, False),
    "def:operator": ("#f92672", False, False),
    "def:function": ("#a6e22e", False, False),
    "python:keyword": ("#66d9ef", True, False),
    "python:builtin-function": ("#66d9ef", False, False),
    "python:function-name": ("#a6e22e", True, False),
    "python:class-name": ("#ffffff", True, False),
    "python:decorator": ("#a6e22e", False, False),
    "python:special-variable": ("#fd971f", False, False),
    "python:multiline-string": ("#e6db74", False, True),
    "python:escaped-char": ("#ae81ff", False, False),
}


@pytest.mark.parametrize("style_id", MONOKAI_STYLES)
def test_monokai_style_colours(style_manager, style_id):
    foreground, bold, italic = MONOKAI_STYLES[style_id]
    style = style_manager.get_scheme("meld-monokai").get_style(style_id)

    assert style.props.foreground == foreground
    assert bool(style.props.bold_set and style.props.bold) == bold
    assert bool(style.props.italic_set and style.props.italic) == italic


@pytest.mark.parametrize(
    "style_id, attribute, colour",
    [
        ("text", "background", "#2b2b2b"),
        ("selection", "background", "#214283"),
        ("current-line", "background", "#323232"),
        ("line-numbers", "foreground", "#606366"),
        ("line-numbers", "background", "#313335"),
        ("bracket-match", "background", "#344134"),
        ("search-match", "background", "#40332b"),
        ("def:error", "underline_color", "#ff0000"),
    ],
)
def test_monokai_editor_colours(style_manager, style_id, attribute, colour):
    style = style_manager.get_scheme("meld-monokai").get_style(style_id)
    assert getattr(style.props, attribute) == colour


def highlighted_colour(scheme, language_id, text, needle):
    """Get the foreground colour that highlighting gives a piece of text

    This runs GtkSourceView's real highlighting engine on a buffer, so
    it covers each language's fallback chain through def.lang.
    """

    language = GtkSource.LanguageManager.get_default().get_language(language_id)
    assert language is not None, language_id
    buf = GtkSource.Buffer(language=language, style_scheme=scheme)
    buf.set_highlight_syntax(True)
    buf.set_text(text)
    buf.ensure_highlight(buf.get_start_iter(), buf.get_end_iter())

    it = buf.get_iter_at_offset(text.index(needle))
    # Tags are ordered by priority, so the last one with a colour wins
    colours = [t.props.foreground_rgba for t in it.get_tags() if t.props.foreground_set]
    if not colours:
        return None
    rgba = colours[-1]
    return "#{:02x}{:02x}{:02x}".format(
        *(round(c * 255) for c in (rgba.red, rgba.green, rgba.blue))
    )


@pytest.mark.parametrize(
    "language_id, text, needle, expected",
    [
        ("python3", "def f(self):\n    return 1  # c\n", "return", "#66d9ef"),
        ("python3", "def f(self):\n    return 1  # c\n", "f(", "#a6e22e"),
        ("python3", "def f(self):\n    return 1  # c\n", "self", "#fd971f"),
        ("python3", "def f(self):\n    return 1  # c\n", "1", "#ae81ff"),
        ("python3", "def f(self):\n    return 1  # c\n", "# c", "#75715e"),
        ("python3", "x = 'a\\n' + y\n", "'a", "#e6db74"),
        ("python3", "x = 'a\\n' + y\n", "\\n", "#ae81ff"),
        ("python3", "x = 'a\\n' + y\n", "+", "#f92672"),
        ("python3", "@decorator\nclass A:\n    pass\n", "@decorator", "#a6e22e"),
        ("markdown", "# Title\n\nSome `code` here\n", "Title", "#a6e22e"),
        ("markdown", "# Title\n\nSome `code` here\n", "`code`", "#e6db74"),
        ("yaml", "key: 'value'\n# comment\n", "key", "#f92672"),
        ("yaml", "key: 'value'\n# comment\n", "'value'", "#e6db74"),
        ("yaml", "key: 'value'\n# comment\n", "# comment", "#75715e"),
        ("json", '{"key": "value", "n": 1}\n', '"key"', "#f92672"),
        ("json", '{"key": "value", "n": 1}\n', '"value"', "#e6db74"),
        ("json", '{"key": "value", "n": 1}\n', "1", "#ae81ff"),
        ("js", "// c\nfunction f() { return 's'; }\n", "return", "#66d9ef"),
        ("js", "// c\nfunction f() { return 's'; }\n", "'s'", "#e6db74"),
        ("js", "// c\nfunction f() { return 's'; }\n", "// c", "#75715e"),
        ("c", "#include <stdio.h>\n/* c */\nint x = 0;\n", "#include", "#f92672"),
        ("c", "#include <stdio.h>\n/* c */\nint x = 0;\n", "/* c */", "#75715e"),
        ("c", "#include <stdio.h>\n/* c */\nint x = 0;\n", "int", "#66d9ef"),
        ("c", "#include <stdio.h>\n/* c */\nint x = 0;\n", "0", "#ae81ff"),
        ("sh", 'if true; then\n  echo "$HOME"  # c\nfi\n', "if", "#66d9ef"),
        ("sh", 'if true; then\n  echo "$HOME"  # c\nfi\n', "$HOME", "#fd971f"),
        ("sh", 'if true; then\n  echo "$HOME"  # c\nfi\n', "# c", "#75715e"),
        ("xml", '<root attr="value"/>\n', "root", "#f92672"),
        ("xml", '<root attr="value"/>\n', "attr", "#a6e22e"),
        ("xml", '<root attr="value"/>\n', '"value"', "#e6db74"),
        ("css", "p { color: red; }\n", "color", "#66d9ef"),
        ("diff", "--- a\n+++ b\n@@ -1 +1 @@\n-old\n+new\n", "-old", "#f92672"),
        ("diff", "--- a\n+++ b\n@@ -1 +1 @@\n-old\n+new\n", "+new", "#a6e22e"),
    ],
)
def test_monokai_highlighting(style_manager, language_id, text, needle, expected):
    scheme = style_manager.get_scheme("meld-monokai")
    assert highlighted_colour(scheme, language_id, text, needle) == expected


def test_schemes_are_installed_and_copied():
    meson = (DATA_DIR / "meson.build").read_text()
    launcher = (ROOT / "bin" / "meld").read_text()
    for scheme_id in BUNDLED_SCHEMES:
        assert (STYLES_DIR / f"{scheme_id}.style-scheme.xml.in").exists()
        assert f"'{scheme_id}.style-scheme.xml'" in meson
        assert f'"{scheme_id}"' in launcher


def test_settings_schema_compiles():
    result = subprocess.run(
        ["glib-compile-schemas", "--strict", "--dry-run", str(DATA_DIR)],
        check=False,
        capture_output=True,
        text=True,
    )
    assert result.returncode == 0, result.stderr


def test_settings_schema_has_diff_colors(tmp_path):
    shutil.copy(DATA_DIR / "org.gnome.Meld.gschema.xml", tmp_path)
    subprocess.run(["glib-compile-schemas", str(tmp_path)], check=True)

    source = Gio.SettingsSchemaSource.new_from_directory(str(tmp_path), None, False)
    schema = source.lookup("org.gnome.meld", False)
    assert schema.has_key("diff-colors")

    key = schema.get_key("diff-colors")
    assert key.get_default_value().get_string() == "scheme"
    range_type, values = key.get_range().unpack()
    assert range_type == "enum"
    assert values == ["scheme", "red-green", "colorblind"]
