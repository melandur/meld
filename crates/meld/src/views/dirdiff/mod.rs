//! Folder comparison (`dirdiff.py`, `dirdiff.ui`, `dirdiff-actions.ui`,
//! the presentation parts of `tree.py`).
//!
//! Two or three folder trees side by side, sharing rows: row `n` is the
//! same entry in every pane, shown struck through and greyed where it
//! doesn't exist. Folders are scanned on a background thread
//! ([`scan`]), filling in the shared [`model::Tree`] as they arrive;
//! copying, deleting and re-reading rows also run off the UI thread.

mod format;
mod keys;
mod model;
mod scan;

use std::collections::{HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use iced::keyboard::{Key, Modifiers, key::Named};
use iced::widget::{
    Space, button, column, container, mouse_area, pin, row, scrollable, space, stack, text, tooltip,
};
use iced::{Background, Border, Color, Element, Font, Length, Point, Subscription, Task, Theme};
use meld_config::{FilterSetting, Settings, StatusColors};
use meld_files::dirdiff::{PaneStatus, Row, RowStatus, ScanIssues};
use meld_files::{
    CompareArgs, CopyOutcome, ExternalEditorSettings, FileOpError, FilterEntry, FilterKind,
    FolderComparison, FolderOptions, State, StatusFilters, TrashError,
};

use crate::command::Command;
use crate::comparison::Comparison;
use crate::shared::Shared;
use crate::ui::{self, BannerKind, icon, menu};
use crate::views::Event;

use model::{Kind, NodeId, Selection, Tree, Visible};
use scan::{ScanEvent, background};

/// Width of one indentation level.
const INDENT: f32 = 16.0;
/// Rows rendered above and below the visible area.
const OVERSCAN: usize = 8;
const DOUBLE_CLICK: Duration = Duration::from_millis(400);

const FOLDER_GLYPH: &str = "🗀";
const FILE_GLYPH: &str = "🗎";
const ERROR_GLYPH: &str = "⚠";
const MARK_GLYPH: &str = "☑";

/// The optional columns of `folder-columns`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Column {
    Size,
    Time,
    IsoTime,
    Permissions,
}

impl Column {
    fn from_settings(settings: &Settings) -> Vec<Column> {
        settings
            .folder_columns
            .iter()
            .filter(|c| c.visible)
            .filter_map(|c| match c.name.as_str() {
                "size" => Some(Column::Size),
                "modification time" => Some(Column::Time),
                "iso-time" => Some(Column::IsoTime),
                "permissions" => Some(Column::Permissions),
                other => {
                    log::warn!("Invalid column {other} in settings");
                    None
                }
            })
            .collect()
    }

