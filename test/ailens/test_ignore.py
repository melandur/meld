import pytest

from meld.ailens.ignore import (
    GENERATED_PATTERNS,
    IgnoreRules,
    create_ignore_rules,
    is_ignored,
)


def ignores(patterns: str, path: str) -> bool:
    return IgnoreRules(patterns).ignores(path)


@pytest.mark.parametrize(
    "path",
    [
        "Cargo.lock",
        "deep/poetry.lock",
        "package-lock.json",
        "web/pnpm-lock.yaml",
        "yarn.lock",
        "npm-shrinkwrap.json",
        "dist/app.min.js",
        "style.min.css",
        "app.js.map",
        "node_modules/x/index.js",
        "packages/a/node_modules/b.js",
    ],
)
def test_generated_files_are_ignored(path):
    assert create_ignore_rules().ignores(path)


@pytest.mark.parametrize(
    "path",
    ["src/app.js", "lock.txt", "node_modules", "app.min.jsx", "README.md"],
)
def test_source_files_are_not_ignored(path):
    assert not create_ignore_rules().ignores(path)


def test_ailensignore_adds_to_the_generated_rules():
    rules = create_ignore_rules("secrets/\n*.pem\n")
    assert rules.ignores("secrets/key.txt")
    assert rules.ignores("certs/server.pem")
    assert rules.ignores("yarn.lock")
    assert not rules.ignores("src/a.ts")


def test_ailensignore_can_re_include_a_generated_file():
    rules = create_ignore_rules("!yarn.lock\n")
    assert not rules.ignores("yarn.lock")
    assert rules.ignores("Cargo.lock")


def test_is_ignored_checks_both_paths_of_a_rename():
    rules = create_ignore_rules("secrets/\n")
    assert is_ignored(rules, "public/key.txt", "secrets/key.txt")
    assert is_ignored(rules, "secrets/key.txt")
    assert not is_ignored(rules, "public/key.txt", "other/key.txt")
    assert not is_ignored(rules, "public/key.txt")


@pytest.mark.parametrize(
    "patterns, path, expected",
    [
        # A name matches at any depth
        ("foo", "foo", True),
        ("foo", "a/foo", True),
        ("foo", "a/foo/b.txt", True),
        ("foo", "foobar", False),
        # A leading slash anchors to the root
        ("/foo", "foo", True),
        ("/foo", "a/foo", False),
        ("/foo", "foo/b.txt", True),
        # So does a slash in the middle
        ("a/b", "a/b", True),
        ("a/b", "x/a/b", False),
        ("a/*/c", "a/x/c", True),
        ("a/*/c", "a/x/y/c", False),
        # A trailing slash matches only directories
        ("foo/", "foo", False),
        ("foo/", "foo/", True),
        ("foo/", "foo/b.txt", True),
        ("foo/", "a/foo/b.txt", True),
        ("/foo/", "a/foo/b.txt", False),
        # Wildcards don't cross directories
        ("*.txt", "a.txt", True),
        ("*.txt", "a/b/c.txt", True),
        ("a*", "a/b", True),
        ("f?o", "fao", True),
        ("f?o", "f/o", False),
        ("a/*.txt", "a/b/c.txt", False),
        # **
        ("**/foo", "foo", True),
        ("**/foo", "a/b/foo", True),
        ("**/foo/bar", "x/foo/bar", True),
        ("a/**/b", "a/b", True),
        ("a/**/b", "a/x/y/b", True),
        ("a/**/b", "x/a/b", False),
        ("a/**", "a", False),
        ("a/**", "a/x", True),
        ("a/**", "a/x/y", True),
        ("**", "anything/at/all", True),
        ("a**b", "axxb", True),
        ("a**b", "ax/xb", False),
        # Bracket expressions
        ("f[ab]o", "fbo", True),
        ("f[ab]o", "fco", False),
        ("f[!a]o", "fbo", True),
        ("f[!a]o", "fao", False),
        ("f[^a]o", "fao", False),
        ("f[a-c]o", "fbo", True),
        ("f[]]o", "f]o", True),
        ("f[[:digit:]]o", "f1o", True),
        ("f[[:digit:]]o", "fxo", False),
        ("f[!a]o", "f/o", False),
        # An unclosed bracket never matches, as in git
        ("ba[r", "ba[r", False),
        # Of a reversed range, git matches only its start
        ("[z-a]", "z", True),
        ("[z-a]", "m", False),
        # Escapes
        ("\\*star", "*star", True),
        ("\\*star", "xstar", False),
        ("a\\[b", "a[b", True),
        ("\\!bang", "!bang", True),
        ("\\#hash", "#hash", True),
        # Comments and blank lines match nothing
        ("#comment", "#comment", False),
        ("\n\n", "a", False),
        # Trailing spaces are dropped unless escaped
        ("foo  ", "foo", True),
        ("foo\\ ", "foo ", True),
        ("foo\\ ", "foo", False),
        # A trailing backslash makes the pattern invalid
        ("trail\\", "trail", False),
        # Case-insensitive, like the npm ignore package
        ("Foo.TXT", "foo.txt", True),
        ("*.LOCK", "Cargo.lock", True),
        # Negation: the last match wins
        ("*.txt\n!keep.txt", "keep.txt", False),
        ("*.txt\n!keep.txt", "drop.txt", True),
        ("!keep.txt\n*.txt", "keep.txt", True),
        # A file in an ignored directory can't be re-included
        ("foo/\n!foo/keep.txt", "foo/keep.txt", True),
        ("foo/*\n!foo/keep.txt", "foo/keep.txt", False),
        # CRLF files
        ("a.txt\r\nb.txt\r\n", "b.txt", True),
        # A byte order mark is dropped
        ("﻿bom", "bom", True),
        # Unicode names
        ("données/", "données/x", True),
        ("*.ñ", "año.ñ", True),
    ],
)
def test_gitignore_syntax(patterns, path, expected):
    assert ignores(patterns, path) is expected


def test_empty_path_is_not_ignored():
    assert not IgnoreRules("*").ignores("")


def test_add_takes_a_list_and_chains():
    rules = IgnoreRules().add(["a", "#b", ""]).add("c")
    assert rules.ignores("a")
    assert not rules.ignores("#b")
    assert rules.ignores("c")


def test_generated_patterns_match_ai_lens():
    assert GENERATED_PATTERNS == (
        "*.lock",
        "package-lock.json",
        "pnpm-lock.yaml",
        "yarn.lock",
        "npm-shrinkwrap.json",
        "*.min.js",
        "*.min.css",
        "*.map",
        "node_modules/",
    )
