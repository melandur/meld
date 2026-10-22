//! File comparison (`meld/filediff.py`), with the find bar
//! (`ui/findbar.py`), patch dialog (`patchdialog.py`), status bar
//! (`ui/statusbar.py`, `ui/bufferselectors.py`), sync points
//! (`syncpoints.py`) and message areas (`ui/msgarea.py`).
//!
//! The widget buffers ([`meld_widgets::Buffer`]) are what the panes show
//! and edit; a [`meld_diff::FileComparison`] holds the same lines and the
//! differ. Every edit goes to both (see [`edits`]): user edits are
//! converted to primitive edits for the comparison, edits made by the
//! comparison (chunk actions, merges) are replayed on the widget buffer.
//! Undo records widget edits per pane. Loading, saving and full diffs
//! run on background tasks; while a comparison is computed the panes are
//! read-only and Escape cancels it.
//!
//! # Embedding
//!
//! Other views (Git Compare) embed a two-pane comparison, like Python's
//! `FileDiff(2)` + `set_files` + `set_meta`:
//!
//! - [`FileDiff::embedded`]: an empty two-pane comparison. It emits no
//!   window events of its own (no recent entries, no closing); the only
//!   [`Event`]s are `Notify`, `SettingsChanged`, and `Close` once
//!   [`FileDiff::request_close`] has dealt with unsaved changes.
//! - [`FileDiff::set_files`] loads [`EmbeddedFile`]s (path, pane label,
//!   read-only flag) and compares them.
//! - [`FileDiff::pane_geometry`] returns a pane's [`PaneGeometry`]; line
//!   `i` (0-based) is drawn at [`PaneGeometry::line_y`]`(i)` below
//!   [`FileDiff::header_height`] (which accounts for a message shown above
//!   that pane only, and for wrapped lines).
//! - [`FileDiff::scroll_to_line`], [`FileDiff::first_change`] (the
//!   pane-1 line of the first chunk), [`FileDiff::is_loading`].
//! - [`FileDiff::header_height`]: how far below the top of the view the
//!   file header row ends; a pane's message area (fixed height when
//!   embedded) is included in its [`PaneGeometry::top`];
//!   [`FileDiff::set_hover`]: extra line highlights per pane drawn over
//!   the chunks.
//! - The parent maps [`Message`] into its own and forwards `update`,
//!   `command`, `view`, `toolbar`, `menu_items`, `unsaved`,
//!   `request_close`, `settings_changed` and `subscription`.

mod chunks;
mod dialogs;
mod edits;
mod files;
mod find;
mod io;
mod ops;
mod style;
mod view;

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant, SystemTime};

use iced::keyboard;
use iced::widget::Id;
use iced::{Element, Font, Size, Subscription, Task, Vector};
use meld_diff::{FileComparison, InlineCache, Syncpoints, TextBuffer, UndoAction, UndoSequence};
use meld_files::{NewlineType, Newlines, TextEncoding};
use meld_widgets::{
    Action, Buffer, ChunkAction, Highlighter, InlineHighlight, LineFlash, MapScroll, MapStyle, Metrics, PaneStyle,
    WrapLayout, WrapMode,
};

use crate::command::Command;
use crate::shared::Shared;
use crate::ui::{BannerKind, menu};
use crate::views::Event;

use chunks::{CursorDetails, Derived};
use find::{FindBar, FindMessage};

pub use io::{LoadFailure, Loaded, SaveFailure, Saved};

/// What to compare (the window's `Comparison::Files`).
#[derive(Debug, Clone, Default)]
pub struct Options {
    pub paths: Vec<Option<PathBuf>>,
    pub labels: Vec<String>,
    pub merge_output: Option<PathBuf>,
    pub auto_merge: bool,
    pub extras: crate::comparison::FileExtras,
}

/// A file for an embedded comparison.
#[allow(dead_code)] // used by views embedding a comparison
#[derive(Debug, Clone)]
pub struct EmbeddedFile {
    pub path: PathBuf,
    pub label: String,
    pub read_only: bool,
}

/// Vertical geometry of a pane's text area.
#[allow(dead_code)] // used by views embedding a comparison
#[derive(Debug, Clone, Copy)]
pub struct PaneGeometry<'a> {
    pub scroll_y: f32,
    pub line_height: f32,
    pub viewport_height: f32,
    pub line_count: usize,
    /// How far below [`FileDiff::header_height`] the pane's text starts
    /// (a message area above this pane only).
    pub top: f32,
    /// Visual rows of the lines when they are wrapped.
    pub wrap: Option<&'a WrapLayout>,
}

#[allow(dead_code)] // used by views embedding a comparison
impl PaneGeometry<'_> {
    /// Top of 0-based `line` (clamped to the end of the text) relative to
    /// [`FileDiff::header_height`].
    pub fn line_y(&self, line: usize) -> f32 {
        let wrap = self.wrap.unwrap_or(WrapLayout::identity());
        self.top + wrap.y_for_line(line.min(self.line_count), self.line_height) - self.scroll_y
    }
}

const OVERSCROLL_LINES: usize = meld_widgets::text_pane::DEFAULT_OVERSCROLL_LINES;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LoadState {
    Empty,
    Loading,
    Loaded,
    Error,
}

