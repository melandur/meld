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
