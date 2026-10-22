//! meld-files: GUI-free file and folder helpers for Meld
//!
//! Everything here is synchronous plain Rust: blocking work (reading
//! files, scanning folders) is exposed as functions and iterators that
//! the app runs on background tasks. Public types are `Send`, and
//! errors are `thiserror` enums.
//!
//! # Modules and the Python they replace
//!
//! | Module | Replaces | Main API |
//! |---|---|---|
//! | [`state`] | `STATE_*` in `meld/tree.py` / `meld/vc/_vc.py` | [`State`] (ordered like the Python ints), [`STATE_MAX`] |
//! | [`text`] | file loading/saving in `meld/filediff.py`, `meld/meldbuffer.py`; `get_custom_encoding_candidates()` in `meld/sourceview.py` | [`load_text_file`], [`decode_text`], [`save_text_file`], [`encode_text`], [`write_atomically`], [`LoadedText`], [`TextEncoding`], [`Newlines`], [`NewlineType`], [`file_mtime`], [`modified_since`] |
//! | [`trash`] | `trash_or_confirm()` in `meld/iohelpers.py` | [`trash_or_confirm`] (freedesktop.org trash), [`delete_permanently`], [`TrashError::NeedsConfirmation`] |
//! | [`iohelpers`] | the rest of `meld/iohelpers.py` | [`find_shared_parent_path`], [`format_home_relative_path`], [`format_parent_relative_path`], [`is_file_on_tmpfs`] |
//! | [`languageguess`] | `meld/languageguess.py`, `LanguageManager` in `meld/sourceview.py`, `get_language_sample_text()` | [`language_from_path`], [`guess_language_from_text`], [`language_id_from_text`], [`choose_pane_languages`], [`get_language_sample_text`] |
//! | [`languages`] | GtkSourceView's language specs (embedded table) | [`LanguageSpec`], [`LANGUAGES`], [`language_for_filename`], [`language_for_mime_type`] |
//! | [`filters`] | `meld/filters.py`, `filename-filters`/`text-filters` defaults | [`FilterEntry`], [`FilterKind`], [`TextRegex`], [`ByteRegex`], [`default_filename_filters`], [`default_text_filters`] |
//! | [`misc`] | GUI-free parts of `meld/misc.py` | [`all_same`], [`shorten_names`], [`merge_intervals`], [`apply_text_filters`], [`apply_byte_filters`], [`calc_syncpoint`] |
//! | [`dirdiff`] | the non-GTK logic of `meld/dirdiff.py` | [`FolderComparison`] (`scan_dir`, `walk`, `walk_cancellable`, `scan_tree`, `row_status`), [`files_same`], [`remove_blank_lines`], [`StatusFilters`], [`FolderOptions`], [`CompareArgs`] |
//! | [`fileops`] | `copy2()`/`copytree()` in `meld/misc.py`, `DirDiff.copy_selected()` | [`copy_entry`], [`copy2`], [`copytree`], [`create_missing_dirs`] |
//! | [`archive`] | `meld/archivehelpers.py` | [`is_archive`], [`open_archive`] / [`extract_archive`] ([`ArchiveFolder`]), [`mount_archive_command`], [`unmount_archive_command`], [`ArchiveMounts`] |
//! | [`external`] | `meld/externalhelpers.py` | [`open_external_command`], [`make_custom_editor_command`], [`spawn_detached`] |
//!
//! Language IDs are GtkSourceView's (`"python3"`, `"sh"`, `"rust"`,
//! `"chdr"`, ...). Regular expressions in filters use Python's syntax
//! and validity rules.

pub mod archive;
pub mod dirdiff;
pub mod external;
pub mod fileops;
pub mod filters;
pub mod iohelpers;
pub mod languageguess;
pub mod languages;
pub mod misc;
mod pyregex;
mod shlex;
pub mod state;
pub mod text;
pub mod trash;

pub use archive::{
    ArchiveFolder, ArchiveFolderKind, ArchiveMounts, extract_archive, is_archive,
    mount_archive_command, open_archive, unmount_archive_command,
};
pub use dirdiff::{
    CompareArgs, ComparisonCache, DirScan, FileComparison, FolderComparison, FolderNode,
    FolderOptions, FolderTree, PaneStatus, Row, RowStatus, ScanOutcome, StatusFilters, Walk,
    WalkItem, files_same, remove_blank_lines,
};
pub use external::{
    ExternalEditorSettings, ExternalError, make_custom_editor_command, open_external_command,
    spawn_detached,
};
pub use fileops::{CopyOutcome, FileOpError, copy_entry, copy2, copytree, create_missing_dirs};
pub use filters::{
    ByteRegex, FilterEntry, FilterKind, TextRegex, default_filename_filters, default_text_filters,
};
pub use iohelpers::{
    find_shared_parent_path, format_home_relative_path, format_parent_relative_path,
    is_file_on_tmpfs,
};
pub use languageguess::{
    choose_pane_languages, get_language_sample_text, guess_language_from_text, language_from_path,
    language_id_from_text,
};
pub use languages::{LANGUAGES, LanguageSpec, language_for_filename, language_for_mime_type};
pub use misc::{
    all_same, apply_byte_filters, apply_text_filters, calc_syncpoint, merge_intervals,
    shorten_names,
};
pub use state::{STATE_MAX, State};
pub use text::{
    LoadError, LoadOptions, LoadedText, NewlineType, Newlines, SaveError, SaveOptions,
    TextEncoding, decode_text, encode_text, file_mtime, load_text_file, modified_since,
    save_text_file, write_atomically,
};
pub use trash::{TrashError, TrashedFile, delete_permanently, trash_or_confirm};

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_send_sync<T: Send + Sync>() {}

    #[test]
    fn public_types_are_send() {
        assert_send_sync::<FolderComparison>();
        assert_send_sync::<Walk>();
        assert_send_sync::<FolderTree>();
        assert_send_sync::<FilterEntry>();
        assert_send_sync::<LoadedText>();
        assert_send_sync::<LoadError>();
        assert_send_sync::<SaveError>();
        assert_send_sync::<TrashError>();
        assert_send_sync::<FileOpError>();
    }
}
