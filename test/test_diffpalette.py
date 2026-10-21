import itertools
import shutil
from pathlib import Path
from types import SimpleNamespace
from unittest import mock

import pytest
from gi.repository import Gdk, GtkSource

import meld.style
from meld.diffpalette import (
    PALETTE_TAGS,
    DiffColours,
    DiffSide,
    get_diff_palette,
    parse_diff_colours,
    parse_diff_side,
)

STYLES_DIR = Path(__file__).parent.parent / "data" / "styles"

PALETTES = (DiffColours.red_green, DiffColours.colorblind)
SIDES = (None, DiffSide.old, DiffSide.new)


def rgba(spec: str) -> Gdk.RGBA:
    colour = Gdk.RGBA()
    assert colour.parse(spec), f"Invalid colour {spec}"
    return colour


def luminance(spec: str) -> float:
    colour = rgba(spec)
    return 0.2126 * colour.red + 0.7152 * colour.green + 0.0722 * colour.blue


@pytest.mark.parametrize(
    "palette, dark, side",
    list(itertools.product(PALETTES, (False, True), SIDES)),
)
def test_palette_selection(palette, dark, side):
    result = get_diff_palette(palette, dark, side)
    assert result is not None

    removed = get_diff_palette(palette, dark, DiffSide.old).insert
    added = get_diff_palette(palette, dark, DiffSide.new).insert

    if side == DiffSide.old:
        # Everything on the old side is removed text
        assert result.insert == result.delete == result.replace == removed
    elif side == DiffSide.new:
        # Everything on the new side is added text
        assert result.insert == result.delete == result.replace == added
    else:
        # Side-less widgets get a neutral variant
        assert result.insert == added
        assert result.delete == removed
        assert result.replace not in (added, removed)

    assert removed != added
    assert result.inline == result.replace.inline

    for tag in PALETTE_TAGS:
        tone = result.tone(tag)
        for spec in (tone.fill, tone.line, tone.inline):
            rgba(spec)
        # Inline highlights need to stand out from their chunk's fill
        assert tone.inline != tone.fill
        if dark:
            assert luminance(tone.fill) < 0.3
            assert luminance(tone.inline) > luminance(tone.fill)
        else:
            assert luminance(tone.fill) > 0.8
            assert luminance(tone.inline) < luminance(tone.fill)


@pytest.mark.parametrize("dark", (False, True))
def test_palette_hues(dark):
    def hue(spec):
        colour = rgba(spec)
        return colour.red, colour.green, colour.blue

    red_green = get_diff_palette(DiffColours.red_green, dark)
    r, g, b = hue(red_green.delete.line)
    assert r > g and r > b
    r, g, b = hue(red_green.insert.line)
    assert g > r and g > b

    colorblind = get_diff_palette(DiffColours.colorblind, dark)
    r, g, b = hue(colorblind.delete.line)
    assert r > b, "Removed text should be orange"
    r, g, b = hue(colorblind.insert.line)
    assert b > r, "Added text should be blue"


def test_known_palette_colours():
    light_old = get_diff_palette("red-green", False, "old")
    assert light_old.insert.fill == "#ffebe9"
    light_new = get_diff_palette("red-green", False, "new")
    assert light_new.insert.fill == "#e6ffec"


@pytest.mark.parametrize(
    "palette, dark, side",
    list(
        itertools.product(
            ("scheme", DiffColours.scheme, "unknown", None, 3),
            (False, True),
            SIDES,
        )
    ),
)
def test_scheme_and_unknown_palettes_use_scheme(palette, dark, side):
    assert get_diff_palette(palette, dark, side) is None


def test_palette_values_match_settings_schema():
    schema = Path(__file__).parent.parent / "data" / "org.gnome.Meld.gschema.xml"
    text = schema.read_text()
    for value in DiffColours:
        assert f'nick="{value.value}"' in text


@pytest.mark.parametrize(
    "value, expected",
    [
        ("scheme", DiffColours.scheme),
        ("red-green", DiffColours.red_green),
        ("colorblind", DiffColours.colorblind),
        ("", DiffColours.scheme),
        (None, DiffColours.scheme),
        (["red-green"], DiffColours.scheme),
    ],
)
def test_parse_diff_colours(value, expected):
    assert parse_diff_colours(value) == expected


@pytest.mark.parametrize(
    "value, expected",
    [
        ("old", DiffSide.old),
        ("new", DiffSide.new),
        (None, None),
        ("", None),
        ("left", None),
    ],
)
def test_parse_diff_side(value, expected):
    assert parse_diff_side(value) == expected


@pytest.fixture(scope="module")
def style_manager(tmp_path_factory):
    """Style scheme manager that can find Meld's own schemes"""

    styles = tmp_path_factory.mktemp("styles")
    # This mirrors bin/meld's handling for uninstalled use
    for source in STYLES_DIR.glob("*.style-scheme.xml.in"):
        shutil.copyfile(source, styles / source.name.removesuffix(".in"))

    manager = GtkSource.StyleSchemeManager.get_default()
    manager.prepend_search_path(str(styles))
    manager.force_rescan()
    return manager


