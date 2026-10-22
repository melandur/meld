//! Port of test/ailens/test_ignore.py

use meld_ailens::ignore::{GENERATED_PATTERNS, IgnoreRules, create_ignore_rules, is_ignored};

fn ignores(patterns: &str, path: &str) -> bool {
    IgnoreRules::from_text(patterns).ignores(path)
}

#[test]
fn generated_files_are_ignored() {
    for path in [
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
    ] {
        assert!(create_ignore_rules(None).ignores(path), "{path}");
    }
}

#[test]
fn source_files_are_not_ignored() {
    for path in [
        "src/app.js",
        "lock.txt",
        "node_modules",
        "app.min.jsx",
        "README.md",
    ] {
        assert!(!create_ignore_rules(None).ignores(path), "{path}");
    }
}

#[test]
fn ailensignore_adds_to_the_generated_rules() {
    let rules = create_ignore_rules(Some("secrets/\n*.pem\n"));
    assert!(rules.ignores("secrets/key.txt"));
    assert!(rules.ignores("certs/server.pem"));
    assert!(rules.ignores("yarn.lock"));
    assert!(!rules.ignores("src/a.ts"));
}

#[test]
fn ailensignore_can_re_include_a_generated_file() {
    let rules = create_ignore_rules(Some("!yarn.lock\n"));
    assert!(!rules.ignores("yarn.lock"));
    assert!(rules.ignores("Cargo.lock"));
}

#[test]
fn is_ignored_checks_both_paths_of_a_rename() {
    let rules = create_ignore_rules(Some("secrets/\n"));
    assert!(is_ignored(
        &rules,
        "public/key.txt",
        Some("secrets/key.txt")
    ));
    assert!(is_ignored(&rules, "secrets/key.txt", None));
    assert!(!is_ignored(&rules, "public/key.txt", Some("other/key.txt")));
    assert!(!is_ignored(&rules, "public/key.txt", None));
}

#[test]
fn gitignore_syntax() {
    let cases: &[(&str, &str, bool)] = &[
        // A name matches at any depth
        ("foo", "foo", true),
        ("foo", "a/foo", true),
        ("foo", "a/foo/b.txt", true),
        ("foo", "foobar", false),
        // A leading slash anchors to the root
        ("/foo", "foo", true),
        ("/foo", "a/foo", false),
        ("/foo", "foo/b.txt", true),
        // So does a slash in the middle
        ("a/b", "a/b", true),
        ("a/b", "x/a/b", false),
        ("a/*/c", "a/x/c", true),
        ("a/*/c", "a/x/y/c", false),
        // A trailing slash matches only directories
        ("foo/", "foo", false),
        ("foo/", "foo/", true),
        ("foo/", "foo/b.txt", true),
        ("foo/", "a/foo/b.txt", true),
        ("/foo/", "a/foo/b.txt", false),
        // Wildcards don't cross directories
        ("*.txt", "a.txt", true),
        ("*.txt", "a/b/c.txt", true),
        ("a*", "a/b", true),
        ("f?o", "fao", true),
        ("f?o", "f/o", false),
        ("a/*.txt", "a/b/c.txt", false),
        // **
        ("**/foo", "foo", true),
        ("**/foo", "a/b/foo", true),
        ("**/foo/bar", "x/foo/bar", true),
        ("a/**/b", "a/b", true),
        ("a/**/b", "a/x/y/b", true),
        ("a/**/b", "x/a/b", false),
        ("a/**", "a", false),
        ("a/**", "a/x", true),
        ("a/**", "a/x/y", true),
        ("**", "anything/at/all", true),
        ("a**b", "axxb", true),
        ("a**b", "ax/xb", false),
        // Bracket expressions
        ("f[ab]o", "fbo", true),
        ("f[ab]o", "fco", false),
        ("f[!a]o", "fbo", true),
        ("f[!a]o", "fao", false),
        ("f[^a]o", "fao", false),
        ("f[a-c]o", "fbo", true),
        ("f[]]o", "f]o", true),
        ("f[[:digit:]]o", "f1o", true),
        ("f[[:digit:]]o", "fxo", false),
        ("f[!a]o", "f/o", false),
        // An unclosed bracket never matches, as in git
        ("ba[r", "ba[r", false),
        // Of a reversed range, git matches only its start
        ("[z-a]", "z", true),
        ("[z-a]", "m", false),
        // Escapes
        ("\\*star", "*star", true),
        ("\\*star", "xstar", false),
        ("a\\[b", "a[b", true),
        ("\\!bang", "!bang", true),
        ("\\#hash", "#hash", true),
        // Comments and blank lines match nothing
        ("#comment", "#comment", false),
        ("\n\n", "a", false),
        // Trailing spaces are dropped unless escaped
        ("foo  ", "foo", true),
        ("foo\\ ", "foo ", true),
        ("foo\\ ", "foo", false),
        // A trailing backslash makes the pattern invalid
        ("trail\\", "trail", false),
        // Case-insensitive, like the npm ignore package
        ("Foo.TXT", "foo.txt", true),
        ("*.LOCK", "Cargo.lock", true),
        // Negation: the last match wins
        ("*.txt\n!keep.txt", "keep.txt", false),
        ("*.txt\n!keep.txt", "drop.txt", true),
        ("!keep.txt\n*.txt", "keep.txt", true),
        // A file in an ignored directory can't be re-included
        ("foo/\n!foo/keep.txt", "foo/keep.txt", true),
        ("foo/*\n!foo/keep.txt", "foo/keep.txt", false),
        // CRLF files
        ("a.txt\r\nb.txt\r\n", "b.txt", true),
        // A byte order mark is dropped
        ("\u{feff}bom", "bom", true),
        // Unicode names
        ("données/", "données/x", true),
        ("*.ñ", "año.ñ", true),
    ];
    for &(patterns, path, expected) in cases {
        assert_eq!(
            ignores(patterns, path),
            expected,
            "{patterns:?} on {path:?}"
        );
    }
}

#[test]
fn empty_path_is_not_ignored() {
    assert!(!IgnoreRules::from_text("*").ignores(""));
}

#[test]
fn add_takes_a_list_and_chains() {
    let mut rules = IgnoreRules::new();
    rules.add(&["a", "#b", ""]).add_text("c");
    assert!(rules.ignores("a"));
    assert!(!rules.ignores("#b"));
    assert!(rules.ignores("c"));
}

#[test]
fn generated_patterns_match_ai_lens() {
    assert_eq!(
        GENERATED_PATTERNS,
        [
            "*.lock",
            "package-lock.json",
            "pnpm-lock.yaml",
            "yarn.lock",
            "npm-shrinkwrap.json",
            "*.min.js",
            "*.min.css",
            "*.map",
            "node_modules/",
        ]
    );
}