/// `MeldBufferData`: the file behind a pane.
#[derive(Debug, Clone)]
struct PaneFile {
    path: Option<PathBuf>,
    /// Where saving writes, when not `path` (merge output).
    savefile: Option<PathBuf>,
    custom_label: Option<String>,
    encoding: TextEncoding,
    newlines: Newlines,
    /// Line ending used when saving.
    newline: NewlineType,
    /// Modification time when loaded or last saved.
    mtime: Option<SystemTime>,
    /// Latest modification time seen on disk.
    disk_mtime: Option<SystemTime>,
    writable: bool,
    /// Never writable (embedded read-only files).
    force_read_only: bool,
    state: LoadState,
}

impl PaneFile {
    fn new() -> Self {
        PaneFile {
            path: None,
            savefile: None,
            custom_label: None,
            encoding: TextEncoding::UTF8,
            newlines: Newlines::default(),
            newline: NewlineType::native(),
            mtime: None,
            disk_mtime: None,
            writable: false,
            force_read_only: false,
            state: LoadState::Empty,
        }
    }

    fn label(&self) -> String {
        if let Some(label) = self.custom_label.as_ref().filter(|l| !l.is_empty()) {
            return label.clone();
        }
        match self.target() {
            Some(p) => p.display().to_string(),
            None => tr!("<unnamed>").into(),
        }
    }