@pytest.fixture
def theme(mocker, style_manager):
    """Set up meld.style for a scheme and diff colour setting"""

    mocker.patch.object(meld.style, "style_scheme", None)
    mocker.patch.object(meld.style, "base_style_scheme", None)
    mocker.patch.object(meld.style, "_use_dark", False)
    meld_settings = SimpleNamespace(diff_colors="scheme")
    mocker.patch.object(meld.style, "get_meld_settings", return_value=meld_settings)

    def _theme(scheme_id, diff_colors):
        meld_settings.diff_colors = diff_colors
        meld.style.set_base_style_scheme(style_manager.get_scheme(scheme_id))

    return _theme


def old_common_theme():
    """get_common_theme() as it was before diff palettes"""

    lookup = meld.style.colour_lookup_with_fallback
    fill_colours = {
        "insert": lookup("meld:insert", "background"),
        "delete": lookup("meld:insert", "background"),
        "conflict": lookup("meld:conflict", "background"),
        "replace": lookup("meld:replace", "background"),
        "error": lookup("meld:error", "background"),
        "focus-highlight": lookup("meld:current-line-highlight", "foreground"),
        "current-chunk-highlight": lookup("meld:current-chunk-highlight", "background"),
        "overscroll": lookup("meld:overscroll", "background"),
    }
    line_colours = {
        "insert": lookup("meld:insert", "line-background"),
        "delete": lookup("meld:insert", "line-background"),
        "conflict": lookup("meld:conflict", "line-background"),
        "replace": lookup("meld:replace", "line-background"),
        "error": lookup("meld:error", "line-background"),
    }
    return fill_colours, line_colours


def colour_strings(colours):
    return {key: colour.to_string() for key, colour in colours.items()}


@pytest.mark.parametrize(
    "scheme_id", ("meld-base", "meld-dark", "meld-lens-light", "meld-lens-dark")
)
@pytest.mark.parametrize("side", SIDES)
def test_scheme_setting_keeps_scheme_colours(theme, scheme_id, side):
    theme(scheme_id, "scheme")

    fill, line = meld.style.get_common_theme(side)
    old_fill, old_line = old_common_theme()

    assert list(fill) == list(old_fill)
    assert list(line) == list(old_line)
    assert colour_strings(fill) == colour_strings(old_fill)
    assert colour_strings(line) == colour_strings(old_line)

    inline = meld.style.get_inline_colour(side)
    expected = meld.style.colour_lookup_with_fallback("meld:inline", "background")
    assert inline.to_string() == expected.to_string()


@pytest.mark.parametrize("palette", PALETTES)
@pytest.mark.parametrize(
    "scheme_id, dark",
    [("meld-base", False), ("meld-dark", True), ("meld-lens-dark", True)],
)
@pytest.mark.parametrize("side", SIDES)
def test_palette_setting_overrides_chunk_colours(theme, palette, scheme_id, dark, side):
    theme(scheme_id, palette)
    expected = get_diff_palette(palette, dark, side)

    fill, line = meld.style.get_common_theme(side)
    old_fill, old_line = old_common_theme()

    # Keys stay the same, so that all widgets keep working
    assert list(fill) == list(old_fill)
    assert list(line) == list(old_line)

    for tag in PALETTE_TAGS:
        tone = expected.tone(tag)
        assert fill[tag].equal(rgba(tone.fill))
        assert line[tag].equal(rgba(tone.line))

    # Everything else still comes from the scheme
    for key in set(old_fill) - set(PALETTE_TAGS):
        assert fill[key].equal(old_fill[key])
    for key in set(old_line) - set(PALETTE_TAGS):
        assert line[key].equal(old_line[key])

    assert meld.style.get_inline_colour(side).equal(rgba(expected.inline))


def test_missing_settings_use_scheme_colours(theme, mocker):
    theme("meld-base", "red-green")
    mocker.patch.object(meld.style, "get_meld_settings", return_value=None)

    fill, line = meld.style.get_common_theme("old")
    old_fill, old_line = old_common_theme()
    assert colour_strings(fill) == colour_strings(old_fill)
    assert colour_strings(line) == colour_strings(old_line)


@pytest.mark.parametrize(
    "num_panes, meta, expected",
    [
        (1, {}, (None, None, None)),
        (2, {}, ("old", "new", None)),
        (3, {}, (None, None, None)),
        (2, {"diff-sides": ("new", "old")}, ("new", "old", None)),
        # Invalid overrides are ignored
        (2, {"diff-sides": ("new", "new")}, ("old", "new", None)),
        (2, {"diff-sides": ("left",)}, ("old", "new", None)),
        # Sides make no sense for three-way comparisons
        (3, {"diff-sides": ("new", "old")}, (None, None, None)),
    ],
)
def test_filediff_diff_sides(num_panes, meta, expected):
    from meld.filediff import FileDiff

    filediff = mock.MagicMock()
    filediff.num_panes = num_panes
    filediff.meta = meta
    filediff.textview = [SimpleNamespace(props=SimpleNamespace()) for _ in range(3)]

    FileDiff._update_diff_sides(filediff)

    assert tuple(t.props.diff_side for t in filediff.textview) == expected