    fn title(self) -> &'static str {
        match self {
            Column::Size => "Size",
            Column::Time => "Modification time",
            Column::IsoTime => "Modification time (ISO)",
            Column::Permissions => "Permissions",
        }
    }

    fn width(self) -> f32 {
        match self {
            Column::Size => 72.0,
            Column::Time => 176.0,
            Column::IsoTime => 150.0,
            Column::Permissions => 84.0,
        }
    }

    fn value(self, pane: &PaneStatus) -> String {
        match self {
            Column::Size => pane.size.map(format::byte_size).unwrap_or_default(),
            Column::Time | Column::IsoTime => pane
                .mtime
                .and_then(format::LocalTime::from_system_time)
                .map(|t| if self == Column::Time { t.format() } else { t.format_iso() })
                .unwrap_or_default(),
            Column::Permissions => pane.mode.map(format::file_mode).unwrap_or_default(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusKind {
    Same,
    New,
    Modified,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Nav {
    Up,
    Down,
    PageUp,
    PageDown,
    Home,
    End,
    PaneLeft,
    PaneRight,
    Expand,
    Collapse,
    ExpandAll,
    Parent,
    SelectAll,
}

/// A queued file operation.
#[derive(Debug, Clone)]
pub enum Op {
    Copy {
        node: NodeId,
        src: PathBuf,
        dst: PathBuf,
        replace: bool,
    },
    Delete {
        node: NodeId,
        path: PathBuf,
        permanently: bool,
    },
}

#[derive(Debug, Clone)]
pub enum OpResult {
    Copied { node: NodeId, outcome: CopyOutcome },
    /// `still_present`: the entry still exists in another pane.
    Deleted { node: NodeId, still_present: bool },
    FolderExists(Op),
    NeedsConfirmation(Op),
    Failed { title: String, message: String, stop: bool },
}

#[derive(Debug, Clone)]
pub enum Message {
    Scan(u64, ScanEvent),
    StatusesUpdated(u64, Vec<(NodeId, RowStatus)>),
    Browse(usize),
    Browsed(usize, Option<PathBuf>),
    RowPressed(usize, NodeId),
    RowRightPressed(usize, NodeId),
    ToggleExpand(NodeId),
    ContextMenu(Point),
    Nav(Nav, bool),
    Modifiers(Modifiers),
    Scrolled(scrollable::Viewport),
    Compare,
    Mark,
    CompareMarked,
    Copy(isize),
    Delete,
    CollapseRecursive,
    ExpandRecursive,
    OpenExternal,
    OpenedExternal(Vec<String>),
    CopyPaths,
    Swap,
    Refresh,
    Stop,
    Change(bool),
    ToggleFilterMenu,
    ClosePopup,
    ToggleStatus(StatusKind),
    ToggleNameFilter(usize),
    ToggleIgnoreCase,
    ToggleNormalize,
    ToggleTextFilters,
    OpFinished(u64, OpResult),
    DialogAnswer(bool),
    HideBanners,
}

struct Job {
    id: u64,
    base: NodeId,
    root: bool,
    cancel: Arc<AtomicBool>,
    differences: bool,
}

enum Dialog {
    ReplaceFolder { op: Op, name: String, parent: String },
    DeletePermanently { op: Op, path: String },
    Error { title: String, message: String },
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Popup {
    Filters,
    Context(Point),
}

#[derive(Default)]
struct Banners {
    identical: Option<String>,
    duplicate: Option<PathBuf>,
    /// Tree-wide scan problems, per pane: (heading, details).
    pane_errors: Vec<Option<(String, String)>>,
    cancelled: bool,
}

/// What to restore after a refresh.
struct Restore {
    expanded: HashSet<Vec<String>>,
    cursor: Option<Vec<String>>,
}

pub struct DirDiff {
    roots: Vec<PathBuf>,
    labels: Vec<String>,
    auto_compare: bool,
    ignore_case: bool,
    normalize_encoding: bool,
    apply_text_filters: bool,
    name_filters: Vec<FilterEntry>,
    text_filters: Vec<FilterEntry>,
    name_filter_settings: Vec<FilterSetting>,
    text_filter_settings: Vec<FilterSetting>,
    columns: Vec<Column>,
    comparison: FolderComparison,
    tree: Tree,
    visible: Vec<Visible>,
    visible_ids: Vec<NodeId>,
    sel: Selection,
    model_gen: u64,
    jobs: Vec<Job>,
    next_job: u64,
    progress: String,
    issues: ScanIssues,
    banners: Banners,
    dialog: Option<Dialog>,
    popup: Option<Popup>,
    ops: VecDeque<Op>,
    op_running: bool,
    modifiers: Modifiers,
    scroll_id: iced::widget::Id,
    scroll_y: f32,
    view_height: f32,
    row_height: f32,
    text_size: f32,
    last_click: Option<(usize, NodeId, Instant)>,
    marked: Option<(NodeId, usize)>,
    restore: Option<Restore>,
    last_folder: Option<PathBuf>,
}

fn filters_from(settings: &[FilterSetting], kind: FilterKind) -> Vec<FilterEntry> {
    settings
        .iter()
        .map(|f| FilterEntry::new(&f.name, f.active, &f.pattern, kind))
        .collect()
}

fn active_patterns(filters: &[FilterEntry]) -> Vec<&str> {
    filters.iter().filter(|f| f.active).map(|f| f.filter_string.as_str()).collect()
}

/// A root row to show until the scan has read the folders.
fn provisional_row(roots: &[PathBuf]) -> Row {
    let names = roots
        .iter()
        .map(|r| {
            r.file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| r.display().to_string())
        })
        .collect();
    Row {
        names,
        paths: roots.to_vec(),
        is_dir: true,
        status: RowStatus {
            state: State::Normal,
            panes: roots
                .iter()
                .map(|_| PaneStatus {
                    state: State::Normal,
                    exists: true,
                    is_dir: true,
                    size: None,
                    mtime: None,
                    mode: None,
                    symlink_target: None,
                })
                .collect(),
            different: false,
        },
    }
}

impl DirDiff {
    pub fn new(paths: Vec<PathBuf>, labels: Vec<String>, auto_compare: bool, shared: &mut Shared) -> (Self, Task<Message>) {
        let settings = &shared.settings;
        let n = paths.len().clamp(1, 3);
        let mut roots: Vec<PathBuf> = paths.into_iter().take(3).map(|p| absolute(&p)).collect();
        roots.resize(n, PathBuf::new());
        let mut labels = labels;
        labels.resize(n, String::new());
        let name_filters = filters_from(&settings.filename_filters, FilterKind::Shell);
        let text_filters = filters_from(&settings.text_filters, FilterKind::Regex);
        let mut tab = DirDiff {
            roots,
            labels,
            auto_compare,
            ignore_case: false,
            normalize_encoding: false,
            apply_text_filters: settings.folder_filter_text,
            name_filters,
            text_filters,
            name_filter_settings: settings.filename_filters.clone(),
            text_filter_settings: settings.text_filters.clone(),
            columns: Column::from_settings(settings),
            comparison: FolderComparison::new(FolderOptions::default(), Vec::new(), Vec::new()),
            tree: Tree::new(),
            visible: Vec::new(),
            visible_ids: Vec::new(),
            sel: Selection::default(),
            model_gen: 0,
            jobs: Vec::new(),
            next_job: 1,
            progress: String::new(),
            issues: ScanIssues::default(),
            banners: Banners::default(),
            dialog: None,
            popup: None,
            ops: VecDeque::new(),
            op_running: false,
            modifiers: Modifiers::empty(),
            scroll_id: iced::widget::Id::unique(),
            scroll_y: 0.0,
            view_height: 1200.0,
            row_height: 24.0,
            text_size: 13.0,
            last_click: None,
            marked: None,
            restore: None,
            last_folder: None,
        };
        tab.update_metrics(shared);
        tab.comparison = FolderComparison::new(
            tab.folder_options(&shared.settings),
            tab.name_filters.clone(),
            tab.text_filters.clone(),
        );
        let task = tab.start_scan(false);
        (tab, task)
    }

    fn panes(&self) -> usize {
        self.roots.len()
    }

    fn ready(&self) -> bool {
        self.roots.iter().all(|r| !r.as_os_str().is_empty())
    }

    fn busy(&self) -> bool {
        !self.jobs.is_empty()
    }

    fn display_names(&self) -> Vec<String> {
        self.roots
            .iter()
            .map(|r| {
                if r.as_os_str().is_empty() {
                    "No folder".to_string()
                } else {
                    r.display().to_string()
                }
            })
            .collect()
    }

    pub fn title(&self) -> String {
        let names = self.display_names();
        let short = meld_files::shorten_names(&names);
        short
            .iter()
            .zip(&self.labels)
            .map(|(name, label)| if label.is_empty() { name.clone() } else { label.clone() })
            .collect::<Vec<_>>()
            .join(" : ")
    }

    pub fn tooltip(&self) -> String {
        let names: Vec<String> = self
            .display_names()
            .into_iter()
            .zip(&self.labels)
            .map(|(name, label)| if label.is_empty() { name } else { label.clone() })
            .collect();
        format!("Folder comparison:\n{}", names.join("\n"))
    }

    pub fn unsaved(&self) -> Vec<String> {
        Vec::new()
    }

    fn update_metrics(&mut self, shared: &Shared) {
        self.text_size = (13.0 + shared.zoom).clamp(8.0, 40.0);
        self.row_height = (self.text_size * 1.75).round();
    }

    fn folder_options(&self, settings: &Settings) -> FolderOptions {
        FolderOptions {
            ignore_case: self.ignore_case,
            normalize_encoding: self.normalize_encoding,
            ignore_symlinks: settings.folder_ignore_symlinks,
            compare: CompareArgs {
                shallow_comparison: settings.folder_shallow_comparison,
                time_resolution_ns: i64::from(settings.folder_time_resolution),
                ignore_blank_lines: settings.ignore_blank_lines,
                apply_text_filters: self.apply_text_filters,
            },
            status_filters: StatusFilters::from_names(&settings.folder_status_filters),
        }
    }

    fn rebuild_comparison(&mut self, settings: &Settings) {
        self.comparison = FolderComparison::new(
            self.folder_options(settings),
            self.name_filters.clone(),
            self.text_filters.clone(),
        );
    }

    // --- Scanning ---------------------------------------------------------

    /// Starts scanning from the top (`set_locations()`), keeping the
    /// expanded folders and cursor when `keep_state` is set.
    fn start_scan(&mut self, keep_state: bool) -> Task<Message> {
        for job in self.jobs.drain(..) {
            job.cancel.store(true, Ordering::Relaxed);
        }
        self.restore = if keep_state && self.tree.root().is_some() {
            Some(Restore {
                expanded: self.tree.expanded_keys(),
                cursor: self.sel.cursor.filter(|&c| self.tree.get(c).is_some()).map(|c| self.tree.key(c)),
            })
        } else {
            None
        };
        self.banners = Banners::default();
        self.issues = ScanIssues::default();
        self.ops.clear();
        self.marked = None;
        self.model_gen += 1;
        let pane = self.sel.pane.min(self.panes() - 1);
        self.sel = Selection::default();
        self.sel.pane = pane;
        if !self.ready() {
            self.tree.clear();
            self.refresh_visible();
            return Task::none();
        }
        self.comparison.cache().clear();
        let root = self.tree.set_root(provisional_row(&self.roots));
        self.tree.node_mut(root).scanning = true;
        self.refresh_visible();
        let roots = self.roots.clone();
        self.spawn_job(root, roots, true)
    }

    fn spawn_job(&mut self, base: NodeId, roots: Vec<PathBuf>, root: bool) -> Task<Message> {
        let id = self.next_job;
        self.next_job += 1;
        let cancel = Arc::new(AtomicBool::new(false));
        self.jobs.push(Job {
            id,
            base,
            root,
            cancel: cancel.clone(),
            differences: false,
        });
        Task::run(scan::start(self.comparison.clone(), roots, cancel), move |event| {
            Message::Scan(id, event)
        })
    }

    /// Rescans one folder row (`recursively_update()`).
    fn rescan_row(&mut self, id: NodeId) -> Task<Message> {
        let Some(row) = self.tree.row(id) else {
            return Task::none();
        };
        let paths = row.paths.clone();
        self.tree.clear_children(id);
        self.tree.node_mut(id).scanning = true;
        self.refresh_visible();
        self.spawn_job(id, paths, false)
    }

    fn handle_scan(&mut self, job_id: u64, event: ScanEvent, events: &mut Vec<Event>) -> Task<Message> {
        let Some(index) = self.jobs.iter().position(|j| j.id == job_id) else {
            return Task::none();
        };
        let base = self.jobs[index].base;
        match event {
            ScanEvent::Base(row) => {
                if self.tree.get(base).is_some() {
                    if self.jobs[index].root {
                        self.tree.set_row(base, *row);
                    } else {
                        self.tree.set_status(base, row.status);
                    }
                }
            }
            ScanEvent::Items(items, current) => {
                self.progress = current;
                let items = Arc::try_unwrap(items).unwrap_or_else(|shared| (*shared).clone());
                let mut differences = false;
                for item in items {
                    let Some(id) = self.tree.resolve(base, &item.tree_path) else {
                        continue;
                    };
                    let mut scan = item.scan;
                    self.issues.invalid_filenames.append(&mut scan.invalid_filenames);
                    self.issues.shadowed.append(&mut scan.shadowed);
                    self.issues.whitespace.append(&mut scan.whitespace);
                    differences |= self.tree.apply_scan(id, scan);
                }
                self.jobs[index].differences |= differences;
                self.refresh_visible();
            }
            ScanEvent::Done { cancelled } => {
                let job = self.jobs.remove(index);
                if self.tree.get(base).is_some() {
                    self.tree.node_mut(base).scanning = false;
                }
                if job.root {
                    self.finish_root_scan(cancelled, job.differences, events);
                }
                if self.jobs.is_empty() {
                    self.progress.clear();
                }
                self.refresh_visible();
            }
        }
        Task::none()
    }

    fn finish_root_scan(&mut self, cancelled: bool, differences: bool, events: &mut Vec<Event>) {
        if cancelled {
            self.banners.cancelled = true;
            return;
        }
        let mut seen = HashSet::new();
        let duplicate = self
            .roots
            .iter()
            .map(|r| meld_files::iohelpers::canonicalize_lexically(r))
            .find(|r| !seen.insert(r.clone()));
        if !self.issues.is_empty() {
            self.banners.pane_errors = self.tree_wide_errors();
        } else if let Some(dup) = duplicate {
            self.banners.duplicate = Some(dup);
        } else if !differences {
            self.banners.identical = Some(self.identical_note());
        }

        if let Some(restore) = self.restore.take() {
            for id in self.tree.all_rows() {
                if self.tree.has_children(id) && restore.expanded.contains(&self.tree.key(id)) {
                    self.tree.set_expanded(id, true);
                }
            }
            if let Some(key) = restore.cursor
                && let Some(id) = self.tree.all_rows().into_iter().find(|&id| self.tree.key(id) == key)
            {
                self.tree.expand_to(id);
                self.sel.set(id);
            }
        }
        if self.sel.cursor.is_none()
            && let Some(root) = self.tree.root()
        {
            self.sel.set(root);
        }

        if std::mem::take(&mut self.auto_compare) {
            for id in self.tree.rows_in_states(&[State::Modified, State::Conflict]) {
                if let Some(comparison) = self.comparison_for(id) {
                    events.push(Event::Open(comparison));
                }
            }
        }
    }

    fn identical_note(&self) -> String {
        let shallow = self.comparison.options.compare.shallow_comparison;
        let mut secondary = vec![if shallow {
            "Scanned files in folders appear identical, but contents have not been scanned."
        } else {
            "Contents of scanned files in folders are identical."
        }];
        if self.name_filters.iter().any(|f| f.active) {
            secondary.push("File filters are in use, so not all files have been scanned.");
        }
        if !shallow && self.text_filters.iter().any(|f| f.active) {
            secondary.push("Text filters are in use and may be masking content differences.");
        }
        secondary.join(" ")
    }

    /// `_show_tree_wide_errors()`
    fn tree_wide_errors(&self) -> Vec<Option<(String, String)>> {
        let n = self.panes();
        let mut invalid = vec![Vec::new(); n];
        for issue in &self.issues.invalid_filenames {
            if let Some(list) = invalid.get_mut(issue.pane) {
                list.push(issue.root.join(&issue.name).display().to_string());
            }
        }
        let mut shadowed = vec![Vec::new(); n];
        for entry in &self.issues.shadowed {
            if let Some(list) = shadowed.get_mut(entry.pane) {
                list.push(format!(
                    "“{}” hidden by “{}”",
                    entry.root.join(&entry.hidden).display(),
                    entry.root.join(&entry.by).display()
                ));
            }
        }
        let mut whitespace = vec![Vec::new(); n];
        for issue in &self.issues.whitespace {
            for list in whitespace.iter_mut() {
                list.push(issue.root.join(&issue.name).display().to_string());
            }
        }
        (0..n)
            .map(|pane| {
                let kinds = [
                    (
                        &invalid[pane],
                        "Files with invalid encodings found",
                        "Some files were in an incorrect encoding. The names are something like:",
                    ),
                    (
                        &shadowed[pane],
                        "Files hidden by case insensitive comparison",
                        "You are running a case insensitive comparison on a case sensitive filesystem. The following files in this folder are hidden:",
                    ),
                    (
                        &whitespace[pane],
                        "Files had mismatched leading or trailing whitespace",
                        "This comparison found some files that differed only in leading or trailing whitespace. Their names appear as:",
                    ),
                ];
                let present: Vec<_> = kinds.iter().filter(|(list, _, _)| !list.is_empty()).collect();
                if present.is_empty() {
                    return None;
                }
                let header = if present.len() == 1 {
                    present[0].1.to_string()
                } else {
                    "Multiple errors occurred while scanning this folder".to_string()
                };
                let mut messages = Vec::new();
                for (list, _, secondary) in &present {
                    messages.push(secondary.to_string());
                    messages.push(list.join("\n"));
                    messages.push(String::new());
                }
                Some((header, messages.join("\n").trim().to_string()))
            })
            .collect()
    }

    /// Another tab saved `path`: refresh the rows showing it and their
    /// folders (`on_file_changed()`).
    pub fn file_changed(&mut self, path: &std::path::Path) -> Task<Message> {
        let ids: Vec<NodeId> = self
            .tree
            .all_rows()
            .into_iter()
            .filter(|&id| self.tree.row(id).is_some_and(|row| row.paths.iter().any(|p| p == path)))
            .flat_map(|id| self.tree.with_ancestors(id))
            .collect();
        self.refresh_rows(ids)
    }

    /// Re-reads the state of rows (`_update_item_state()`).
    fn refresh_rows(&self, ids: Vec<NodeId>) -> Task<Message> {
        let rows: Vec<(NodeId, Vec<PathBuf>)> = ids
            .into_iter()
            .filter_map(|id| Some((id, self.tree.row(id)?.paths.clone())))
            .collect();
        if rows.is_empty() {
            return Task::none();
        }
        let comparison = self.comparison.clone();
        let generation = self.model_gen;
        Task::perform(
            background(move || {
                rows.into_iter()
                    .map(|(id, paths)| (id, comparison.row_status(&paths)))
                    .collect::<Vec<_>>()
            }),
            move |statuses| Message::StatusesUpdated(generation, statuses.unwrap_or_default()),
        )
    }

    fn refresh_visible(&mut self) {
        let previous = std::mem::take(&mut self.visible_ids);
        self.visible = self.tree.flatten();
        self.visible_ids = self.visible.iter().map(|v| v.id).collect();
        // A cursor hidden by collapsing moves to the collapsed row.
        if let Some(cursor) = self.sel.cursor
            && self.tree.get(cursor).is_some()
            && !self.visible_ids.contains(&cursor)
            && let Some(ancestor) = self
                .tree
                .with_ancestors(cursor)
                .into_iter()
                .find(|a| self.visible_ids.contains(a))
        {
            self.sel.set(ancestor);
        }
        self.sel.retain_visible(&self.visible_ids, &previous);
    }

    // --- Selection and navigation ---------------------------------------

    fn selected(&self) -> Vec<NodeId> {
        self.sel.selected(&self.visible_ids)
    }

    /// Whether the selection can be copied, deleted or opened: every
    /// selected row exists in the focused pane and no scan is running.
    fn selection_valid(&self) -> bool {
        let selected = self.selected();
        !selected.is_empty()
            && !self.busy()
            && selected.iter().all(|&id| {
                matches!(&self.tree.node(id).kind, Kind::Entry(row)
                    if row.status.panes.get(self.sel.pane).is_some_and(|p| p.exists && p.state != State::Error))
            })
    }

    fn scroll_to_row(&mut self, id: NodeId) -> Task<Message> {
        let Some(index) = self.visible_ids.iter().position(|&r| r == id) else {
            return Task::none();
        };
        let top = index as f32 * self.row_height;
        let target = if top < self.scroll_y {
            top
        } else if top + self.row_height > self.scroll_y + self.view_height {
            top + self.row_height - self.view_height
        } else {
            return Task::none();
        };
        self.scroll_y = target.max(0.0);
        iced::widget::operation::scroll_to(
            self.scroll_id.clone(),
            iced::widget::operation::AbsoluteOffset {
                x: None,
                y: Some(self.scroll_y),
            },
        )
    }

    fn navigate(&mut self, nav: Nav, extend: bool) -> Task<Message> {
        let rows = self.visible_ids.clone();
        let page = ((self.view_height / self.row_height) as isize - 1).max(1);
        match nav {
            Nav::Up => self.sel.move_by(&rows, -1, extend),
            Nav::Down => self.sel.move_by(&rows, 1, extend),
            Nav::PageUp => self.sel.move_by(&rows, -page, extend),
            Nav::PageDown => self.sel.move_by(&rows, page, extend),
            Nav::Home => self.sel.move_to(&rows, 0, extend),
            Nav::End => self.sel.move_to(&rows, rows.len().saturating_sub(1), extend),
            Nav::PaneLeft | Nav::PaneRight => {
                let target = self.sel.pane as isize + if nav == Nav::PaneRight { 1 } else { -1 };
                if (0..self.panes() as isize).contains(&target) {
                    self.sel.pane = target as usize;
                }
                return Task::none();
            }
            Nav::Expand | Nav::Collapse | Nav::ExpandAll => {
                if let Some(cursor) = self.sel.cursor {
                    match nav {
                        Nav::Expand => self.tree.set_expanded(cursor, true),
                        Nav::Collapse => self.tree.set_expanded(cursor, false),
                        _ => self.tree.set_expanded_recursive(cursor, true),
                    }
                    self.refresh_visible();
                }
                return Task::none();
            }
            Nav::Parent => {
                if let Some(parent) = self.sel.cursor.and_then(|c| self.tree.node(c).parent) {
                    self.sel.set(parent);
                }
            }
            Nav::SelectAll => {
                self.sel.select_all(&rows);
                return Task::none();
            }
        }
        match self.sel.cursor {
            Some(cursor) => self.scroll_to_row(cursor),
            None => Task::none(),
        }
    }

    fn change_focus_pane(&mut self, delta: isize) {
        let n = self.panes() as isize;
        self.sel.pane = ((self.sel.pane as isize + delta).rem_euclid(n)) as usize;
    }

    /// `next_diff()`
    fn go_to_change(&mut self, forward: bool) -> Task<Message> {
        match self.tree.find_change(self.sel.cursor, forward) {
            Some(id) => {
                self.tree.expand_to(id);
                self.refresh_visible();
                self.sel.set(id);
                self.scroll_to_row(id)
            }
            None => Task::none(),
        }
    }

    // --- Opening comparisons ---------------------------------------------

    /// `run_diff_from_iter()`: compares a row's files, or its folders.
    fn comparison_for(&self, id: NodeId) -> Option<Comparison> {
        let row = self.tree.row(id)?;
        let first = row.status.panes.iter().find(|p| p.exists)?;
        if first.is_dir {
            let paths = row
                .paths
                .iter()
                .zip(&row.status.panes)
                .map(|(path, pane)| if pane.exists && pane.is_dir { path.clone() } else { PathBuf::new() })
                .collect();
            Some(Comparison::Folders {
                paths,
                labels: Vec::new(),
                auto_compare: false,
            })
        } else {
            let paths = row
                .paths
                .iter()
                .zip(&row.status.panes)
                .map(|(path, pane)| (pane.exists && !pane.is_dir).then(|| path.clone()))
                .collect();
            Comparison::from_paths(paths, false).ok()
        }
    }

    /// `on_treeview_row_activated()`: files are compared, folders expand
    /// or collapse; a missing entry uses the neighbouring pane.
    fn activate(&mut self, id: NodeId, pane: usize, events: &mut Vec<Event>) {
        let Some(row) = self.tree.row(id) else {
            return;
        };
        const ORDERING: [[usize; 3]; 3] = [[0, 1, 2], [1, 2, 0], [2, 1, 0]];
        let n = self.panes();
        let Some(pane) = ORDERING[pane.min(2)]
            .iter()
            .copied()
            .find(|&p| p < n && row.status.panes.get(p).is_some_and(|s| s.exists))
        else {
            return;
        };
        if row.status.panes[pane].is_dir {
            let expanded = self.tree.node(id).expanded;
            self.tree.set_expanded(id, !expanded);
            self.refresh_visible();
        } else if let Some(comparison) = self.comparison_for(id) {
            events.push(Event::Open(comparison));
        }
    }

    fn compare_marked(&self) -> Option<Comparison> {
        let (marked, marked_pane) = self.marked?;
        let pane = self.sel.pane;
        if pane == marked_pane {
            return None;
        }
        let selected = *self.selected().first()?;
        let marked_path = self.tree.row(marked)?.paths.get(marked_pane)?.clone();
        let selected_path = self.tree.row(selected)?.paths.get(pane)?.clone();
        let ordered = if pane < marked_pane {
            [selected_path, marked_path]
        } else {
            [marked_path, selected_path]
        };
        let paths = ordered.into_iter().filter(|p| p.exists()).map(Some).collect();
        Comparison::from_paths(paths, false).ok()
    }

    // --- File operations --------------------------------------------------

    fn queue_copy(&mut self, direction: isize) -> Task<Message> {
        let src = self.sel.pane;
        let dst = src as isize + direction;
        if !(0..self.panes() as isize).contains(&dst) || !self.selection_valid() {
            return Task::none();
        }
        let dst = dst as usize;
        for id in self.selected().into_iter().rev() {
            let Some(row) = self.tree.row(id) else { continue };
            self.ops.push_back(Op::Copy {
                node: id,
                src: row.paths[src].clone(),
                dst: row.paths[dst].clone(),
                replace: false,
            });
        }
        self.run_ops()
    }

    fn queue_delete(&mut self) -> Task<Message> {
        if !self.selection_valid() {
            return Task::none();
        }
        let pane = self.sel.pane;
        for id in self.selected().into_iter().rev() {
            if Some(id) == self.tree.root() {
                continue;
            }
            let Some(row) = self.tree.row(id) else { continue };
            self.ops.push_back(Op::Delete {
                node: id,
                path: row.paths[pane].clone(),
                permanently: false,
            });
        }
        self.run_ops()
    }

    fn run_ops(&mut self) -> Task<Message> {
        if self.op_running || self.dialog.is_some() {
            return Task::none();
        }
        let Some(op) = self.ops.pop_front() else {
            return Task::none();
        };
        self.op_running = true;
        let others: Vec<PathBuf> = match &op {
            Op::Delete { node, path, .. } => self
                .tree
                .row(*node)
                .map(|r| r.paths.iter().filter(|p| *p != path).cloned().collect())
                .unwrap_or_default(),
            Op::Copy { .. } => Vec::new(),
        };
        let generation = self.model_gen;
        Task::perform(background(move || execute(op, &others)), move |result| {
            Message::OpFinished(
                generation,
                result.unwrap_or(OpResult::Failed {
                    title: "Error".into(),
                    message: "The operation was interrupted".into(),
                    stop: true,
                }),
            )
        })
    }

    fn op_finished(&mut self, generation: u64, result: OpResult) -> Task<Message> {
        self.op_running = false;
        if generation != self.model_gen {
            return self.run_ops();
        }
        let mut tasks = Vec::new();
        match result {
            OpResult::Copied { node, outcome } => match outcome {
                // `file_created()`: the row and its folders change state
                CopyOutcome::File => tasks.push(self.refresh_rows(self.tree.with_ancestors(node))),
                CopyOutcome::Folder => {
                    tasks.push(self.rescan_row(node));
                    let ancestors = self.tree.with_ancestors(node).into_iter().skip(1).collect();
                    tasks.push(self.refresh_rows(ancestors));
                }
                CopyOutcome::Nothing => {}
            },
            OpResult::Deleted { node, still_present } => {
                // `file_deleted()`
                if still_present {
                    tasks.push(self.refresh_rows(self.tree.with_ancestors(node)));
                } else if self.tree.get(node).is_some() {
                    let ancestors: Vec<NodeId> = self.tree.with_ancestors(node).into_iter().skip(1).collect();
                    self.tree.remove(node);
                    if self.marked.is_some_and(|(m, _)| self.tree.get(m).is_none()) {
                        self.marked = None;
                    }
                    self.refresh_visible();
                    tasks.push(self.refresh_rows(ancestors));
                }
            }
            OpResult::FolderExists(op) => {
                if let Op::Copy { dst, .. } = &op {
                    let name = dst.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                    let parent = dst.parent().map(|p| p.display().to_string()).unwrap_or_default();
                    self.dialog = Some(Dialog::ReplaceFolder { op, name, parent });
                }
            }
            OpResult::NeedsConfirmation(op) => {
                if let Op::Delete { path, .. } = &op {
                    let path = path.display().to_string();
                    self.dialog = Some(Dialog::DeletePermanently { op, path });
                }
            }
            OpResult::Failed { title, message, stop } => {
                if stop {
                    self.ops.retain(|op| !matches!(op, Op::Delete { .. }));
                }
                self.dialog = Some(Dialog::Error { title, message });
            }
        }
        tasks.push(self.run_ops());
        Task::batch(tasks)
    }

    fn answer_dialog(&mut self, yes: bool) -> Task<Message> {
        match self.dialog.take() {
            Some(Dialog::ReplaceFolder { op, .. }) => {
                if yes && let Op::Copy { node, src, dst, .. } = op {
                    self.ops.push_front(Op::Copy {
                        node,
                        src,
                        dst,
                        replace: true,
                    });
                }
            }
            Some(Dialog::DeletePermanently { op, .. }) => {
                if yes && let Op::Delete { node, path, .. } = op {
                    self.ops.push_front(Op::Delete {
                        node,
                        path,
                        permanently: true,
                    });
                } else {
                    self.ops.retain(|op| !matches!(op, Op::Delete { .. }));
                }
            }
            Some(Dialog::Error { .. }) | None => {}
        }
        self.run_ops()
    }

    fn open_external(&self, shared: &Shared) -> Task<Message> {
        if !self.selection_valid() {
            return Task::none();
        }
        let pane = self.sel.pane;
        let paths: Vec<PathBuf> = self
            .selected()
            .into_iter()
            .filter_map(|id| self.tree.row(id).map(|r| r.paths[pane].clone()))
            .collect();
        let settings = ExternalEditorSettings {
            use_system_editor: shared.settings.use_system_editor,
            custom_editor_command: shared.settings.custom_editor_command.clone(),
        };
        Task::perform(
            background(move || {
                paths
                    .iter()
                    .filter_map(|path| {
                        let result = meld_files::open_external_command(path, 1, &settings, false)
                            .map_err(|e| e.to_string())
                            .and_then(|argv| meld_files::spawn_detached(&argv).map_err(|e| e.to_string()));
                        result.err()
                    })
                    .collect::<Vec<_>>()
            }),
            |errors| Message::OpenedExternal(errors.unwrap_or_default()),
        )
    }

    fn copy_paths(&self) -> Task<Message> {
        let pane = self.sel.pane;
        let paths: Vec<String> = self
            .selected()
            .into_iter()
            .filter_map(|id| self.tree.row(id))
            .filter(|r| r.status.panes.get(pane).is_some_and(|p| p.exists))
            .map(|r| r.paths[pane].display().to_string())
            .collect();
        if paths.is_empty() {
            return Task::none();
        }
        iced::clipboard::write(paths.join("\n"))
    }

    fn expand_selected(&mut self, expanded: bool) {
        let selected = self.selected();
        let targets = if selected.is_empty() {
            self.tree.root().into_iter().collect()
        } else {
            selected
        };
        for id in targets {
            self.tree.set_expanded_recursive(id, expanded);
        }
        if let Some(root) = self.tree.root() {
            self.tree.set_expanded(root, true);
        }
        self.refresh_visible();
    }

    // --- Update -----------------------------------------------------------

    pub fn update(&mut self, message: Message, shared: &mut Shared, events: &mut Vec<Event>) -> Task<Message> {
        match message {
            Message::Scan(job, event) => return self.handle_scan(job, event, events),
            Message::StatusesUpdated(generation, statuses) => {
                if generation == self.model_gen {
                    for (id, status) in statuses {
                        if self.tree.get(id).is_some() {
                            self.tree.set_status(id, status);
                        }
                    }
                }
            }
            Message::Browse(pane) => {
                let mut dialog = rfd::AsyncFileDialog::new().set_title("Select Folder");
                let start = self
                    .roots
                    .get(pane)
                    .filter(|r| !r.as_os_str().is_empty())
                    .cloned()
                    .or_else(|| self.last_folder.clone())
                    .or_else(|| std::env::var_os("HOME").map(PathBuf::from));
                if let Some(start) = start {
                    dialog = dialog.set_directory(start);
                }
                return Task::perform(dialog.pick_folder(), move |handle| {
                    Message::Browsed(pane, handle.map(|h| h.path().to_path_buf()))
                });
            }
            Message::Browsed(pane, path) => {
                let Some(path) = path else {
                    return Task::none();
                };
                if pane >= self.panes() {
                    return Task::none();
                }
                self.last_folder = path.parent().map(Path::to_path_buf);
                self.roots[pane] = path;
                self.labels[pane].clear();
                if self.ready() {
                    events.push(Event::Recent(Comparison::Folders {
                        paths: self.roots.clone(),
                        labels: Vec::new(),
                        auto_compare: false,
                    }));
                }
                return self.start_scan(false);
            }
            Message::RowPressed(pane, id) => {
                self.popup = None;
                if pane != self.sel.pane {
                    self.sel.clear();
                    self.sel.pane = pane;
                }
                let plain = !self.modifiers.command() && !self.modifiers.shift();
                let double = plain
                    && self
                        .last_click
                        .is_some_and(|(p, r, t)| p == pane && r == id && t.elapsed() < DOUBLE_CLICK);
                if double {
                    self.last_click = None;
                    self.sel.set(id);
                    self.activate(id, pane, events);
                } else {
                    if let Some(index) = self.visible_ids.iter().position(|&r| r == id) {
                        self.sel
                            .click(&self.visible_ids, index, self.modifiers.command(), self.modifiers.shift());
                    }
                    self.last_click = Some((pane, id, Instant::now()));
                }
            }
            Message::RowRightPressed(pane, id) => {
                if pane != self.sel.pane || !self.sel.is_selected(id) {
                    self.sel.pane = pane;
                    self.sel.set(id);
                }
            }
            Message::ContextMenu(point) => {
                if !self.sel.is_empty() {
                    self.popup = Some(Popup::Context(point));
                }
            }
            Message::ToggleExpand(id) => {
                let expanded = self.tree.node(id).expanded;
                self.tree.set_expanded(id, !expanded);
                self.refresh_visible();
            }
            Message::Nav(nav, extend) => {
                if self.dialog.is_some() {
                    return Task::none();
                }
                self.popup = None;
                return self.navigate(nav, extend);
            }
            Message::Modifiers(modifiers) => self.modifiers = modifiers,
            Message::Scrolled(viewport) => {
                self.scroll_y = viewport.absolute_offset().y;
                self.view_height = viewport.bounds().height.max(self.row_height);
            }
            Message::Compare => {
                self.popup = None;
                for id in self.selected() {
                    if let Some(comparison) = self.comparison_for(id) {
                        events.push(Event::Open(comparison));
                    }
                }
            }
            Message::Mark => {
                self.popup = None;
                if let Some(&id) = self.selected().first() {
                    self.marked = Some((id, self.sel.pane));
                }
            }
            Message::CompareMarked => {
                self.popup = None;
                if let Some(comparison) = self.compare_marked() {
                    events.push(Event::Open(comparison));
                }
            }
            Message::Copy(direction) => {
                self.popup = None;
                return self.queue_copy(direction);
            }
            Message::Delete => {
                self.popup = None;
                return self.queue_delete();
            }
            Message::CollapseRecursive => {
                self.popup = None;
                self.expand_selected(false);
            }
            Message::ExpandRecursive => {
                self.popup = None;
                self.expand_selected(true);
            }
            Message::OpenExternal => {
                self.popup = None;
                return self.open_external(shared);
            }
            Message::OpenedExternal(errors) => {
                if !errors.is_empty() {
                    events.push(Event::Notify(errors.join("\n")));
                }
            }
            Message::CopyPaths => {
                self.popup = None;
                return self.copy_paths();
            }
            Message::Swap => {
                self.popup = None;
                if self.panes() == 2 {
                    self.roots.reverse();
                    self.labels.reverse();
                    return self.start_scan(true);
                }
            }
            Message::Refresh => {
                self.popup = None;
                return self.start_scan(true);
            }
            Message::Stop => {
                for job in &self.jobs {
                    job.cancel.store(true, Ordering::Relaxed);
                }
            }
            Message::Change(forward) => return self.go_to_change(forward),
            Message::ToggleFilterMenu => {
                self.popup = if self.popup == Some(Popup::Filters) {
                    None
                } else {
                    Some(Popup::Filters)
                };
            }
            Message::ClosePopup => self.popup = None,
            Message::ToggleStatus(kind) => {
                // The status filters are a setting shared by every folder
                // comparison; the settings change rescans.
                let mut filters = StatusFilters::from_names(&shared.settings.folder_status_filters);
                match kind {
                    StatusKind::Same => filters.normal = !filters.normal,
                    StatusKind::New => filters.new = !filters.new,
                    StatusKind::Modified => filters.modified = !filters.modified,
                }
                shared.settings.folder_status_filters = filters.names().into_iter().map(String::from).collect();
                events.push(Event::SettingsChanged);
            }
            Message::ToggleNameFilter(index) => {
                if let Some(filter) = self.name_filters.get_mut(index) {
                    let active = filter.active;
                    filter.set_active(!active);
                    self.rebuild_comparison(&shared.settings);
                    return self.start_scan(true);
                }
            }
            Message::ToggleIgnoreCase | Message::ToggleNormalize | Message::ToggleTextFilters => {
                match message {
                    Message::ToggleIgnoreCase => self.ignore_case = !self.ignore_case,
                    Message::ToggleNormalize => self.normalize_encoding = !self.normalize_encoding,
                    _ => self.apply_text_filters = !self.apply_text_filters,
                }
                self.rebuild_comparison(&shared.settings);
                return self.start_scan(true);
            }
            Message::OpFinished(generation, result) => return self.op_finished(generation, result),
            Message::DialogAnswer(yes) => return self.answer_dialog(yes),
            Message::HideBanners => self.banners = Banners::default(),
        }
        Task::none()
    }

    pub fn command(&mut self, command: Command, shared: &mut Shared, events: &mut Vec<Event>) -> Task<Message> {
        if self.dialog.is_some() {
            return match command {
                Command::Stop => self.answer_dialog(false),
                Command::Activate => self.answer_dialog(true),
                _ => Task::none(),
            };
        }
        match command {
            Command::Stop => {
                if self.popup.is_some() {
                    self.popup = None;
                    Task::none()
                } else {
                    self.update(Message::Stop, shared, events)
                }
            }
            Command::Activate => {
                if let Some(cursor) = self.sel.cursor {
                    self.activate(cursor, self.sel.pane, events);
                }
                Task::none()
            }
            Command::PushLeft => self.update(Message::Copy(-1), shared, events),
            Command::PushRight => self.update(Message::Copy(1), shared, events),
            Command::Delete => self.update(Message::Delete, shared, events),
            Command::OpenExternal => self.update(Message::OpenExternal, shared, events),
            Command::NextChange => self.go_to_change(true),
            Command::PreviousChange => self.go_to_change(false),
            Command::SwapPanes => self.update(Message::Swap, shared, events),
            Command::Refresh => self.update(Message::Refresh, shared, events),
            Command::Filter => self.update(Message::ToggleFilterMenu, shared, events),
            Command::NextPane => {
                self.change_focus_pane(1);
                Task::none()
            }
            Command::PreviousPane => {
                self.change_focus_pane(-1);
                Task::none()
            }
            _ => Task::none(),
        }
    }

    pub fn request_close(&mut self, _shared: &mut Shared, events: &mut Vec<Event>) -> Task<Message> {
        for job in &self.jobs {
            job.cancel.store(true, Ordering::Relaxed);
        }
        events.push(Event::Close { status: 0 });
        Task::none()
    }

    pub fn settings_changed(&mut self, shared: &Shared) -> Task<Message> {
        let settings = &shared.settings;
        self.update_metrics(shared);
        self.columns = Column::from_settings(settings);

        let mut filters_changed = false;
        if settings.filename_filters != self.name_filter_settings {
            let new = filters_from(&settings.filename_filters, FilterKind::Shell);
            filters_changed |= active_patterns(&new) != active_patterns(&self.name_filters);
            self.name_filters = new;
            self.name_filter_settings = settings.filename_filters.clone();
        }
        if settings.text_filters != self.text_filter_settings {
            let new = filters_from(&settings.text_filters, FilterKind::Regex);
            filters_changed |= active_patterns(&new) != active_patterns(&self.text_filters);
            self.text_filters = new;
            self.text_filter_settings = settings.text_filters.clone();
        }
        let options = self.folder_options(settings);
        if filters_changed || options != self.comparison.options {
            self.rebuild_comparison(settings);
            return self.start_scan(true);
        }
        Task::none()
    }

    pub fn subscription(&self) -> Subscription<Message> {
        Subscription::none()
    }

    // --- View -------------------------------------------------------------

    pub fn toolbar<'a>(&'a self, _shared: &'a Shared) -> Element<'a, Message> {
        let valid = self.selection_valid();
        let pane = self.sel.pane;
        let n = self.panes();
        let mut r = row![space::horizontal()].spacing(2).align_y(iced::Center);
        r = r.push(ui::tool_button(
            "⇔",
            "Compare selected (Return)",
            (!self.sel.is_empty()).then_some(Message::Compare),
        ));
        r = r.push(ui::tool_button(
            icon::LEFT,
            "Copy to left (Alt+Left)",
            (valid && pane > 0).then_some(Message::Copy(-1)),
        ));
        r = r.push(ui::tool_button(
            icon::RIGHT,
            "Copy to right (Alt+Right)",
            (valid && pane + 1 < n).then_some(Message::Copy(1)),
        ));
        r = r.push(ui::tool_button("✖", "Delete selected (Delete)", valid.then_some(Message::Delete)));
        r = r.push(ui::tool_button(icon::FILTER, "Filters (F8)", Some(Message::ToggleFilterMenu)));
        if self.busy() {
            r = r.push(ui::tool_button(icon::STOP, "Stop scanning (Escape)", Some(Message::Stop)));
        } else {
            r = r.push(ui::tool_button(icon::REFRESH, "Refresh (Ctrl+R)", Some(Message::Refresh)));
        }
        r.into()
    }

    pub fn menu_items(&self, _shared: &Shared) -> Vec<menu::Item<Message>> {
        self.action_items(true)
    }

    fn action_items(&self, full: bool) -> Vec<menu::Item<Message>> {
        fn item(label: &str, shortcut: Option<&'static str>, message: Option<Message>) -> menu::Item<Message> {
            menu::Item::Action {
                label: label.to_string(),
                shortcut,
                message,
            }
        }
        let valid = self.selection_valid();
        let has_selection = !self.sel.is_empty();
        let pane = self.sel.pane;
        let n = self.panes();
        let single_folder = self.sel.len() == 1
            && self.selected().first().is_some_and(|&id| self.tree.has_children(id));
        let mut items = vec![
            item("Compare Selected Files", Some("Return"), has_selection.then_some(Message::Compare)),
            item("Mark Selected File", None, has_selection.then_some(Message::Mark)),
            item(
                "Compare with Marked File",
                None,
                (has_selection && self.marked.is_some_and(|(_, p)| p != pane)).then_some(Message::CompareMarked),
            ),
            menu::Item::Separator,
            item("Copy to Left", Some("Alt+Left"), (valid && pane > 0).then_some(Message::Copy(-1))),
            item("Copy to Right", Some("Alt+Right"), (valid && pane + 1 < n).then_some(Message::Copy(1))),
            item("Delete Selected", Some("Delete"), valid.then_some(Message::Delete)),
            menu::Item::Separator,
            item("Collapse Recursively", None, single_folder.then_some(Message::CollapseRecursive)),
            item("Expand Recursively", None, single_folder.then_some(Message::ExpandRecursive)),
            menu::Item::Separator,
            item("Open Externally", Some("Ctrl+Shift+O"), valid.then_some(Message::OpenExternal)),
            item("Copy File Paths", None, has_selection.then_some(Message::CopyPaths)),
        ];
        if full {
            items.extend([
                menu::Item::Separator,
                item("Previous Change", Some("Alt+Up"), Some(Message::Change(false))),
                item("Next Change", Some("Alt+Down"), Some(Message::Change(true))),
                item("Swap Panes", Some("Alt+\\"), (n == 2).then_some(Message::Swap)),
                item("Refresh", Some("Ctrl+R"), Some(Message::Refresh)),
                item("Filters", Some("F8"), Some(Message::ToggleFilterMenu)),
            ]);
        }
        items
    }

    fn filter_items(&self, settings_filters: StatusFilters) -> Vec<menu::Item<Message>> {
        let mut items = vec![
            menu::Item::Heading("File Status".into()),
            menu::Item::toggle("Same", settings_filters.normal, Message::ToggleStatus(StatusKind::Same)),
            menu::Item::toggle("New", settings_filters.new, Message::ToggleStatus(StatusKind::New)),
            menu::Item::toggle("Modified", settings_filters.modified, Message::ToggleStatus(StatusKind::Modified)),
            menu::Item::Separator,
        ];
        if !self.name_filters.is_empty() {
            items.push(menu::Item::Heading("Filename Filters".into()));
            for (i, filter) in self.name_filters.iter().enumerate() {
                if filter.is_valid() {
                    items.push(menu::Item::toggle(filter.label.clone(), filter.active, Message::ToggleNameFilter(i)));
                } else {
                    items.push(menu::Item::disabled(format!("{} (invalid)", filter.label)));
                }
            }
            items.push(menu::Item::Separator);
        }
        items.push(menu::Item::toggle("Ignore Filename Case", self.ignore_case, Message::ToggleIgnoreCase));
        items.push(menu::Item::toggle("Normalize Unicode Paths", self.normalize_encoding, Message::ToggleNormalize));
        items.push(menu::Item::toggle(
            "Apply Text Filters",
            self.apply_text_filters,
            Message::ToggleTextFilters,
        ));
        items
    }

    pub fn view<'a>(&'a self, shared: &'a Shared) -> Element<'a, Message> {
        let colors = shared.style.status_colors();
        let mut content = column![].width(Length::Fill).height(Length::Fill);

        if let Some(note) = &self.banners.identical {
            content = content.push(ui::banner(
                BannerKind::Info,
                "Folders have no differences",
                Some(note.clone()),
                vec![("Hide".to_string(), Message::HideBanners)],
            ));
        }
        if let Some(dup) = &self.banners.duplicate {
            content = content.push(ui::banner(
                BannerKind::Warning,
                format!("Folder {} is being compared to itself", dup.display()),
                None,
                vec![("Hide".to_string(), Message::HideBanners)],
            ));
        }
        if self.banners.cancelled {
            content = content.push(ui::banner(
                BannerKind::Warning,
                "Folder scan stopped",
                Some("Not all folders have been compared.".to_string()),
                vec![
                    ("Refresh".to_string(), Message::Refresh),
                    ("Hide".to_string(), Message::HideBanners),
                ],
            ));
        }

        content = content.push(self.pane_headers());

        if self.banners.pane_errors.iter().any(Option::is_some) {
            let mut errors = row![].spacing(1).width(Length::Fill);
            for error in &self.banners.pane_errors {
                let cell: Element<'_, Message> = match error {
                    Some((heading, details)) => ui::banner(
                        BannerKind::Error,
                        heading.as_str(),
                        Some(details.clone()),
                        vec![("Hide".to_string(), Message::HideBanners)],
                    ),
                    None => Space::new().width(Length::Fill).into(),
                };
                errors = errors.push(container(cell).width(Length::FillPortion(1)));
            }
            content = content.push(errors);
        }

        if !self.ready() {
            content = content.push(ui::separator(false));
            content = content.push(
                container(text("Select a folder for each pane to compare").style(ui::dim_text))
                    .center(Length::Fill),
            );
        } else {
            content = content.push(iced::widget::responsive(move |size| {
                let columns = self.fitting_columns(size.width);
                let mut area = column![].width(Length::Fill).height(Length::Fill);
                if !columns.is_empty() {
                    area = area.push(self.column_headers(&columns));
                }
                area.push(ui::separator(false))
                    .push(self.rows_view(shared, &colors, columns, size.width))
                    .into()
            }));
        }

        if self.busy() {
            let label = if self.progress.is_empty() {
                "Scanning…".to_string()
            } else {
                format!("Scanning {}", self.progress)
            };
            content = content.push(
                container(
                    row![
                        text(label).size(13).width(Length::Fill).wrapping(text::Wrapping::None),
                        ui::text_button("Stop", Some(Message::Stop)),
                    ]
                    .spacing(8)
                    .align_y(iced::Center),
                )
                .padding([2, 10])
                .width(Length::Fill)
                .clip(true)
                .style(ui::header_bar),
            );
        }

        let mut base: Element<'_, Message> = content.into();

        match self.popup {
            Some(Popup::Filters) => {
                let filters = StatusFilters::from_names(&shared.settings.folder_status_filters);
                base = menu::overlay(base, self.filter_items(filters), menu::Anchor::TopRight(8.0), Message::ClosePopup);
            }
            Some(Popup::Context(point)) => base = self.context_menu(base, point),
            None => {}
        }

        match &self.dialog {
            None => base,
            Some(dialog) => ui::modal::modal(base, self.dialog_view(dialog), Message::DialogAnswer(false)),
        }
    }

    fn dialog_view<'a>(&'a self, dialog: &'a Dialog) -> Element<'a, Message> {
        let cancel = || button(text("Cancel")).style(button::secondary).on_press(Message::DialogAnswer(false));
        match dialog {
            Dialog::ReplaceFolder { name, parent, .. } => ui::modal::dialog(
                format!("Replace folder “{name}”?"),
                text(format!(
                    "Another folder with the same name already exists in “{parent}”.\nIf you replace the existing folder, all files in it will be lost."
                )),
                vec![
                    cancel().into(),
                    button(text("Replace")).style(button::danger).on_press(Message::DialogAnswer(true)).into(),
                ],
            ),
            Dialog::DeletePermanently { path, .. } => ui::modal::dialog(
                "Delete Permanently?",
                text(format!("“{path}” can’t be put in the trash. Do you want to delete it permanently?")),
                vec![
                    cancel().into(),
                    button(text("Delete Permanently"))
                        .style(button::danger)
                        .on_press(Message::DialogAnswer(true))
                        .into(),
                ],
            ),
            Dialog::Error { title, message } => ui::modal::dialog(
                title.as_str(),
                text(message.as_str()),
                vec![button(text("OK")).on_press(Message::DialogAnswer(false)).into()],
            ),
        }
    }

    fn context_menu<'a>(&'a self, base: Element<'a, Message>, point: Point) -> Element<'a, Message> {
        let mut list = column![].width(260);
        for item in self.action_items(false) {
            list = list.push(match item {
                menu::Item::Action { label, shortcut, message } => {
                    let mut r = row![text(label).size(14).width(Length::Fill)].spacing(12);
                    if let Some(s) = shortcut {
                        r = r.push(text(s).size(12).style(ui::dim_text));
                    }
                    Element::from(
                        button(r)
                            .width(Length::Fill)
                            .padding([5, 10])
                            .style(ui::flat_button)
                            .on_press_maybe(message),
                    )
                }
                _ => container(ui::separator(false)).padding([4, 0]).into(),
            });
        }
        let menu = container(list).padding(6).style(ui::popover);
        stack![
            base,
            mouse_area(
                pin(iced::widget::opaque(menu))
                    .x(point.x)
                    .y(point.y)
                    .width(Length::Fill)
                    .height(Length::Fill)
            )
            .on_press(Message::ClosePopup)
            .on_right_press(Message::ClosePopup),
        ]
        .into()
    }

    fn pane_headers(&self) -> Element<'_, Message> {
        let parent = if self.ready() {
            meld_files::find_shared_parent_path(&self.roots.iter().map(Some).collect::<Vec<_>>())
        } else {
            None
        };
        let mut headers = row![].spacing(1).width(Length::Fill);
        for (pane, root) in self.roots.iter().enumerate() {
            let label = if !self.labels[pane].is_empty() {
                self.labels[pane].clone()
            } else if root.as_os_str().is_empty() {
                format!("Folder {}", pane + 1)
            } else {
                parent
                    .as_ref()
                    .and_then(|p| meld_files::format_parent_relative_path(p, root).ok())
                    .unwrap_or_else(|| meld_files::format_home_relative_path(root))
            };
            let full = if root.as_os_str().is_empty() {
                "No folder selected".to_string()
            } else {
                root.display().to_string()
            };
            let focused = pane == self.sel.pane;
            let label = tooltip(
                container(text(label).size(13).wrapping(text::Wrapping::None).font(if focused {
                    ui::bold()
                } else {
                    Font::DEFAULT
                }))
                .width(Length::Fill)
                .clip(true),
                container(text(full).size(13)).padding([4, 8]).style(ui::tooltip_box),
                tooltip::Position::Bottom,
            );
            headers = headers.push(
                container(
                    row![
                        ui::tool_button(icon::FOLDER, "Select folder to open in this pane", Some(Message::Browse(pane))),
                        label,
                    ]
                    .spacing(4)
                    .align_y(iced::Center),
                )
                .padding([2, 4])
                .width(Length::FillPortion(1))
                .style(ui::header_bar),
            );
        }
        headers.into()
    }

    /// The configured columns that fit next to a usable name column.
    fn fitting_columns(&self, width: f32) -> Vec<Column> {
        let n = self.panes() as f32;
        let pane_width = (width - (n - 1.0)) / n;
        let mut available = pane_width - 16.0 - 160.0;
        let mut out = Vec::new();
        for &column in &self.columns {
            let needed = column.width() + 6.0;
            if needed > available {
                break;
            }
            available -= needed;
            out.push(column);
        }
        out
    }

    fn column_headers(&self, columns: &[Column]) -> Element<'_, Message> {
        let header_text = |label: &'static str| text(label).size(12).font(ui::bold()).wrapping(text::Wrapping::None);
        let mut panes = row![].width(Length::Fill);
        for pane in 0..self.panes() {
            if pane > 0 {
                panes = panes.push(ui::separator(true));
            }
            let mut r = row![container(header_text("Name")).width(Length::Fill).clip(true)]
                .spacing(6)
                .padding([3, 8]);
            for column in columns {
                r = r.push(container(header_text(column.title())).width(column.width()).clip(true));
            }
            panes = panes.push(container(r).width(Length::FillPortion(1)));
        }
        container(panes.height(Length::Shrink)).width(Length::Fill).into()
    }

    fn rows_view<'a>(
        &'a self,
        shared: &'a Shared,
        colors: &StatusColors,
        columns: Vec<Column>,
        width: f32,
    ) -> Element<'a, Message> {
        let n = self.panes() as f32;
        let pane_width = (width - (n - 1.0)) / n;
        let name_width = pane_width - 12.0 - columns.iter().map(|c| c.width() + 6.0).sum::<f32>();
        let total = self.visible.len();
        let first = ((self.scroll_y / self.row_height).floor() as usize).saturating_sub(OVERSCAN).min(total);
        let count = (self.view_height / self.row_height).ceil() as usize + 2 * OVERSCAN;
        let last = (first + count).min(total);

        let mut list = column![Space::new().height(first as f32 * self.row_height)].width(Length::Fill);
        for visible in &self.visible[first..last] {
            let mut cells = row![].width(Length::Fill).height(self.row_height);
            for pane in 0..self.panes() {
                if pane > 0 {
                    cells = cells.push(ui::separator(true));
                }
                cells = cells.push(self.cell(shared, colors, &columns, name_width, pane, *visible));
            }
            list = list.push(cells);
        }
        list = list.push(Space::new().height((total - last) as f32 * self.row_height));

        let scroller = scrollable(list)
            .id(self.scroll_id.clone())
            .on_scroll(Message::Scrolled)
            .width(Length::Fill)
            .height(Length::Fill);

        keys::KeyListener::new(scroller, on_key)
            .on_modifiers(Message::Modifiers)
            .on_right_click(Message::ContextMenu)
            .into()
    }

    fn cell<'a>(
        &'a self,
        _shared: &'a Shared,
        colors: &StatusColors,
        columns: &[Column],
        name_width: f32,
        pane: usize,
        visible: Visible,
    ) -> Element<'a, Message> {
        let id = visible.id;
        let node = self.tree.node(id);
        let size = self.text_size;
        let selected = pane == self.sel.pane && self.sel.is_selected(id);
        let cursor_elsewhere = pane != self.sel.pane && self.sel.cursor == Some(id);

        let expander: Element<'a, Message> = if self.tree.has_children(id) {
            mouse_area(
                container(text(if node.expanded { "▾" } else { "▸" }).size(size))
                    .width(INDENT)
                    .center_x(INDENT),
            )
            .on_press(Message::ToggleExpand(id))
            .interaction(iced::mouse::Interaction::Pointer)
            .into()
        } else {
            Space::new().width(INDENT).into()
        };

        let mut line = row![Space::new().width(visible.depth as f32 * INDENT), expander]
            .spacing(2)
            .align_y(iced::Center)
            .height(Length::Fill);

        let state = self.tree.pane_state(id, pane);
        let style = state.map(|s| state_style(s, colors));
        let (name, glyph): (String, Option<&'static str>) = match &node.kind {
            Kind::Entry(row) => {
                let pane_status = &row.status.panes[pane];
                let mut name = row.names[pane].clone();
                if let Some(target) = &pane_status.symlink_target {
                    name = format!("{name} ⟶ {}", target.display());
                }
                if node.scanning {
                    name = format!("{name} (scanning…)");
                }
                let glyph = if self.marked == Some((id, pane)) {
                    Some(MARK_GLYPH)
                } else if row.is_dir || pane_status.is_dir {
                    Some(FOLDER_GLYPH)
                } else if pane_status.exists {
                    Some(FILE_GLYPH)
                } else {
                    None
                };
                (name, glyph)
            }
            Kind::Empty => ("empty folder".to_string(), None),
            Kind::Error { pane: p, message } if *p == pane => (message.clone(), Some(ERROR_GLYPH)),
            Kind::Error { .. } => (String::new(), None),
        };
        let (fg, italic, bold, strike) = match style {
            Some(s) => s,
            None => (None, false, false, false),
        };
        let fg = if node.scanning { None } else { fg };
        let italic = italic || node.scanning;
        let icon_color = match (&node.kind, state) {
            (Kind::Entry(_), Some(State::New | State::Modified | State::Conflict | State::NonExist)) => fg,
            (Kind::Error { .. }, _) => fg,
            _ => None,
        };
        line = line.push(
            container(match glyph {
                Some(g) => Element::from(text(g).size(size + 1.0).color_maybe(icon_color)),
                None => Space::new().into(),
            })
            .width(size + 6.0)
            .center_x(size + 6.0),
        );
        let font = Font {
            weight: if bold { iced::font::Weight::Bold } else { iced::font::Weight::Normal },
            style: if italic { iced::font::Style::Italic } else { iced::font::Style::Normal },
            ..Font::DEFAULT
        };
        let name_widget: Element<'a, Message> = if strike {
            // Strikethrough lines aren't clipped with the text, so keep
            // struck-through names within their column.
            let available = name_width - (visible.depth as f32 + 1.0) * INDENT - size - 6.0 - 3.0 * 6.0;
            let max_chars = (available / (size * 0.62)).floor().max(1.0) as usize;
            let name = if name.chars().count() > max_chars {
                let mut short: String = name.chars().take(max_chars.saturating_sub(1)).collect();
                short.push('…');
                short
            } else {
                name
            };
            iced::widget::rich_text([iced::widget::span::<(), Font>(name).color_maybe(fg).strikethrough(true).font(font)])
                .size(size)
                .wrapping(text::Wrapping::None)
                .into()
        } else {
            text(name).size(size).font(font).color_maybe(fg).wrapping(text::Wrapping::None).into()
        };
        line = line.push(container(name_widget).width(Length::Fill).clip(true));

        if let Kind::Entry(row) = &node.kind {
            let pane_status = &row.status.panes[pane];
            for column in columns {
                let value = if pane_status.exists && !node.scanning {
                    column.value(pane_status)
                } else {
                    String::new()
                };
                line = line.push(
                    container(text(value).size(size - 1.0).wrapping(text::Wrapping::None))
                        .width(column.width())
                        .clip(true),
                );
            }
        } else {
            for column in columns {
                line = line.push(Space::new().width(column.width()));
            }
        }

        let body = container(line.spacing(6))
            .padding([0, 6])
            .width(Length::Fill)
            .height(Length::Fill)
            .style(move |theme: &Theme| {
                let palette = theme.extended_palette();
                let background = if selected {
                    Some(Background::Color(palette.primary.base.color.scale_alpha(0.35)))
                } else if cursor_elsewhere {
                    Some(Background::Color(palette.background.strong.color.scale_alpha(0.35)))
                } else {
                    None
                };
                container::Style {
                    background,
                    border: Border::default(),
                    ..container::Style::default()
                }
            });
        mouse_area(body)
            .on_press(Message::RowPressed(pane, id))
            .on_right_press(Message::RowRightPressed(pane, id))
            .into()
    }
}

