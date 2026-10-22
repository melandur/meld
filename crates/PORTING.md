# Meld in Rust: porting guide

Meld is rewritten in Rust with an [iced](https://iced.rs) 0.14 front end.
Behaviour, defaults and file formats follow the Python version, which was
removed from the tree after commit 29d59e07; check that commit out (e.g.
`git show 29d59e07:meld/filediff.py`) to read the original code.

## Running

    mise exec rust@1.98.1 -- cargo run -p meld --release -- [meld arguments]

for example `-- file1 file2`, `-- dir1 dir2 dir3`, `-- .` (version control),
`-- --git --base main` or `-- --git --pr 123` (review a pull request).
`cargo run -p meld -- --help` lists the options, which match the Python
`meld` launcher.

## Toolchain

    mise exec rust@1.98.1 -- cargo build
    mise exec rust@1.98.1 -- cargo test -p <crate>

Edition 2024. Shared dependency versions live in `[workspace.dependencies]` of
the root `Cargo.toml`; crates use `dep.workspace = true`. Add a new shared
dependency there first.

## Crates

| Crate          | Ports                                                                 | Depends on |
|----------------|-----------------------------------------------------------------------|------------|
| `meld-diff`    | `meld/matchers/*`, `undo.py`, `syncpoints.py` model, unified-diff/patch output, text filters applied to buffers, buffer-line helpers | – |
| `meld-files`   | `iohelpers.py`, `languageguess.py`, `filters.py`, `misc.py`, `tree.py` states, the non-GTK folder comparison logic of `dirdiff.py`, `archivehelpers.py`, `externalhelpers.py` | – |
| `meld-config`  | `settings.py` + `data/org.gnome.Meld.gschema.xml` (as TOML), `recent.py`, `style.py`, `diffpalette.py`, style-scheme XML loading | – |
| `meld-vc`      | `meld/vc/*`                                                           | – |
| `meld-ailens`  | `meld/ailens/*`                                                       | – |
| `meld-forge`   | new: pull/merge requests through `gh` (GitHub) and `glab` (GitLab): lists, details, comments, posting reviews and replies, fetching into `refs/meld/pr/…` | meld-ailens |
| `meld-widgets` | iced widgets replacing `sourceview.py`, `linkmap.py`, `chunkmap.py`, `actiongutter.py`, `gutterrendererchunk.py`, syntax highlighting | iced, syntect |
| `meld`         | the application: window, tabs, file/folder/VC/Git Compare views, dialogs, preferences, CLI | all |

`xtask` (run as `cargo xtask …`, not shipped) maintains the translation
template and catalogs; see [Translations](#translations).

Library crates other than `meld-widgets` must not depend on iced: they are
plain Rust, synchronous, and unit-tested. Long-running work (process calls,
folder scans) is exposed as plain functions or iterators; the app runs them
on background tasks.

## Conventions

- Port behaviour, not structure: idiomatic Rust (enums, `Result`, iterators)
  over GObject signal plumbing. Keep the Python module's algorithms and edge
  cases; port the Python tests in `test/` that cover a module to Rust tests.
- Line-oriented text is `Vec<String>` without line terminators unless a type
  says otherwise. Line numbers are 0-based, ranges are half-open.
- Errors: `thiserror` enums per crate. No `unwrap()` on user data.
- Every crate's `lib.rs` starts with a doc comment listing its public API and
  which Python modules it replaces.
- File and folder choosers go through `ui::chooser` (`pick_file`,
  `pick_folder`, `save_file`), never rfd directly: it falls back from the
  portal to zenity/kdialog and then to a dialog in the window.
- Settings live in `$XDG_CONFIG_HOME/meld/settings.toml` (keys from the
  gschema, kebab-case, same defaults); recent comparisons in
  `$XDG_DATA_HOME/meld/recent.toml`.

## Translations

The Python version's catalogs in `po/` are used as they are: `build.rs` of
the `meld` crate parses the languages in `po/LINGUAS` (skipping fuzzy and
untranslated entries) and embeds them; no system gettext is needed. Python
strings are mapped to the Rust syntax on the way in: `%s`/`%d` and `{}`
become `{}`, `%(name)s` becomes `{name}`, GTK mnemonics (`_Save`, `保存(_S)`)
are dropped. The language comes from `LANGUAGE`, `LC_ALL`, `LC_MESSAGES`
and `LANG` (`LANGUAGE=de cargo run -p meld` to try one).

Mark every user-visible string (`crates/meld/src/i18n/mod.rs` has the
details):

- `tr!("Save")` gives a `&'static str`; with arguments,
  `tr!("Compare {} with {other}", a, other = b)` gives a `String`. Only `{}`
  and `{name}` placeholders, with explicit `name = value` arguments; format
  numbers before passing them. Translations may reorder placeholders.
- `trc!("shortcut window", "Quit Meld")` for messages with a `msgctxt`.
- `trn!("{} file", "{} files", n, n)` for plurals; the count is passed again
  to show it.
- `n_!("Label")` (`nc_!(ctx, "Label")` with a context) marks a message in
  a table; translate it where it is shown with `i18n::gettext(label)`
  (`i18n::pgettext`). Labels the library crates return are marked
  in `crates/meld/src/i18n/library.rs`.

Reuse the Python wording where the meaning is the same, so that existing
translations apply: `cargo xtask msgids [lang]` lists the Python messages
in the Rust syntax with how many languages translate each (`USED` marks
those the app uses). Maintenance:

    mise exec rust@1.98.1 -- cargo xtask check      # badly marked messages
    mise exec rust@1.98.1 -- cargo xtask pot        # regenerate po/meld.pot
    mise exec rust@1.98.1 -- cargo xtask update-po  # pot + msgmerge into po/*.po
    mise exec rust@1.98.1 -- cargo xtask stats      # translated messages per language

`update-po` first rewrites each catalog into the Rust syntax (so existing
translations match exactly), then runs `msgmerge --previous` against the
new template; without `msgmerge` it only regenerates `po/meld.pot`.