    /// `gfiletarget`: where the pane is saved.
    fn target(&self) -> Option<&PathBuf> {
        self.savefile.as_ref().or(self.path.as_ref())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MsgId {
    Same,
    SlowHighlight,
    Syncpoints,
    LineTooLong,
    Error,
    Binary,
    Decode,
    Changed,
    SaveConflict,
    SelfCompare,
    Stopped,
}

impl MsgId {
    /// Messages removed when any file of the comparison is reloaded.
    fn transient(self) -> bool {
        matches!(self, MsgId::Same | MsgId::SlowHighlight | MsgId::Stopped)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BannerAction {
    Hide,
    ShowUnfiltered,
    KeepHighlighting,
    LoadAnyway,
    OpenExternally,
    Reload,
    SaveAnyway,
    ChooseEncoding,
    CompareAgain,
}

/// A message area banner (`MsgAreaController`): one per pane.
#[derive(Debug, Clone)]
struct Banner {
    id: MsgId,
    kind: BannerKind,
    primary: String,
    secondary: Option<String>,
    actions: Vec<(String, BannerAction)>,
}

/// An undoable edit: a widget edit in one pane.
#[derive(Debug, Clone)]
struct PaneEdit {
    pane: usize,
    edit: meld_widgets::Edit,
}

impl UndoAction for PaneEdit {
    fn buffer(&self) -> usize {
        self.pane
    }
}

struct Pane {
    id: Id,
    buffer: Buffer,
    highlighter: Highlighter,
    language: Option<&'static str>,
    language_chosen: bool,
    scroll: Vector,
    viewport: Size,
    file: PaneFile,
    /// The read-only toggle (`set_buffer_editable`).
    editable: bool,
    /// Modified according to the undo history (`checkpointed` events,
    /// like GtkTextBuffer's modified flag).
    modified: bool,
    /// Modified apart from the undo history (merge output, newline
    /// change, auto-merged text).
    extra_modified: bool,
    banner: Option<Banner>,
    /// Loaded text waiting for the initial comparison.
    pending: Option<TextBuffer>,
    inline: Vec<InlineHighlight>,
    sync_lines: Vec<usize>,
    /// Lines highlighted by an embedding view (`set_hover`), on top of
    /// the chunk highlights; when set, the chunk highlights plus these.
    hover_lines: Vec<std::ops::Range<usize>>,
    hover: Vec<meld_widgets::LineHighlight>,
    /// Visual rows when wrapping lines.
    wrap: WrapLayout,
    /// Text hidden by text filters, in the lines around the view.
    dimmed: Vec<InlineHighlight>,
    /// Find matches in the lines around the view.
    matches: Vec<InlineHighlight>,
    /// What `dimmed` and `matches` were computed for.
    decorations_key: Option<u64>,
    /// Fading highlights, as drawn now.
    flashes: Vec<LineFlash>,
}

impl Pane {
    fn new() -> Self {
        Pane {
            id: Id::unique(),
            buffer: Buffer::new(vec![String::new()]),
            highlighter: Highlighter::plain(),
            language: None,
            language_chosen: false,
            scroll: Vector::ZERO,
            viewport: Size::ZERO,
            file: PaneFile::new(),
            editable: true,
            modified: false,
            extra_modified: false,
            banner: None,
            pending: None,
            inline: Vec::new(),
            sync_lines: Vec::new(),
            hover_lines: Vec::new(),
            hover: Vec::new(),
            wrap: WrapLayout::default(),
            dimmed: Vec::new(),
            matches: Vec::new(),
            decorations_key: None,
            flashes: Vec::new(),
        }
    }
}

/// A fading highlight being animated (`TextviewLineAnimation`).
#[derive(Debug, Clone, Copy)]
struct Flash {
    pane: usize,
    start: usize,
    end: usize,
    color: iced::Color,
    started: Instant,
    duration: Duration,
    stroke: bool,
}

/// A running comparison job.
struct Job {
    cancel: Arc<AtomicBool>,
}

/// A text filter of this comparison (a copy of the `text-filters`
/// setting that the filter menu toggles).
#[derive(Debug, Clone)]
struct TextFilter {
    name: String,
    pattern: String,
    active: bool,
    regex: Option<regex::Regex>,
}

/// View options of the panes (statusbar Display menu).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ViewOptions {
    show_line_numbers: bool,
    show_whitespace: bool,
    highlight_current_line: bool,
    /// Text wrapping (`wrap-mode-bool`).
    wrap: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CloseState {
    Normal,
    Closing,
}

/// What to do after confirming that unsaved changes may be discarded.
#[derive(Debug, Clone)]
enum Pending {
    Revert,
    ReloadPane(usize, Option<&'static encoding_rs::Encoding>),
    OpenFile(usize, PathBuf),
}

#[derive(Debug, Clone)]
enum Dialog {
    /// Save changes before closing? (`save-confirm-dialog.ui`)
    ConfirmClose {
        save: Vec<bool>,
    },
    /// Discard unsaved changes? (`revert-dialog.ui`)
    ConfirmDiscard {
        unsaved: Vec<String>,
        then: Pending,
    },
    /// Mark the conflict as resolved?
    Resolve,
    Error {
        primary: String,
        secondary: String,
    },
    Patch {
        left: bool,
        reverse: bool,
        text: String,
    },
    Encoding {
        pane: usize,
        filter: String,
    },
    Language {
        pane: usize,
        filter: String,
    },
    Display {
        pane: usize,
    },
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Popup {
    CopyMenu,
    Filters,
    Context { pane: usize, x: f32, y: f32 },
}

/// User actions, from shortcuts, buttons and menus.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Act {
    PrevChange,
    NextChange,
    PrevConflict,
    NextConflict,
    PushLeft,
    PushRight,
    PullLeft,
    PullRight,
    CopyLeftUp,
    CopyLeftDown,
    CopyRightUp,
    CopyRightDown,
    DeleteChange,
    MergeAllLeft,
    MergeAllRight,
    MergeAll,
    Undo,
    Redo,
    Save,
    SaveAs,
    SaveAll,
    SavePane(usize),
    Revert,
    Refresh,
    Find,
    FindReplace,
    FindNext,
    FindPrevious,
    GoToLine,
    OpenExternal,
    OpenFolder,
    CopyFullPath,
    Cut,
    Copy,
    Paste,
    Swap,
    NextPane,
    PrevPane,
    FormatPatch,
    ToggleOverview,
    ToggleLockScrolling,
    ToggleTextFilter(usize),
    ToggleIgnoreBlank,
    SyncPoint,
    ClearSyncPoints,
    ToggleLineNumbers,
    ToggleWhitespace,
    ToggleCurrentLine,
    ToggleWrap,
    ToggleReadOnly(usize),
    ChooseEncoding(usize),
    ChooseLanguage(usize),
    SetNewline(usize, NewlineType),
    OpenFile(usize),
    Display(usize),
    ToggleOverwrite,
    Popup(Option<PopupKind>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PopupKind {
    CopyMenu,
    Filters,
}

#[derive(Debug, Clone)]
pub enum DialogMessage {
    Cancel,
    CloseSave,
    CloseDiscard,
    CloseCheck(usize, bool),
    DiscardConfirm,
    Resolve,
    PatchLeft(bool),
    PatchReverse(bool),
    PatchCopy,
    PatchSave,
    Filter(String),
    PickEncoding(&'static str),
    PickLanguage(Option<&'static str>),
}

#[derive(Debug, Clone)]
pub enum Message {
    Pane(usize, Action),
    MapScroll(usize, MapScroll),
    /// Mouse wheel over a link map: `true` for down.
    LinkScroll(bool),
    /// Animation frame for fading highlights.
    Tick(Instant),
    /// Action gutter: pair `(from, to)`, chunk index in that pair's list.
    Gutter(usize, usize, usize, ChunkAction),
    Loaded(u64, usize, Result<Box<Loaded>, LoadFailure>),
    Diffed(u64, Result<Box<io::DiffDone>, meld_diff::DiffError>),
    Saved(usize, Result<Saved, SaveFailure>),
    SaveAsChosen(usize, Option<PathBuf>),
    OpenChosen(usize, Option<PathBuf>),
    PatchSaveChosen(Option<PathBuf>),
    Done(Result<(), String>),
    VcPrepared(Result<Box<Options>, String>),
    Resolved(Result<(), String>),
    Paste(usize, Option<String>),
    Act(Act),
    Banner(usize, BannerAction),
    Find(FindMessage),
    GoToLine(String),
    GoToLineDone,
    Dialog(DialogMessage),
    ClosePopup,
    CheckDisk,
    DiskTimes(u64, Vec<Option<SystemTime>>),
    Spin,
    CapturedEscape,
}

pub struct FileDiff {
    panes: Vec<Pane>,
    cmp: Option<FileComparison>,
    job: Option<Job>,
    /// Load batch; stale load results are ignored.
    load_gen: u64,
    /// Comparison job; stale results are ignored.
    diff_gen: u64,
    undo: UndoSequence<PaneEdit>,
    cursor: CursorDetails,
    focused: Option<usize>,
    last_focused: Option<usize>,
    derived: Derived,
    inline_cache: InlineCache,
    /// Per merged chunk: inline highlights computed.
    inline_done: Vec<bool>,
    /// A pane whose file is also shown in another pane.
    self_compare: Option<usize>,
    syncpoints: Syncpoints,
    find: FindBar,
    find_id: Id,
    go_to: Option<(usize, String)>,
    go_to_id: Id,
    dialog: Option<Dialog>,
    dialog_input_id: Id,
    popup: Option<Popup>,
    merge_output: Option<PathBuf>,
    auto_merge: bool,
    extras: crate::comparison::FileExtras,
    text_filters: Vec<TextFilter>,
    ignore_blank_lines: bool,
    show_overview_map: bool,
    map_style: MapStyle,
    /// The `wrap-mode` setting.
    wrap_setting: meld_config::WrapMode,
    view_options: ViewOptions,
    settings_view_options: ViewOptions,
    highlight_syntax: bool,
    lock_scrolling: bool,
    overwrite: bool,
    metrics: Metrics,
    font: Font,
    styles: Vec<PaneStyle>,
    link_style: PaneStyle,
    tab_width: usize,
    insert_spaces: bool,
    detect_encodings: Vec<String>,
    close_state: CloseState,
    middle_saved: bool,
    resolve_asked: bool,
    force_highlight: bool,
    force_load: bool,
    embedded: bool,
    /// Pane and line to centre once the pane knows its size.
    pending_centers: Vec<(usize, usize)>,
    flashes: Vec<Flash>,
    /// `focus-highlight` colour per pane (fading highlight after jumps).
    focus_colors: Vec<iced::Color>,
    spinner: usize,
}

impl FileDiff {
    fn with_panes(num_panes: usize, shared: &Shared) -> Self {
        let n = num_panes.clamp(1, 3);
        let settings = &shared.settings;
        let view_options = ViewOptions {
            show_line_numbers: settings.show_line_numbers,
            show_whitespace: settings.enable_space_drawer,
            highlight_current_line: settings.highlight_current_line,
            wrap: settings.wrap_mode != meld_config::WrapMode::None,
        };
        let mut diff = FileDiff {
            panes: (0..n).map(|_| Pane::new()).collect(),
            cmp: None,
            job: None,
            load_gen: 0,
            diff_gen: 0,
            undo: UndoSequence::new(0..n),
            cursor: CursorDetails::default(),
            focused: None,
            last_focused: None,
            derived: Derived::default(),
            inline_cache: InlineCache::new(),
            inline_done: Vec::new(),
            self_compare: None,
            syncpoints: Syncpoints::new(n),
            find: FindBar::default(),
            find_id: Id::unique(),
            go_to: None,
            go_to_id: Id::unique(),
            dialog: None,
            dialog_input_id: Id::unique(),
            popup: None,
            merge_output: None,
            auto_merge: false,
            extras: Default::default(),
            text_filters: text_filters_from(settings),
            ignore_blank_lines: settings.ignore_blank_lines,
            show_overview_map: settings.show_overview_map,
            map_style: map_style(settings.overview_map_style),
            wrap_setting: settings.wrap_mode,
            view_options,
            settings_view_options: view_options,
            highlight_syntax: settings.highlight_syntax,
            lock_scrolling: false,
            overwrite: false,
            metrics: Metrics::new(shared.mono_size()),
            font: shared.mono_font(),
            styles: Vec::new(),
            link_style: PaneStyle::default(),
            tab_width: settings.indent_width.max(1) as usize,
            insert_spaces: settings.insert_spaces_instead_of_tabs,
            detect_encodings: settings.detect_encodings.clone(),
            close_state: CloseState::Normal,
            middle_saved: false,
            resolve_asked: false,
            force_highlight: false,
            force_load: false,
            embedded: false,
            pending_centers: Vec::new(),
            flashes: Vec::new(),
            focus_colors: Vec::new(),
            spinner: 0,
        };
        diff.refresh_styles(shared);
        for pane in &mut diff.panes {
            pane.buffer.set_indentation(diff.tab_width, diff.insert_spaces);
        }
        diff.refresh_wraps();
        diff
    }

    pub fn new(options: Options, shared: &mut Shared) -> (Self, Task<Message>) {
        let n = options.paths.len().clamp(1, 3);
        let mut diff = Self::with_panes(n, shared);
        diff.auto_merge = options.auto_merge && n == 3;
        diff.extras = options.extras;
        diff.refresh_styles(shared);
        for (pane, label) in diff.panes.iter_mut().zip(&options.labels) {
            if !label.is_empty() {
                pane.file.custom_label = Some(label.clone());
            }
        }
        if let Some(output) = options.merge_output.filter(|_| n > 1) {
            // `set_merge_output_file`
            diff.panes[1].file.savefile = Some(output.clone());
            diff.merge_output = Some(output);
        }
        let mut paths = options.paths;
        paths.truncate(n);
        let task = diff.load_files(paths, vec![None; n]);
        (diff, task)
    }

    /// A single version-controlled file compared with its base revision.
    pub fn new_vc_file(path: PathBuf, shared: &mut Shared) -> Result<(Self, Task<Message>), String> {
        let mut diff = Self::with_panes(2, shared);
        diff.panes[0].file.state = LoadState::Loading;
        diff.panes[1].file.state = LoadState::Loading;
        diff.panes[1].file.custom_label = Some(path.display().to_string());
        let left_is_local = shared.settings.vc_left_is_local;
        let order = match shared.settings.vc_merge_file_order {
            meld_config::MergeFileOrder::LocalMergeRemote => "local-merge-remote",
            meld_config::MergeFileOrder::RemoteMergeLocal => "remote-merge-local",
        };
        let task = Task::perform(
            io::blocking(move || io::prepare_vc_file(path, left_is_local, order)),
            |r| Message::VcPrepared(r.map(Box::new)),
        );
        Ok((diff, task))
    }

    /// An empty two-pane comparison for embedding in another view.
    #[allow(dead_code)] // embedding API
    pub fn embedded(shared: &mut Shared) -> Self {
        let mut diff = Self::with_panes(2, shared);
        diff.embedded = true;
        diff
    }

    /// Loads files into an embedded comparison and compares them.
    #[allow(dead_code)] // embedding API
    pub fn set_files(&mut self, files: Vec<EmbeddedFile>, shared: &mut Shared) -> Task<Message> {
        let n = files.len().clamp(1, 3);
        if n != self.panes.len() {
            let embedded = self.embedded;
            *self = Self::with_panes(n, shared);
            self.embedded = embedded;
        }
        self.undo.clear();
        for (pane, file) in self.panes.iter_mut().zip(&files) {
            pane.file = PaneFile::new();
            pane.file.custom_label = Some(file.label.clone()).filter(|l| !l.is_empty());
            pane.file.force_read_only = file.read_only;
            pane.extra_modified = false;
            pane.language_chosen = false;
        }
        let paths = files.into_iter().take(n).map(|f| Some(f.path)).collect();
        self.load_files(paths, vec![None; n])
    }

    /// The vertical geometry of `pane`.
    #[allow(dead_code)] // embedding API
    pub fn pane_geometry(&self, pane: usize) -> Option<PaneGeometry<'_>> {
        let p = self.panes.get(pane)?;
        Some(PaneGeometry {
            scroll_y: p.scroll.y,
            line_height: self.metrics.line_height,
            viewport_height: p.viewport.height,
            line_count: p.buffer.display_line_count(),
            top: self.banner_height(pane),
            wrap: p.wrap.is_wrapping().then_some(&p.wrap),
        })
    }

    /// The pane and 0-based line the cursor is on, once a pane has one.
    #[allow(dead_code)] // embedding API
    pub fn cursor_line(&self) -> Option<(usize, usize)> {
        Some((self.cursor.pane?, self.cursor.line?))
    }

    /// Height of the message area above `pane`, when it is known (fixed
    /// for embedded comparisons; otherwise measured from the panes).
    fn banner_height(&self, pane: usize) -> f32 {
        let Some(p) = self.panes.get(pane) else {
            return 0.0;
        };
        if p.banner.is_none() {
            return 0.0;
        }
        if self.embedded {
            return view::EMBEDDED_BANNER_HEIGHT;
        }
        // The panes share the body's height; one without a message shows
        // how tall that is.
        let full = self
            .panes
            .iter()
            .filter(|q| q.banner.is_none())
            .map(|q| q.viewport.height)
            .fold(0.0, f32::max);
        if full > 0.0 {
            (full - p.viewport.height).max(0.0)
        } else {
            0.0
        }
    }

    /// Scrolls `pane` so that `line` is centred, syncing the other panes.
    #[allow(dead_code)] // embedding API
    pub fn scroll_to_line(&mut self, pane: usize, line: usize) -> Task<Message> {
        if pane < self.panes.len() {
            self.center_line(pane, line);
            self.sync_scroll(pane);
            self.ensure_inline_visible();
        }
        Task::none()
    }

    /// The first line (in pane 1, or pane 0 of a single pane) of the
    /// first change.
    #[allow(dead_code)] // embedding API
    pub fn first_change(&self) -> Option<usize> {
        let cmp = self.cmp.as_ref()?;
        let pane = if self.panes.len() > 1 { 1 } else { 0 };
        let merged = cmp.differ().all_changes().first()?;
        chunks::pane_chunk(merged, pane).map(|c| c.start_a)
    }

    /// How far below the top of [`FileDiff::view`] the panes' text starts.
    #[allow(dead_code)] // embedding API
    pub fn header_height(&self) -> f32 {
        view::HEADER_ROW_HEIGHT + 1.0
    }

    /// Highlights lines of the two panes on top of the chunks (the
    /// hovered Why card of Git Compare); empty ranges clear it.
    #[allow(dead_code)] // embedding API
    pub fn set_hover(&mut self, lines: [Vec<std::ops::Range<usize>>; 2]) {
        for (pane, lines) in self.panes.iter_mut().zip(lines) {
            pane.hover_lines = lines;
        }
        self.rebuild_hover();
    }

    fn rebuild_hover(&mut self) {
        for (i, pane) in self.panes.iter_mut().enumerate() {
            pane.hover.clear();
            if pane.hover_lines.is_empty() {
                continue;
            }
            pane.hover = self.derived.highlights.get(i).cloned().unwrap_or_default();
            pane.hover.extend(
                pane.hover_lines
                    .iter()
                    .filter(|r| !r.is_empty())
                    .map(|r| meld_widgets::LineHighlight {
                        lines: r.clone(),
                        kind: meld_widgets::ChunkKind::Error,
                        current: false,
                    }),
            );
        }
    }

    /// Whether files are being loaded or compared.
    pub fn is_loading(&self) -> bool {
        self.job.is_some() || self.panes.iter().any(|p| p.file.state == LoadState::Loading)
    }

    fn is_modified(&self, pane: usize) -> bool {
        self.panes[pane].modified || self.panes[pane].extra_modified
    }

    /// `on_undo_checkpointed`: track modified flags.
    fn process_undo_events(&mut self) {
        for event in self.undo.take_events() {
            if let meld_diff::UndoEvent::Checkpointed { buffer, checkpointed } = event
                && let Some(pane) = self.panes.get_mut(buffer)
            {
                pane.modified = !checkpointed;
                if checkpointed {
                    pane.extra_modified = false;
                }
            }
        }
    }

    /// The pane actions apply to (`_get_focused_pane`, falling back to
    /// the last focused pane since buttons take focus in iced).
    fn focus_pane(&self) -> Option<usize> {
        self.focused.or(self.last_focused)
    }

    /// Whether `pane` can be edited now.
    fn editable_now(&self, pane: usize) -> bool {
        self.cmp.is_some()
            && self.job.is_none()
            && self.panes.get(pane).is_some_and(|p| p.editable)
            && !(self.auto_merge && pane != 1)
    }

    fn refresh_read_only(&mut self) {
        for pane in 0..self.panes.len() {
            let editable = self.editable_now(pane);
            self.panes[pane].buffer.set_read_only(!editable);
        }
    }

    fn refresh_styles(&mut self, shared: &Shared) {
        let n = self.panes.len();
        let sides = meld_config::diff_sides(n, self.extras.diff_sides.as_ref().map(|s| s.as_slice()));
        self.styles = (0..n)
            .map(|i| style::pane_style(shared, &style::pane_colors(shared, sides[i])))
            .collect();
        self.focus_colors = (0..n).map(|i| style::focus_color(shared, sides[i])).collect();
        // Link maps are side-less; only a two-way comparison has a side on
        // its left pane, and three-way ones have none.
        self.link_style = style::pane_style(shared, &style::link_colors(shared, sides[0]));
    }

    fn active_filters(&self) -> Vec<regex::Regex> {
        self.text_filters
            .iter()
            .filter(|f| f.active)
            .filter_map(|f| f.regex.clone())
            .collect()
    }

    pub fn title(&self) -> String {
        tab_title(&self.labels(), &self.modified_flags(), self.extras.tab_label.as_deref())
    }

    pub fn tooltip(&self) -> String {
        match &self.extras.tab_label {
            Some(label) => format!("{}\n{label}", tr!("File comparison:")),
            None => format!("{}\n{}", tr!("File comparison:"), self.labels().join("\n")),
        }
    }

    fn labels(&self) -> Vec<String> {
        self.panes.iter().map(|p| p.file.label()).collect()
    }

    fn modified_flags(&self) -> Vec<bool> {
        (0..self.panes.len()).map(|i| self.is_modified(i)).collect()
    }

    pub fn unsaved(&self) -> Vec<String> {
        (0..self.panes.len())
            .filter(|&i| self.is_modified(i))
            .map(|i| self.panes[i].file.label())
            .collect()
    }

    pub fn update(&mut self, message: Message, shared: &mut Shared, events: &mut Vec<Event>) -> Task<Message> {
        let task = self.handle(message, shared, events);
        self.after_message();
        task
    }

    /// Bookkeeping after every message or command.
    fn after_message(&mut self) {
        self.process_undo_events();
        self.refresh_wraps();
        self.ensure_inline_visible();
        self.rebuild_hover();
        self.refresh_decorations();
    }

    fn handle(&mut self, message: Message, shared: &mut Shared, events: &mut Vec<Event>) -> Task<Message> {
        match message {
            Message::Pane(pane, action) => self.pane_action(pane, action),
            Message::MapScroll(pane, s) => {
                if let Some(p) = self.panes.get_mut(pane) {
                    p.scroll.y = s.scroll_y;
                    self.sync_scroll(pane);
                }
                Task::none()
            }
            Message::LinkScroll(down) => {
                self.next_diff_in_view(down);
                Task::none()
            }
            Message::Tick(now) => {
                self.animate(now);
                Task::none()
            }
            Message::Gutter(from, to, index, action) => {
                self.gutter_action(from, to, index, action);
                Task::none()
            }
            Message::Loaded(generation, pane, result) => self.file_loaded(generation, pane, result, shared),
            Message::Diffed(generation, result) => self.diff_done(generation, result),
            Message::Saved(pane, result) => self.file_saved(pane, result, shared, events),
            Message::SaveAsChosen(pane, path) => match path {
                Some(path) => self.save_as_chosen(pane, path),
                None => {
                    self.close_state = CloseState::Normal;
                    Task::none()
                }
            },
            Message::OpenChosen(pane, path) => match path {
                Some(path) => self.confirm_discard(None, Pending::OpenFile(pane, path)),
                None => Task::none(),
            },
            Message::PatchSaveChosen(path) => self.save_patch(path),
            Message::Done(result) => {
                if let Err(err) = result {
                    self.dialog = Some(Dialog::Error {
                        primary: tr!("Error").into(),
                        secondary: err,
                    });
                }
                Task::none()
            }
            Message::VcPrepared(result) => match result {
                Ok(options) => {
                    let (mut diff, task) = Self::new(*options, shared);
                    diff.embedded = self.embedded;
                    *self = diff;
                    task
                }
                Err(err) => {
                    for pane in &mut self.panes {
                        pane.file.state = LoadState::Error;
                    }
                    self.panes[0].banner = Some(Banner {
                        id: MsgId::Error,
                        kind: BannerKind::Error,
                        primary: tr!("Could not open the version control comparison").into(),
                        secondary: Some(err),
                        actions: vec![(tr!("Hide").into(), BannerAction::Hide)],
                    });
                    Task::none()
                }
            },
            Message::Resolved(result) => {
                if let Err(err) = result {
                    events.push(Event::Notify(tr!("Could not mark the conflict resolved: {}", err)));
                }
                self.finish_close(events)
            }
            Message::Paste(pane, text) => {
                if let Some(text) = text
                    && self.editable_now(pane)
                {
                    let action = Action::Edit(meld_widgets::EditAction::Paste(Arc::new(text)));
                    return self.pane_action(pane, action);
                }
                Task::none()
            }
            Message::Act(act) => self.act(act, shared, events),
            Message::Banner(pane, action) => self.banner_action(pane, action),
            Message::Find(message) => self.find_message(message),
            Message::GoToLine(text) => self.go_to_line_text(text),
            Message::GoToLineDone => {
                let pane = self.go_to.take().map(|(p, _)| p);
                match pane {
                    Some(p) => iced::widget::operation::focus(self.panes[p].id.clone()),
                    None => Task::none(),
                }
            }
            Message::Dialog(message) => self.dialog_message(message, shared, events),
            Message::ClosePopup => {
                self.popup = None;
                Task::none()
            }
            Message::CheckDisk => self.check_disk(),
            Message::DiskTimes(generation, times) => {
                self.disk_times(generation, times);
                Task::none()
            }
            Message::Spin => {
                self.spinner = self.spinner.wrapping_add(1);
                Task::none()
            }
            Message::CapturedEscape => {
                if self.find.visible {
                    return self.find_message(FindMessage::Close);
                }
                if self.go_to.is_some() {
                    return self.handle(Message::GoToLineDone, shared, events);
                }
                if matches!(self.dialog, Some(Dialog::Encoding { .. } | Dialog::Language { .. })) {
                    self.dialog = None;
                }
                Task::none()
            }
        }
    }

    pub fn command(&mut self, command: Command, shared: &mut Shared, events: &mut Vec<Event>) -> Task<Message> {
        if command == Command::Stop {
            return self.stop();
        }
        if self.dialog.is_some() {
            return Task::none();
        }
        let act = match command {
            Command::Find => Act::Find,
            Command::FindNext => Act::FindNext,
            Command::FindPrevious => Act::FindPrevious,
            Command::FindReplace => Act::FindReplace,
            Command::GoToLine => Act::GoToLine,
            Command::NextChange => Act::NextChange,
            Command::PreviousChange => Act::PrevChange,
            Command::NextPane => Act::NextPane,
            Command::PreviousPane => Act::PrevPane,
            Command::OpenExternal => Act::OpenExternal,
            Command::Undo => Act::Undo,
            Command::Redo => Act::Redo,
            Command::Refresh => Act::Refresh,
            Command::Save => Act::Save,
            Command::SaveAll => Act::SaveAll,
            Command::SaveAs => Act::SaveAs,
            Command::Filter => Act::Popup(Some(PopupKind::Filters)),
            Command::PreviousConflict => Act::PrevConflict,
            Command::NextConflict => Act::NextConflict,
            Command::PushLeft => Act::PushLeft,
            Command::PushRight => Act::PushRight,
            Command::PullLeft => Act::PullLeft,
            Command::PullRight => Act::PullRight,
            Command::CopyLeftUp => Act::CopyLeftUp,
            Command::CopyRightUp => Act::CopyRightUp,
            Command::CopyLeftDown => Act::CopyLeftDown,
            Command::CopyRightDown => Act::CopyRightDown,
            Command::DeleteChange => Act::DeleteChange,
            Command::TogglePanel => Act::ToggleOverview,
            Command::SwapPanes => Act::Swap,
            _ => return Task::none(),
        };
        let task = self.act(act, shared, events);
        self.after_message();
        task
    }

    /// Escape: close dialogs, popups and bars, or stop the comparison.
    fn stop(&mut self) -> Task<Message> {
        if self.popup.take().is_some() {
            return Task::none();
        }
        if let Some(dialog) = self.dialog.take() {
            if matches!(dialog, Dialog::ConfirmClose { .. } | Dialog::Resolve) {
                self.close_state = CloseState::Normal;
            }
            return Task::none();
        }
        if self.go_to.take().is_some() {
            return Task::none();
        }
        if self.find.visible {
            return self.find_message(FindMessage::Close);
        }
        if let Some(job) = self.job.take() {
            job.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
            self.diff_gen += 1;
            self.show_stopped();
        }
        Task::none()
    }

    pub fn request_close(&mut self, _shared: &mut Shared, events: &mut Vec<Event>) -> Task<Message> {
        self.close_state = CloseState::Closing;
        let modified = self.modified_flags();
        if !modified.iter().any(|m| *m) {
            return self.finish_close(events);
        }
        self.popup = None;
        self.dialog = Some(Dialog::ConfirmClose { save: modified });
        Task::none()
    }

    pub fn settings_changed(&mut self, shared: &Shared) -> Task<Message> {
        let settings = &shared.settings;
        self.metrics = Metrics::new(shared.mono_size());
        self.font = shared.mono_font();
        self.refresh_styles(shared);
        self.tab_width = settings.indent_width.max(1) as usize;
        self.insert_spaces = settings.insert_spaces_instead_of_tabs;
        for pane in &mut self.panes {
            pane.buffer.set_indentation(self.tab_width, self.insert_spaces);
        }
        self.show_overview_map = settings.show_overview_map;
        self.map_style = map_style(settings.overview_map_style);
        self.highlight_syntax = settings.highlight_syntax;
        self.detect_encodings = settings.detect_encodings.clone();
        let view_options = ViewOptions {
            show_line_numbers: settings.show_line_numbers,
            show_whitespace: settings.enable_space_drawer,
            highlight_current_line: settings.highlight_current_line,
            wrap: settings.wrap_mode != meld_config::WrapMode::None,
        };
        if view_options != self.settings_view_options {
            self.settings_view_options = view_options;
            self.view_options = view_options;
        }
        self.wrap_setting = settings.wrap_mode;
        self.refresh_wraps();
        self.refresh_decorations();

        // `on_text_filters_changed`: refresh only if the active filters
        // changed.
        let old: Vec<String> = self
            .text_filters
            .iter()
            .filter(|f| f.active)
            .map(|f| f.pattern.clone())
            .collect();
        let new_filters = text_filters_from(settings);
        let new: Vec<String> = new_filters
            .iter()
            .filter(|f| f.active)
            .map(|f| f.pattern.clone())
            .collect();
        let same_list = self.text_filters.len() == new_filters.len()
            && self
                .text_filters
                .iter()
                .zip(&new_filters)
                .all(|(a, b)| a.name == b.name && a.pattern == b.pattern);
        let mut refresh = false;
        if !same_list {
            self.text_filters = new_filters;
            refresh = old != new;
        }
        if settings.ignore_blank_lines != self.ignore_blank_lines {
            self.ignore_blank_lines = settings.ignore_blank_lines;
            refresh = true;
        }
        if refresh {
            return self.refresh_comparison();
        }
        Task::none()
    }

    pub fn subscription(&self) -> Subscription<Message> {
        let mut subs = vec![
            iced::event::listen_with(|event, status, _window| match event {
                iced::Event::Keyboard(keyboard::Event::KeyPressed { key, modifiers, .. }) => {
                    match (key.as_ref(), status) {
                        (keyboard::Key::Named(keyboard::key::Named::Escape), iced::event::Status::Captured) => {
                            Some(Message::CapturedEscape)
                        }
                        (keyboard::Key::Named(keyboard::key::Named::Insert), iced::event::Status::Ignored)
                            if modifiers.is_empty() =>
                        {
                            Some(Message::Act(Act::ToggleOverwrite))
                        }
                        _ => None,
                    }
                }
                _ => None,
            }),
            iced::time::every(std::time::Duration::from_secs(2)).map(|_| Message::CheckDisk),
        ];
        if self.is_loading() {
            subs.push(iced::time::every(std::time::Duration::from_millis(120)).map(|_| Message::Spin));
        }
        if !self.flashes.is_empty() {
            subs.push(iced::window::frames().map(Message::Tick));
        }
        Subscription::batch(subs)
    }

    pub fn toolbar<'a>(&'a self, shared: &'a Shared) -> Element<'a, Message> {
        self.toolbar_view(shared)
    }

    pub fn menu_items(&self, shared: &Shared) -> Vec<menu::Item<Message>> {
        self.menu(shared)
    }

    pub fn view<'a>(&'a self, shared: &'a Shared) -> Element<'a, Message> {
        self.main_view(shared)
    }
}

fn map_style(style: meld_config::OverviewMapStyle) -> MapStyle {
    match style {
        meld_config::OverviewMapStyle::Chunkmap => MapStyle::Chunks,
        meld_config::OverviewMapStyle::CompactSourcemap => MapStyle::CompactSource,
        meld_config::OverviewMapStyle::FullSourcemap => MapStyle::FullSource,
    }
}

/// The wrap mode of the panes: the setting's when wrapping is on
/// (word wrapping if the setting is "none"), as `wrap_mode_from_bool`.
fn wrap_mode(setting: meld_config::WrapMode, on: bool) -> WrapMode {
    if !on {
        return WrapMode::None;
    }
    match setting {
        meld_config::WrapMode::None | meld_config::WrapMode::Word => WrapMode::Word,
        meld_config::WrapMode::Char => WrapMode::Char,
        meld_config::WrapMode::WordChar => WrapMode::WordChar,
    }
}

fn text_filters_from(settings: &meld_config::Settings) -> Vec<TextFilter> {
    settings
        .text_filters
        .iter()
        .map(|f| TextFilter {
            name: f.name.clone(),
            pattern: f.pattern.clone(),
            active: f.active,
            regex: regex::Regex::new(&f.pattern).ok(),
        })
        .collect()
}

/// The tab title (`recompute_label`): shortened file names joined with
/// " — ", each with "*" when modified, or the comparison's tab label.
pub fn tab_title(labels: &[String], modified: &[bool], tab_label: Option<&str>) -> String {
    if let Some(label) = tab_label.filter(|l| !l.is_empty()) {
        return label.to_owned();
    }
    let mut names = meld_files::shorten_names(labels);
    for (name, modified) in names.iter_mut().zip(modified) {
        if *modified {
            name.push('*');
        }
    }
    names.join(" — ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn undo_group_restores_checkpoint() {
        let e = meld_widgets::Edit {
            start_line: 0,
            removed_lines: vec![],
            inserted_lines: vec!["x".into()],
            cursor_before: Default::default(),
            cursor_after: Default::default(),
        };
        let mut u = UndoSequence::new(0..2);
        u.checkpoint(0);
        u.checkpoint(1);
        u.begin_group();
        u.add_action(PaneEdit {
            pane: 0,
            edit: e.clone(),
        });
        u.add_action(PaneEdit {
            pane: 0,
            edit: e.clone(),
        });
        u.end_group();
        assert!(!u.checkpointed(0));
        assert!(u.checkpointed(1));
        u.undo();
        assert!(u.checkpointed(0));
    }

    #[test]
    fn titles() {
        let labels = vec!["/home/u/a/foo.txt".to_string(), "/home/u/b/foo.txt".to_string()];
        assert_eq!(tab_title(&labels, &[false, true], None), "[a] foo.txt — [b] foo.txt*");
        let labels = vec![
            "/x/one.py".to_string(),
            "/x/two.py".to_string(),
            "<unnamed>".to_string(),
        ];
        assert_eq!(
            tab_title(&labels, &[false, false, false], None),
            "/x/one.py — /x/two.py — <unnamed>"
        );
        assert_eq!(
            tab_title(&labels, &[true, true, true], Some("foo (working, repository)")),
            "foo (working, repository)"
        );
    }
}