/// Keys handled by the trees themselves, when no other widget took them.
fn on_key(key: &Key, modifiers: Modifiers) -> Option<Message> {
    if modifiers.alt() || modifiers.logo() {
        return None;
    }
    let shift = modifiers.shift();
    let ctrl = modifiers.command();
    let nav = match key {
        Key::Named(Named::ArrowUp) if !ctrl => Nav::Up,
        Key::Named(Named::ArrowDown) if !ctrl => Nav::Down,
        Key::Named(Named::PageUp) if !ctrl => Nav::PageUp,
        Key::Named(Named::PageDown) if !ctrl => Nav::PageDown,
        Key::Named(Named::Home) => Nav::Home,
        Key::Named(Named::End) => Nav::End,
        Key::Named(Named::ArrowLeft) if !ctrl && !shift => Nav::PaneLeft,
        Key::Named(Named::ArrowRight) if !ctrl && !shift => Nav::PaneRight,
        Key::Named(Named::ArrowLeft) if shift => Nav::Collapse,
        Key::Named(Named::ArrowRight) if shift => Nav::Expand,
        Key::Named(Named::Backspace) => Nav::Parent,
        Key::Character(c) if !ctrl => match c.as_str() {
            "+" | "=" => Nav::Expand,
            "-" => Nav::Collapse,
            "*" => Nav::ExpandAll,
            _ => return None,
        },
        Key::Character(c) if ctrl && c.as_str().eq_ignore_ascii_case("a") => Nav::SelectAll,
        _ => return None,
    };
    let extend = shift && matches!(nav, Nav::Up | Nav::Down | Nav::PageUp | Nav::PageDown | Nav::Home | Nav::End);
    Some(Message::Nav(nav, extend))
}

