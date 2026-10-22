
About Meld
==========

Meld is a visual diff and merge tool targeted at developers. Meld helps you
compare files, directories, and version controlled projects. It provides
two- and three-way comparison of both files and directories, and supports
many version control systems including Git, Mercurial, Bazaar, CVS, Darcs
and Subversion.

Meld helps you review code changes, understand patches, and makes enormous
merge conflicts slightly less painful. Git Compare compares any two
revisions of a Git repository, with a Why column saying which commit made
each change, why, and why it is the right change (AI Lens).

This is a rewrite of Meld in Rust with an [iced](https://iced.rs) user
interface. The original Python and GTK version lives on upstream; its last
state in this repository is commit 29d59e07.

Meld is licensed under the GPL v2 or later.


Requirements
------------

* Rust 1.88 or later (the workspace is developed with 1.98.1)
* A C compiler (for the bundled Oniguruma regular expression library)
* A GPU driver with Vulkan, Metal, DirectX 12 or OpenGL support
* At run time, optionally: `git` and other version control tools, `gh` and
  `glab` for pull requests, and Claude Code (`claude`) for AI Lens
  explanations


Building and running
--------------------

```sh
$ cargo run --release -p meld -- [arguments]
```

for example:

```sh
$ cargo run --release -p meld -- file1 file2           # compare files
$ cargo run --release -p meld -- dir1 dir2 dir3        # compare folders
$ cargo run --release -p meld -- .                     # version control view
$ cargo run --release -p meld -- --git --base main     # Git Compare
$ cargo run --release -p meld -- --auto-merge local base remote -o merged
```

`meld --help` lists all options. They match the Python version's, so Meld
still works as a `git mergetool` or `git difftool`.

To install a desktop entry and icon for your user, build a release binary
and run `crates/meld/data/install-desktop.sh`.

Settings are stored in `$XDG_CONFIG_HOME/meld/settings.toml` and recent
comparisons in `$XDG_DATA_HOME/meld/recent.toml`.


Developing
----------

The workspace is split into crates:

| Crate          | Contents                                                         |
|----------------|------------------------------------------------------------------|
| `meld-diff`    | diff and merge engine: matchers, chunks, merging, patches, undo  |
| `meld-files`   | text loading and encodings, languages, filters, folder comparison|
| `meld-config`  | settings, recent comparisons, style schemes, diff palettes       |
| `meld-vc`      | Git, Mercurial, Subversion, Bazaar, CVS and Darcs backends       |
| `meld-ailens`  | Git Compare core: attribution, explanations, Claude Code client  |
| `meld-widgets` | iced widgets: text panes, link and chunk maps, highlighting      |
| `meld`         | the application                                                  |

```sh
$ cargo test --workspace
```

`crates/PORTING.md` describes the conventions. The version control
integration tests skip tools that aren't installed.


Contributing
------------

Upstream Meld uses GNOME's GitLab to track bugs, and user questions and
development discussions happen on the GNOME Discourse.

List of issues: https://gitlab.gnome.org/GNOME/meld/issues

Support forum:  https://discourse.gnome.org/tag/meld


Links
-----

Home page:      https://meld.app/

Documentation:  https://meld.app/help/