/// Text attributes for a state (`text_attributes` in `tree.py`):
/// foreground, italic, bold, strikethrough.
fn state_style(state: State, colors: &StatusColors) -> (Option<Color>, bool, bool, bool) {
    let c = |rgba| Some(crate::shared::color(rgba));
    match state {
        State::Ignored | State::None => (c(colors.unknown), false, false, false),
        State::Normal => (None, false, false, false),
        State::NoChange => (None, true, false, false),
        State::Error => (c(colors.error), false, true, false),
        State::Empty => (c(colors.unknown), true, false, false),
        State::New => (c(colors.new), false, true, false),
        State::Modified => (c(colors.modified), false, true, false),
        State::Renamed => (c(colors.modified), false, false, false),
        State::Conflict => (c(colors.conflict), false, true, false),
        State::Removed | State::Missing => (c(colors.removed), false, true, true),
        State::NonExist => (c(colors.unknown), false, false, true),
        State::Spinner => (None, true, false, false),
    }
}

fn absolute(path: &Path) -> PathBuf {
    if path.as_os_str().is_empty() || path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().map(|cwd| cwd.join(path)).unwrap_or_else(|_| path.to_path_buf())
    }
}

/// Runs a file operation (on a background thread).
fn execute(op: Op, other_paths: &[PathBuf]) -> OpResult {
    match op {
        Op::Copy {
            node,
            ref src,
            ref dst,
            replace,
        } => match meld_files::copy_entry(src, dst, replace) {
            Ok(outcome) => OpResult::Copied { node, outcome },
            Err(FileOpError::FolderExists { .. }) => OpResult::FolderExists(op),
            Err(err) => OpResult::Failed {
                title: "Error copying file".into(),
                message: err.to_string(),
                stop: false,
            },
        },
        Op::Delete {
            node,
            ref path,
            permanently,
        } => {
            let result = if permanently {
                meld_files::delete_permanently(path).map_err(|e| e.to_string())
            } else {
                match meld_files::trash_or_confirm(path) {
                    Ok(_) => Ok(()),
                    Err(TrashError::NeedsConfirmation { .. }) => return OpResult::NeedsConfirmation(op),
                    Err(err) => Err(err.to_string()),
                }
            };
            match result {
                Ok(()) => OpResult::Deleted {
                    node,
                    still_present: other_paths.iter().any(|p| p.exists()),
                },
                Err(message) => OpResult::Failed {
                    title: format!("Error deleting {}", path.display()),
                    message,
                    stop: true,
                },
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_bindings() {
        let none = Modifiers::empty();
        assert!(matches!(on_key(&Key::Named(Named::ArrowDown), none), Some(Message::Nav(Nav::Down, false))));
        assert!(matches!(
            on_key(&Key::Named(Named::ArrowDown), Modifiers::SHIFT),
            Some(Message::Nav(Nav::Down, true))
        ));
        assert!(on_key(&Key::Named(Named::ArrowDown), Modifiers::ALT).is_none());
        assert!(on_key(&Key::Named(Named::Enter), none).is_none());
        assert!(matches!(on_key(&Key::Character("a".into()), Modifiers::CTRL), Some(Message::Nav(Nav::SelectAll, false))));
    }

    #[test]
    fn columns_follow_settings() {
        let mut settings = Settings::default();
        assert_eq!(Column::from_settings(&settings), [Column::Size, Column::Time]);
        settings.folder_columns.reverse();
        settings.folder_columns[0].visible = true;
        assert_eq!(Column::from_settings(&settings), [Column::Permissions, Column::Time, Column::Size]);
    }

    #[test]
    fn copy_and_delete_operations() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a");
        let b = dir.path().join("b");
        std::fs::create_dir_all(a.join("sub")).unwrap();
        std::fs::create_dir_all(b.join("sub")).unwrap();
        std::fs::write(a.join("f"), "x").unwrap();
        let result = execute(
            Op::Copy {
                node: 1,
                src: a.join("f"),
                dst: b.join("f"),
                replace: false,
            },
            &[],
        );
        assert!(matches!(result, OpResult::Copied { node: 1, outcome: CopyOutcome::File }));
        assert_eq!(std::fs::read(b.join("f")).unwrap(), b"x");
        let result = execute(
            Op::Copy {
                node: 2,
                src: a.join("sub"),
                dst: b.join("sub"),
                replace: false,
            },
            &[],
        );
        assert!(matches!(result, OpResult::FolderExists(Op::Copy { node: 2, .. })));
        let result = execute(
            Op::Delete {
                node: 3,
                path: b.join("f"),
                permanently: true,
            },
            &[a.join("f")],
        );
        assert!(matches!(result, OpResult::Deleted { node: 3, still_present: true }));
        assert!(!b.join("f").exists());
    }

    #[test]
    fn provisional_rows_name_the_folders() {
        let row = provisional_row(&[PathBuf::from("/x/left"), PathBuf::from("/y/right")]);
        assert_eq!(row.names, ["left", "right"]);
        assert!(row.is_dir);
        assert_eq!(row.status.panes.len(), 2);
    }
}
