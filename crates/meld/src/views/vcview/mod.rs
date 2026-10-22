//! Version control view (`vcview.py`, `vcview.ui`).
//!
//! Shows the files of a version controlled folder with their status,
//! opens comparisons against the repository, and runs version control
//! actions (commit, update, push, add, remove, revert, resolve, unstage)
//! whose command lines and output appear in the console pane. Backend
//! detection, scans and commands run on background tasks; see [`jobs`].

mod dialogs;
mod jobs;
mod keys;
mod tree;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use iced::keyboard::{Key, Modifiers, key::Named};
use iced::widget::operation::{self, AbsoluteOffset};
use iced::widget::{
    button, checkbox, column, container, mouse_area, pick_list, rich_text, row, scrollable, space, span, text,
    text_editor,
};
use iced::{Background, Border, Color, Element, Font, Length, Subscription, Task, Theme, mouse, task};
use meld_config::StatusColors;
use meld_vc::view::{remove_needs_confirmation, tab_labels};
use meld_vc::{
    Action, ActionSensitivity, CommitSettings, DiffOptions, DiffRequest, MergeFileOrder, State,
    StatusFilters, Vc, VcChoices, VcKind,
};

use crate::command::Command;
use crate::comparison::{Comparison, FileExtras};
use crate::shared::{self, Shared};
use crate::ui::{self, icon, menu};
use crate::views::Event;
use dialogs::{CommitDialog, Dialog, HistoryItem};
use jobs::{CommandEvent, ConsoleKind, ScanEvent, SharedVc, Slot};
use tree::{Activation, Mods, NodeId, NodeKind, TreeModel};

const ROW_HEIGHT: f32 = 26.0;
const INDENT: f32 = 18.0;
const STATUS_WIDTH: f32 = 110.0;
const TREE_SCROLL: &str = "vc-tree";

/// An entry of the backend chooser.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VcItem {
    index: usize,
    label: String,
}

impl std::fmt::Display for VcItem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.label)
    }
}

/// The status filters (`vc-status-filters`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Filter {
    Modified,
    Normal,
    Unknown,
    Ignored,
    Flatten,
}

impl Filter {
    const STATES: [Filter; 4] = [Filter::Modified, Filter::Normal, Filter::Unknown, Filter::Ignored];

    fn label(self) -> &'static str {
        match self {
            Filter::Modified => "Modified",
            Filter::Normal => "Normal",
            Filter::Unknown => "Unversioned",
            Filter::Ignored => "Ignored",
            Filter::Flatten => "Flatten Folders",
        }
    }

    fn get(self, filters: &StatusFilters) -> bool {
        match self {
            Filter::Modified => filters.modified,
            Filter::Normal => filters.normal,
            Filter::Unknown => filters.unknown,
            Filter::Ignored => filters.ignored,
            Filter::Flatten => filters.flatten,
        }
    }

    fn toggle(self, filters: &mut StatusFilters) {
        let flag = match self {
            Filter::Modified => &mut filters.modified,
            Filter::Normal => &mut filters.normal,
            Filter::Unknown => &mut filters.unknown,
            Filter::Ignored => &mut filters.ignored,
            Filter::Flatten => &mut filters.flatten,
        };
        *flag = !*flag;
    }
}

/// The view's actions (`view.compare`, `view.vc-*`, `view.open-external`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Act {
    Compare,
    Commit,
    Update,
    Push,
    Add,
    Unstage,
    Remove,
    Revert,
    Resolve,
    Delete,
    OpenExternal,
}

impl Act {
    fn enabled(self, s: &ActionSensitivity) -> bool {
        match self {
            Act::Compare => s.compare,
            Act::Commit => s.commit,
            Act::Update => s.update,
            Act::Push => s.push,
            Act::Add => s.add,
            Act::Unstage => s.unstage,
            Act::Remove => s.remove,
            Act::Revert => s.revert,
            Act::Resolve => s.resolve,
            Act::Delete => s.delete_locally,
            Act::OpenExternal => s.open_external,
        }
    }

    /// Whether the action changes the working copy or repository, so it
    /// must wait for scans and other commands to finish.
    fn mutates(self) -> bool {
        !matches!(self, Act::Compare | Act::OpenExternal)
    }
}

/// Keyboard navigation in the tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Nav {
    Up,
    Down,
    PageUp,
    PageDown,
    Home,
    End,
    Left,
    Right,
    SelectAll,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// Looking for version control systems.
    Loading,
    Scanning,
    Idle,
}

struct ConsoleLine {
    kind: ConsoleKind,
    text: String,
}

/// Dragging the console separator.
#[derive(Debug, Clone, Copy)]
struct Drag {
    /// Cursor y and console height when the drag started.
    start: Option<(f32, f32)>,
}

pub struct VcView {
    /// The requested folder; the backend's location once opened.
    location: PathBuf,
    /// Open comparisons of all modified files after the first scan.
    auto_compare: bool,
    title: String,
    tooltip: String,

    choices: Option<VcChoices>,
    active_choice: usize,
    /// The backend instance jobs run on.
    vc: Option<SharedVc>,
    /// A second instance for queries on the UI thread.
    ui_vc: Option<Box<dyn Vc>>,
    kind: Option<VcKind>,

    tree: TreeModel,
    push_summary: Option<String>,
    filters: StatusFilters,
    sensitivity: ActionSensitivity,

    phase: Phase,
    progress: String,
    load_gen: u64,
    scan_gen: u64,
    scan_handle: Option<task::Handle>,
    scan_cancel: Option<Arc<AtomicBool>>,
    /// Command jobs in flight.
    running: usize,

    console: Vec<ConsoleLine>,
    console_height: f32,
    drag: Option<Drag>,
    dialog: Option<Dialog>,
    error: Option<(String, Option<String>)>,

    modifiers: Modifiers,
    scroll_y: f32,
    viewport_height: f32,
}

#[derive(Debug, Clone)]
pub enum Message {
    Loaded(u64, Result<jobs::Loaded, String>),
    ChooseVc(VcItem),
    Opened(u64, Result<Slot<jobs::OpenedVc>, String>),
    ChooseLocation,
    LocationChosen(Option<PathBuf>),
    Refresh,
    Stop,
    Scan(u64, ScanEvent),

    RowPressed(usize),
    RowActivated(usize),
    ToggleRow(NodeId),
    Nav(Nav, bool),
    Modifiers(Modifiers),
    Scrolled(scrollable::Viewport),
    PreviousChange,
    NextChange,

    ToggleFilter(Filter),
    Action(Act),
    DiffsReady(Vec<Result<DiffRequest, String>>),
    Command(CommandEvent),
    CommitInfo(jobs::CommitInfo),
    PushSummary(Result<String, String>),
    Trashed((Vec<PathBuf>, Vec<String>)),
    Deleted(Vec<String>),
    Done,

    CloseDialog,
    ConfirmDialog,
    CommitEdit(text_editor::Action),
    CommitHistory(HistoryItem),

    ToggleConsole,
    ClearConsole,
    DragStart,
    DragMove(f32),
    DragEnd,
    DismissError,
}

impl VcView {
    pub fn new(location: PathBuf, auto_compare: bool, shared: &mut Shared) -> (Self, Task<Message>) {
        let location = meld_vc::paths::abspath(&location);
        let title = location
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| location.display().to_string());
        let mut view = VcView {
            tooltip: location.display().to_string(),
            title,
            tree: TreeModel::root_only(&location),
            location,
            auto_compare,
            choices: None,
            active_choice: 0,
            vc: None,
            ui_vc: None,
            kind: None,
            push_summary: None,
            filters: StatusFilters::from_keys(&shared.settings.vc_status_filters),
            sensitivity: ActionSensitivity::default(),
            phase: Phase::Idle,
            progress: String::new(),
            load_gen: 0,
            scan_gen: 0,
            scan_handle: None,
            scan_cancel: None,
            running: 0,
            console: Vec::new(),
            console_height: 180.0,
            drag: None,
            dialog: None,
            error: None,
            modifiers: Modifiers::empty(),
            scroll_y: 0.0,
            viewport_height: 0.0,
        };
        let task = view.load();
        (view, task)
    }

    pub fn title(&self) -> String {
        self.title.clone()
    }

    pub fn tooltip(&self) -> String {
        self.tooltip.clone()
    }

    pub fn unsaved(&self) -> Vec<String> {
        Vec::new()
    }

    // ----- Loading and scanning -----

    /// Looks for version control systems at the location and opens the
    /// default one (`set_location`).
    fn load(&mut self) -> Task<Message> {
        self.cancel_scan();
        self.load_gen += 1;
        let generation = self.load_gen;
        self.phase = Phase::Loading;
        self.progress = "Looking for version control systems".into();
        self.update_sensitivity();
        Task::perform(jobs::load(self.location.clone(), self.kind), move |result| {
            Message::Loaded(generation, result)
        })
    }

    fn install(&mut self, opened: jobs::OpenedVc) {
        let jobs::OpenedVc { worker, ui } = opened;
        self.kind = Some(ui.kind());
        let location = ui.location().to_path_buf();
        let (title, tooltip) = tab_labels(&*ui, &location);
        self.title = title;
        self.tooltip = tooltip;
        if self.tree.root().and_then(|r| r.entry()).map(|e| e.path.as_path()) != Some(location.as_path()) {
            self.tree = TreeModel::root_only(&location);
        }
        self.location = location;
        self.vc = Some(Arc::new(Mutex::new(worker)));
        self.ui_vc = Some(ui);
        self.push_summary = None;
    }

    fn cancel_scan(&mut self) {
        if let Some(handle) = self.scan_handle.take() {
            handle.abort();
        }
        if let Some(cancel) = self.scan_cancel.take() {
            cancel.store(true, Ordering::Relaxed);
        }
        if self.phase == Phase::Scanning {
            self.phase = Phase::Idle;
            self.progress.clear();
        }
    }

    /// Rescans the tree, first refreshing the backend's cached state when
    /// `refresh`.
    fn scan(&mut self, refresh: bool) -> Task<Message> {
        self.cancel_scan();
        let Some(vc) = self.vc.clone() else {
            return Task::none();
        };
        self.scan_gen += 1;
        let generation = self.scan_gen;
        self.phase = Phase::Scanning;
        self.progress = "Scanning repository".into();
        let cancel = Arc::new(AtomicBool::new(false));
        let (task, handle) = Task::run(jobs::scan(vc, self.filters, refresh, cancel.clone()), move |event| {
            Message::Scan(generation, event)
        })
        .abortable();
        self.scan_handle = Some(handle);
        self.scan_cancel = Some(cancel);
        self.update_sensitivity();
        task
    }

    fn update_sensitivity(&mut self) {
        let selection = self.tree.path_states();
        self.sensitivity = meld_vc::action_sensitivity(self.ui_vc.as_deref(), &selection);
    }

    /// Whether `act` can run now.
    fn can(&self, act: Act) -> bool {
        let ready = self.vc.is_some()
            && self.phase != Phase::Loading
            && (!act.mutates() || (self.phase == Phase::Idle && self.running == 0));
        ready && act.enabled(&self.sensitivity)
    }

    fn act_message(&self, act: Act) -> Option<Message> {
        self.can(act).then_some(Message::Action(act))
    }

    fn report(&mut self, primary: impl Into<String>, secondary: impl Into<Option<String>>) {
        self.error = Some((primary.into(), secondary.into()));
    }

    // ----- Actions -----

    fn diff_options(shared: &Shared) -> DiffOptions {
        DiffOptions {
            left_is_local: shared.settings.vc_left_is_local,
            merge_file_order: match shared.settings.vc_merge_file_order {
                meld_config::MergeFileOrder::LocalMergeRemote => MergeFileOrder::LocalMergeRemote,
                meld_config::MergeFileOrder::RemoteMergeLocal => MergeFileOrder::RemoteMergeLocal,
            },
        }
    }

    fn compare(&mut self, paths: Vec<PathBuf>, shared: &Shared) -> Task<Message> {
        let Some(vc) = self.vc.clone() else {
            return Task::none();
        };
        if paths.is_empty() {
            return Task::none();
        }
        Task::perform(jobs::prepare_diffs(vc, paths, Self::diff_options(shared)), Message::DiffsReady)
    }

    fn run_action(&mut self, action: Action, files: Vec<PathBuf>, message: String) -> Task<Message> {
        let Some(vc) = self.vc.clone() else {
            return Task::none();
        };
        self.running += 1;
        Task::run(jobs::run_action(vc, action, files, message), Message::Command)
    }

    fn action(&mut self, act: Act, shared: &mut Shared) -> Task<Message> {
        if !self.can(act) {
            return Task::none();
        }
        let selected = self.tree.selected_paths();
        match act {
            Act::Compare => self.compare(selected, shared),
            Act::Commit => {
                let Some(vc) = self.vc.clone() else {
                    return Task::none();
                };
                self.dialog = Some(Dialog::Commit(Box::new(CommitDialog::new(selected.clone()))));
                Task::batch([
                    Task::perform(jobs::commit_info(vc, selected), Message::CommitInfo),
                    operation::focus(dialogs::COMMIT_EDITOR),
                ])
            }
            Act::Push => {
                let Some(vc) = self.vc.clone() else {
                    return Task::none();
                };
                self.dialog = Some(Dialog::Push(None));
                Task::perform(jobs::push_summary(vc), Message::PushSummary)
            }
            Act::Update => self.run_action(Action::Update, Vec::new(), String::new()),
            Act::Add => self.run_action(Action::Add, selected, String::new()),
            Act::Unstage => self.run_action(Action::Unstage, selected, String::new()),
            Act::Resolve => self.run_action(Action::Resolve, selected, String::new()),
            Act::Remove => {
                // Removing only files from version control needs no
                // confirmation.
                if remove_needs_confirmation(&selected) {
                    self.dialog = Some(Dialog::ConfirmRemove(selected));
                    Task::none()
                } else {
                    self.run_action(Action::Remove, selected, String::new())
                }
            }
            Act::Revert => {
                self.dialog = Some(Dialog::ConfirmRevert(selected));
                Task::none()
            }
            Act::Delete => {
                self.running += 1;
                Task::perform(jobs::trash(selected), Message::Trashed)
            }
            Act::OpenExternal => {
                let settings = meld_files::ExternalEditorSettings {
                    use_system_editor: shared.settings.use_system_editor,
                    custom_editor_command: shared.settings.custom_editor_command.clone(),
                };
                let errors: Vec<String> = selected
                    .iter()
                    .filter_map(|path| {
                        let argv = match meld_files::open_external_command(path, 0, &settings, false) {
                            Ok(argv) => argv,
                            Err(e) => return Some(e.to_string()),
                        };
                        meld_files::external::spawn_detached(&argv)
                            .err()
                            .map(|e| format!("Failed to launch {}: {e}", argv.join(" ")))
                    })
                    .collect();
                if !errors.is_empty() {
                    self.report("Could not open the file externally", Some(errors.join("\n")));
                }
                Task::none()
            }
        }
    }

    fn confirm_dialog(&mut self, shared: &mut Shared) -> Task<Message> {
        let Some(dialog) = self.dialog.take() else {
            return Task::none();
        };
        match dialog {
            Dialog::Commit(mut commit) => {
                let settings = &shared.settings;
                let commit_settings = CommitSettings {
                    show_margin: settings.vc_show_commit_margin,
                    margin: settings.vc_commit_margin.max(1) as usize,
                    break_message: settings.vc_break_commit_message,
                };
                let message = meld_vc::finish_commit_message(&commit.message(), &commit_settings);
                let mut tasks = Vec::new();
                if !message.trim().is_empty() {
                    commit.history.prepend(&message);
                    tasks.push(Task::perform(jobs::save_history(commit.history.clone()), |_| Message::Done));
                }
                tasks.push(self.run_action(Action::Commit, std::mem::take(&mut commit.selected), message));
                Task::batch(tasks)
            }
            Dialog::Push(_) => self.run_action(Action::Push, Vec::new(), String::new()),
            Dialog::ConfirmRemove(files) => self.run_action(Action::Remove, files, String::new()),
            Dialog::ConfirmRevert(files) => self.run_action(Action::Revert, files, String::new()),
            Dialog::ConfirmDelete(files) => {
                self.running += 1;
                Task::perform(jobs::delete_permanently(files), Message::Deleted)
            }
        }
    }

    fn open_diffs(&mut self, results: Vec<Result<DiffRequest, String>>, events: &mut Vec<Event>) {
        let mut errors = Vec::new();
        for result in results {
            match result {
                Ok(DiffRequest::Folder(path)) => events.push(Event::Open(Comparison::Folders {
                    paths: vec![path],
                    labels: Vec::new(),
                    auto_compare: false,
                })),
                Ok(DiffRequest::Files {
                    paths,
                    labels,
                    tab_label,
                    merge_output,
                    auto_merge,
                    prompt_resolve,
                    diff_sides,
                    temp_files,
                }) => {
                    let side = |s: meld_vc::DiffSide| match s {
                        meld_vc::DiffSide::New => meld_config::DiffSide::New,
                        meld_vc::DiffSide::Old => meld_config::DiffSide::Old,
                    };
                    events.push(Event::Open(Comparison::Files {
                        paths: paths.into_iter().map(Some).collect(),
                        labels: labels.into_iter().map(Option::unwrap_or_default).collect(),
                        merge_output,
                        auto_merge,
                        extras: FileExtras {
                            tab_label: Some(tab_label),
                            prompt_resolve,
                            diff_sides: diff_sides.map(|[a, b]| [side(a), side(b)]),
                            temp_files,
                        },
                    }));
                }
                Err(e) => errors.push(e),
            }
        }
        if !errors.is_empty() {
            self.report("Could not open the comparison", Some(errors.join("\n")));
        }
    }

    fn activate(&mut self, id: NodeId, shared: &Shared) -> Task<Message> {
        match self.tree.activation(id) {
            Activation::Toggle(id) => {
                self.tree.toggle_expanded(id);
                self.update_sensitivity();
                Task::none()
            }
            Activation::Diff(path) => self.compare(vec![path], shared),
            Activation::Nothing => Task::none(),
        }
    }

    // ----- Tree navigation -----

    fn page_rows(&self) -> isize {
        ((self.viewport_height / ROW_HEIGHT).floor() as isize - 1).max(1)
    }

    /// Scrolls the cursor row into view.
    fn reveal_cursor(&mut self) -> Task<Message> {
        let Some(row) = self.tree.cursor_row() else {
            return Task::none();
        };
        let top = row as f32 * ROW_HEIGHT;
        let height = if self.viewport_height > 0.0 { self.viewport_height } else { return Task::none() };
        let target = if top < self.scroll_y {
            top
        } else if top + ROW_HEIGHT > self.scroll_y + height {
            top + ROW_HEIGHT - height
        } else {
            return Task::none();
        };
        self.scroll_y = target.max(0.0);
        operation::scroll_to(
            TREE_SCROLL,
            AbsoluteOffset {
                x: None,
                y: Some(self.scroll_y),
            },
        )
    }

    fn navigate(&mut self, nav: Nav, extend: bool) -> Task<Message> {
        let last = self.tree.visible().len().saturating_sub(1);
        match nav {
            Nav::Up => self.tree.move_cursor(-1, extend),
            Nav::Down => self.tree.move_cursor(1, extend),
            Nav::PageUp => self.tree.move_cursor(-self.page_rows(), extend),
            Nav::PageDown => self.tree.move_cursor(self.page_rows(), extend),
            Nav::Home => self.tree.move_to_row(0, extend),
            Nav::End => self.tree.move_to_row(last, extend),
            Nav::Left => self.tree.key_left(),
            Nav::Right => self.tree.key_right(),
            Nav::SelectAll => self.tree.select_all(),
        }
        self.update_sensitivity();
        self.reveal_cursor()
    }

    fn go_to_change(&mut self, forward: bool) -> Task<Message> {
        match self.tree.find_change(forward) {
            Some(id) => {
                self.tree.go_to(id);
                self.update_sensitivity();
                self.reveal_cursor()
            }
            None => Task::none(),
        }
    }

    fn toggle_console(shared: &mut Shared, events: &mut Vec<Event>) {
        shared.settings.vc_console_visible = !shared.settings.vc_console_visible;
        events.push(Event::SettingsChanged);
    }

    // ----- Update -----

    pub fn update(&mut self, message: Message, shared: &mut Shared, events: &mut Vec<Event>) -> Task<Message> {
        match message {
            Message::Loaded(generation, result) => {
                if generation != self.load_gen {
                    return Task::none();
                }
                self.phase = Phase::Idle;
                self.progress.clear();
                let loaded = match result {
                    Ok(loaded) => loaded,
                    Err(e) => {
                        self.report("Could not look for version control systems", Some(e));
                        return Task::none();
                    }
                };
                self.active_choice = loaded.choices.active;
                self.choices = Some(loaded.choices);
                match loaded.vc.map(|slot| slot.take()) {
                    Ok(Some(opened)) => {
                        self.install(opened);
                        return self.scan(true);
                    }
                    Ok(None) => {}
                    Err(e) => self.report("Could not open the version control system", Some(e)),
                }
                self.update_sensitivity();
            }
            Message::ChooseVc(item) => {
                let Some(choices) = &self.choices else {
                    return Task::none();
                };
                let Some(choice) = choices.choices.get(item.index) else {
                    return Task::none();
                };
                if !choice.enabled() || item.index == self.active_choice {
                    return Task::none();
                }
                let (kind, location) = (choice.kind, choices.location.clone());
                self.active_choice = item.index;
                self.cancel_scan();
                self.load_gen += 1;
                let generation = self.load_gen;
                self.phase = Phase::Loading;
                self.update_sensitivity();
                return Task::perform(jobs::open(kind, location), move |result| {
                    Message::Opened(generation, result)
                });
            }
            Message::Opened(generation, result) => {
                if generation != self.load_gen {
                    return Task::none();
                }
                self.phase = Phase::Idle;
                match result.map(|slot| slot.take()) {
                    Ok(Some(opened)) => {
                        self.install(opened);
                        return self.scan(true);
                    }
                    Ok(None) => {}
                    Err(e) => self.report("Could not open the version control system", Some(e)),
                }
                self.update_sensitivity();
            }
            Message::ChooseLocation => {
                let dialog = rfd::AsyncFileDialog::new()
                    .set_title("Select Folder")
                    .set_directory(&self.location);
                return Task::perform(dialog.pick_folder(), |h| {
                    Message::LocationChosen(h.map(|h| h.path().to_path_buf()))
                });
            }
            Message::LocationChosen(path) => {
                if let Some(path) = path {
                    self.location = path;
                    self.tree = TreeModel::root_only(&self.location);
                    self.error = None;
                    return self.load();
                }
            }
            Message::Refresh => return self.load(),
            Message::Stop => self.stop(),
            Message::Scan(generation, event) => {
                if generation != self.scan_gen {
                    return Task::none();
                }
                match event {
                    ScanEvent::Progress(progress) => self.progress = progress,
                    ScanEvent::Done(result) => return self.scan_done(&result, shared),
                }
            }

            Message::RowPressed(row) => {
                if self.dialog.is_none() {
                    let mods = Mods {
                        ctrl: self.modifiers.command(),
                        shift: self.modifiers.shift(),
                    };
                    self.tree.click(row, mods);
                    self.update_sensitivity();
                }
            }
            Message::RowActivated(row) => {
                if let Some(&id) = self.tree.visible().get(row) {
                    self.tree.select_only(id);
                    self.update_sensitivity();
                    return self.activate(id, shared);
                }
            }
            Message::ToggleRow(id) => {
                self.tree.toggle_expanded(id);
                self.update_sensitivity();
            }
            Message::Nav(nav, extend) => {
                if self.dialog.is_none() {
                    return self.navigate(nav, extend);
                }
            }
            Message::Modifiers(modifiers) => self.modifiers = modifiers,
            Message::Scrolled(viewport) => {
                self.scroll_y = viewport.absolute_offset().y;
                self.viewport_height = viewport.bounds().height;
            }
            Message::PreviousChange => return self.go_to_change(false),
            Message::NextChange => return self.go_to_change(true),

            Message::ToggleFilter(filter) => {
                filter.toggle(&mut self.filters);
                shared.settings.vc_status_filters = self.filters.keys();
                events.push(Event::SettingsChanged);
                return self.scan(true);
            }
            Message::Action(act) => return self.action(act, shared),
            Message::DiffsReady(results) => self.open_diffs(results, events),
            Message::Command(event) => match event {
                CommandEvent::Console(kind, text) => {
                    if !text.is_empty() {
                        self.console.push(ConsoleLine { kind, text });
                    }
                }
                CommandEvent::Finished { refresh, failed, error } => {
                    self.running = self.running.saturating_sub(1);
                    if failed && !shared.settings.vc_console_visible {
                        shared.settings.vc_console_visible = true;
                        events.push(Event::SettingsChanged);
                    }
                    if let Some(error) = error {
                        self.report("Error running command", Some(error));
                    }
                    if refresh {
                        return self.scan(true);
                    }
                    self.update_sensitivity();
                }
            },
            Message::CommitInfo(info) => {
                if let Some(Dialog::Commit(commit)) = &mut self.dialog {
                    commit.files = Some(info.files);
                    commit.history = info.history;
                    if let Some(prefill) = info.prefill
                        && commit.content.text().trim().is_empty()
                    {
                        commit.content = text_editor::Content::with_text(&prefill);
                    }
                }
            }
            Message::PushSummary(summary) => {
                if let Some(Dialog::Push(state)) = &mut self.dialog {
                    *state = Some(summary);
                }
            }
            Message::Trashed((confirm, errors)) => {
                self.running = self.running.saturating_sub(1);
                if !errors.is_empty() {
                    self.report("Error deleting files", Some(errors.join("\n")));
                }
                if !confirm.is_empty() {
                    self.dialog = Some(Dialog::ConfirmDelete(confirm));
                }
                return self.scan(true);
            }
            Message::Deleted(errors) => {
                self.running = self.running.saturating_sub(1);
                if !errors.is_empty() {
                    self.report("Error deleting files", Some(errors.join("\n")));
                }
                return self.scan(true);
            }
            Message::Done => {}

            Message::CloseDialog => self.dialog = None,
            Message::ConfirmDialog => return self.confirm_dialog(shared),
            Message::CommitEdit(action) => {
                if let Some(Dialog::Commit(commit)) = &mut self.dialog {
                    commit.content.perform(action);
                }
            }
            Message::CommitHistory(item) => {
                if let Some(Dialog::Commit(commit)) = &mut self.dialog {
                    commit.choose_history(item);
                }
            }

            Message::ToggleConsole => Self::toggle_console(shared, events),
            Message::ClearConsole => self.console.clear(),
            Message::DragStart => self.drag = Some(Drag { start: None }),
            Message::DragMove(y) => {
                if let Some(drag) = &mut self.drag {
                    match drag.start {
                        None => drag.start = Some((y, self.console_height)),
                        Some((start_y, start_height)) => {
                            self.console_height = (start_height + start_y - y).clamp(80.0, 900.0);
                        }
                    }
                }
            }
            Message::DragEnd => self.drag = None,
            Message::DismissError => self.error = None,
        }
        Task::none()
    }

    fn scan_done(&mut self, result: &Result<jobs::ScanOutput, String>, shared: &Shared) -> Task<Message> {
        self.phase = Phase::Idle;
        self.progress.clear();
        self.scan_handle = None;
        self.scan_cancel = None;
        let output = match result {
            Ok(output) => output,
            Err(e) => {
                self.report("Error scanning the repository", Some(e.clone()));
                self.update_sensitivity();
                return Task::none();
            }
        };
        let fresh = self.tree.nodes().len() <= 1;
        if fresh {
            self.tree = TreeModel::new(&output.tree);
        } else {
            self.tree.replace(&output.tree);
        }
        self.push_summary = output.push_summary.clone();
        self.update_sensitivity();
        let mut tasks = vec![self.reveal_cursor()];
        if std::mem::take(&mut self.auto_compare) {
            let paths = meld_vc::auto_compare_paths(&output.tree);
            tasks.push(self.compare(paths, shared));
        }
        Task::batch(tasks)
    }

    fn stop(&mut self) {
        if self.dialog.take().is_some() {
            return;
        }
        match self.phase {
            Phase::Scanning => {
                self.cancel_scan();
                self.update_sensitivity();
            }
            Phase::Loading => {
                // Results of the abandoned search are ignored.
                self.load_gen += 1;
                self.phase = Phase::Idle;
                self.progress.clear();
            }
            Phase::Idle => {}
        }
    }

    pub fn command(&mut self, command: Command, shared: &mut Shared, events: &mut Vec<Event>) -> Task<Message> {
        if command == Command::Stop {
            self.stop();
            return Task::none();
        }
        if self.dialog.is_some() {
            return Task::none();
        }
        match command {
            Command::Refresh => self.load(),
            Command::Activate => match self.tree.cursor() {
                Some(id) => self.activate(id, shared),
                None => Task::none(),
            },
            Command::Commit => self.action(Act::Commit, shared),
            Command::OpenExternal => self.action(Act::OpenExternal, shared),
            Command::Delete => self.action(Act::Delete, shared),
            Command::TogglePanel => {
                Self::toggle_console(shared, events);
                Task::none()
            }
            Command::NextChange => self.go_to_change(true),
            Command::PreviousChange => self.go_to_change(false),
            _ => Task::none(),
        }
    }

    pub fn request_close(&mut self, _shared: &mut Shared, events: &mut Vec<Event>) -> Task<Message> {
        self.cancel_scan();
        events.push(Event::Close { status: 0 });
        Task::none()
    }

    pub fn settings_changed(&mut self, shared: &Shared) -> Task<Message> {
        let filters = StatusFilters::from_keys(&shared.settings.vc_status_filters);
        if filters != self.filters {
            self.filters = filters;
            return self.scan(true);
        }
        Task::none()
    }

    pub fn subscription(&self) -> Subscription<Message> {
        if self.drag.is_none() {
            return Subscription::none();
        }
        iced::event::listen_with(|event, _status, _window| match event {
            iced::Event::Mouse(mouse::Event::CursorMoved { position }) => Some(Message::DragMove(position.y)),
            iced::Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) => Some(Message::DragEnd),
            _ => None,
        })
    }

    // ----- View -----

    pub fn toolbar<'a>(&'a self, shared: &'a Shared) -> Element<'a, Message> {
        let scanning = self.phase != Phase::Idle;
        let refresh = if scanning {
            ui::tool_button(icon::STOP, "Stop the scan (Escape)", Some(Message::Stop))
        } else {
            ui::tool_button(icon::REFRESH, "Refresh (Ctrl+R)", Some(Message::Refresh))
        };
        let console_tip = if shared.settings.vc_console_visible {
            "Hide the console output (F9)"
        } else {
            "Show the console output (F9)"
        };
        row![
            space::horizontal(),
            ui::tool_button(
                icon::UP,
                "Go to the previous change (Alt+Up)",
                self.tree.find_change(false).map(|_| Message::PreviousChange)
            ),
            ui::tool_button(
                icon::DOWN,
                "Go to the next change (Alt+Down)",
                self.tree.find_change(true).map(|_| Message::NextChange)
            ),
            refresh,
            ui::tool_button(">_", console_tip, Some(Message::ToggleConsole)),
        ]
        .spacing(2)
        .align_y(iced::Center)
        .into()
    }

    pub fn menu_items(&self, shared: &Shared) -> Vec<menu::Item<Message>> {
        let action = |label: &str, shortcut: Option<&'static str>, act: Act| match self.act_message(act) {
            Some(message) => menu::Item::action(label, shortcut, message),
            None => menu::Item::Action {
                label: label.to_string(),
                shortcut,
                message: None,
            },
        };
        let mut items = vec![
            menu::Item::Heading("Version control".into()),
            action("Compare", Some("Return"), Act::Compare),
            action("Commit…", Some("Ctrl+M"), Act::Commit),
            action("Update", None, Act::Update),
            action("Push…", None, Act::Push),
            menu::Item::Separator,
            action("Add", None, Act::Add),
            action("Unstage", None, Act::Unstage),
            action("Remove", None, Act::Remove),
            action("Revert…", None, Act::Revert),
            action("Resolve Conflict", None, Act::Resolve),
            action("Delete", Some("Delete"), Act::Delete),
            action("Open Externally", Some("Ctrl+Shift+O"), Act::OpenExternal),
            menu::Item::Separator,
            menu::Item::action("Refresh Comparison", Some("Ctrl+R"), Message::Refresh),
            menu::Item::Separator,
            menu::Item::Heading("Version filters".into()),
        ];
        for filter in Filter::STATES {
            items.push(menu::Item::toggle(filter.label(), filter.get(&self.filters), Message::ToggleFilter(filter)));
        }
        items.push(menu::Item::toggle(
            Filter::Flatten.label(),
            self.filters.flatten,
            Message::ToggleFilter(Filter::Flatten),
        ));
        items.push(menu::Item::Separator);
        items.push(menu::Item::toggle(
            "Version Control Console",
            shared.settings.vc_console_visible,
            Message::ToggleConsole,
        ));
        items.push(menu::Item::action("Clear Console", None, Message::ClearConsole));
        items
    }

    pub fn view<'a>(&'a self, shared: &'a Shared) -> Element<'a, Message> {
        let mut content = column![self.action_bar(), ui::separator(false), self.filter_bar(), ui::separator(false)];
        if let Some((primary, secondary)) = &self.error {
            content = content.push(ui::banner(
                ui::BannerKind::Error,
                primary.as_str(),
                secondary.clone(),
                vec![("Dismiss".to_string(), Message::DismissError)],
            ));
        }
        let tree = keys::key_handler(self.tree_view(shared), nav_key, Message::Modifiers);
        content = content.push(tree);
        if shared.settings.vc_console_visible {
            content = content.push(self.console_view(shared));
        }
        let base: Element<'a, Message> = content.height(Length::Fill).into();
        match &self.dialog {
            Some(dialog) => ui::modal::modal(base, dialogs::view(dialog, shared, &self.location), Message::CloseDialog),
            None => base,
        }
    }

    fn action_bar(&self) -> Element<'_, Message> {
        let tool = |glyph: &'static str, tip: &'static str, act: Act| ui::tool_button(glyph, tip, self.act_message(act));
        let commit = ui::text_button("Commit…", self.act_message(Act::Commit));
        let commit = iced::widget::tooltip(
            commit,
            container(text("Commit changes to version control (Ctrl+M)").size(13))
                .padding([4, 8])
                .style(ui::tooltip_box),
            iced::widget::tooltip::Position::Bottom,
        );
        let actions = row![
            commit,
            ui::separator(true),
            tool("⇣", "Pull remote changes to the local repository", Act::Update),
            tool("⇡", "Push local changes to remote", Act::Push),
            ui::separator(true),
            tool("+", "Add to version control", Act::Add),
            tool(icon::UNDO, "Remove staged changes", Act::Unstage),
            tool("−", "Remove from version control", Act::Remove),
            tool("⟲", "Revert working copy to original state", Act::Revert),
            tool("✓", "Mark as resolved in version control", Act::Resolve),
            ui::separator(true),
            tool("⇄", "Compare selected files (Return)", Act::Compare),
            tool("↗", "Open in an external editor (Ctrl+Shift+O)", Act::OpenExternal),
            tool("🗑", "Delete the selected files from the working copy", Act::Delete),
        ]
        .spacing(2)
        .height(28)
        .align_y(iced::Center);

        let location = iced::widget::tooltip(
            row![
                ui::tool_button(icon::FOLDER, "Open version controlled folder", Some(Message::ChooseLocation)),
                text(display_path(&self.location)).size(14).wrapping(text::Wrapping::None),
            ]
            .spacing(4)
            .align_y(iced::Center),
            container(text(self.location.display().to_string()).size(13))
                .padding([4, 8])
                .style(ui::tooltip_box),
            iced::widget::tooltip::Position::Bottom,
        );

        let chooser: Element<'_, Message> = match &self.choices {
            Some(choices) if choices.choices.len() > 1 => {
                let items: Vec<VcItem> = choices
                    .choices
                    .iter()
                    .enumerate()
                    .map(|(index, c)| VcItem {
                        index,
                        label: c.label.clone(),
                    })
                    .collect();
                let selected = items.get(self.active_choice).cloned();
                iced::widget::tooltip(
                    pick_list(items, selected, Message::ChooseVc)
                        .text_size(13)
                        .padding([4, 8])
                        .width(Length::Shrink),
                    container(text(choices.tooltip).size(13)).padding([4, 8]).style(ui::tooltip_box),
                    // Not below: the tooltip would cover the open menu.
                    iced::widget::tooltip::Position::Left,
                )
                .into()
            }
            Some(choices) => text(choices.choices.first().map_or("", |c| c.label.as_str()))
                .size(13)
                .into(),
            None => text("…").size(13).style(ui::dim_text).into(),
        };

        container(
            row![
                actions,
                space::horizontal().width(Length::FillPortion(1)),
                container(location).clip(true).width(Length::FillPortion(4)).center_x(Length::FillPortion(4)),
                space::horizontal().width(Length::FillPortion(1)),
                chooser,
            ]
            .spacing(8)
            .align_y(iced::Center),
        )
        .padding([4, 8])
        .width(Length::Fill)
        .into()
    }

    fn filter_bar(&self) -> Element<'_, Message> {
        let mut filters = row![text("Show:").size(13).style(ui::dim_text)].spacing(14).align_y(iced::Center);
        for filter in Filter::STATES {
            filters = filters.push(
                checkbox(filter.get(&self.filters))
                    .label(filter.label())
                    .size(14)
                    .text_size(13)
                    .on_toggle(move |_| Message::ToggleFilter(filter)),
            );
        }
        filters = filters.push(ui::separator(true));
        filters = filters.push(
            checkbox(self.filters.flatten)
                .label(Filter::Flatten.label())
                .size(14)
                .text_size(13)
                .on_toggle(|_| Message::ToggleFilter(Filter::Flatten)),
        );

        let status = if self.phase != Phase::Idle && !self.progress.is_empty() {
            format!("{}…", self.progress)
        } else if self.running > 0 {
            "Running command…".to_string()
        } else if self.vc.is_some() {
            match self.tree.entry_count() {
                1 => "1 item".to_string(),
                n => format!("{n} items"),
            }
        } else {
            String::new()
        };
        container(
            row![
                filters.height(20),
                space::horizontal(),
                text(status).size(13).style(ui::dim_text).wrapping(text::Wrapping::None),
            ]
            .spacing(8)
            .align_y(iced::Center),
        )
        .padding([4, 10])
        .width(Length::Fill)
        .into()
    }

    fn tree_view<'a>(&'a self, shared: &'a Shared) -> Element<'a, Message> {
        let header_cell = |label: &'a str| text(label).size(13).font(ui::bold());
        let header = container(
            row![
                container(header_cell("Name")).width(Length::FillPortion(5)),
                container(header_cell("Location")).width(Length::FillPortion(3)),
                container(header_cell("Status")).width(STATUS_WIDTH),
                container(header_cell("Extra")).width(Length::FillPortion(3)),
            ]
            .spacing(8),
        )
        .padding([4, 10])
        .width(Length::Fill)
        .style(ui::header_bar);

        let visible = self.tree.visible();
        let height = if self.viewport_height > 0.0 { self.viewport_height } else { 1200.0 };
        let first = ((self.scroll_y / ROW_HEIGHT).floor().max(0.0) as usize).min(visible.len());
        let last = (first + (height / ROW_HEIGHT).ceil() as usize + 2).min(visible.len());
        let colors = shared.style.status_colors();

        let mut rows = column![space::vertical().height(first as f32 * ROW_HEIGHT)].width(Length::Fill);
        for (row, &id) in visible.iter().enumerate().take(last).skip(first) {
            rows = rows.push(self.row_view(row, id, &colors));
        }
        rows = rows.push(space::vertical().height((visible.len() - last) as f32 * ROW_HEIGHT));

        let body = scrollable(rows)
            .id(TREE_SCROLL)
            .on_scroll(Message::Scrolled)
            .width(Length::Fill)
            .height(Length::Fill);
        column![header, ui::separator(false), body].height(Length::Fill).into()
    }

    fn row_view<'a>(&'a self, row: usize, id: NodeId, colors: &StatusColors) -> Element<'a, Message> {
        let node = self.tree.node(id);
        let state = node.state();
        let look = StateLook::of(state, colors);

        let expander: Element<'a, Message> = if self.tree.has_children(id) {
            button(text(if self.tree.is_expanded(id) { "▾" } else { "▸" }).size(13).center())
                .padding(0)
                .width(16)
                .style(ui::flat_button)
                .on_press(Message::ToggleRow(id))
                .into()
        } else {
            space::horizontal().width(16).into()
        };

        let (glyph, label, location, status, extra) = match &node.kind {
            NodeKind::Entry(entry) => {
                let label = entry
                    .path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| entry.name.clone());
                let extra = if id == 0 {
                    self.push_summary.clone().unwrap_or_default()
                } else {
                    entry.options.clone()
                };
                (
                    if node.is_folder() { "▣" } else { "▢" },
                    label,
                    entry.display_location(&self.location).unwrap_or_default(),
                    entry.status(),
                    extra,
                )
            }
            NodeKind::Empty(label) => ("", label.clone(), String::new(), "", String::new()),
            NodeKind::Error(label) => ("⚠", label.clone(), String::new(), state.label(), String::new()),
        };

        let mut font = Font::DEFAULT;
        if look.bold {
            font.weight = iced::font::Weight::Bold;
        }
        if look.italic {
            font.style = iced::font::Style::Italic;
        }
        let name = rich_text::<(), Message, Theme, iced::Renderer>([span(label)
            .color_maybe(look.color)
            .font(font)
            .strikethrough(look.strikethrough)])
        .size(14)
        .wrapping(text::Wrapping::None);
        let glyph = text(glyph).size(13).color_maybe(look.icon_tint).width(16);

        let name_cell = row![
            space::horizontal().width(node.depth as f32 * INDENT),
            expander,
            glyph,
            name,
        ]
        .spacing(4)
        .align_y(iced::Center);

        let cell = |content: Element<'a, Message>, width: Length| container(content).width(width).clip(true);
        let status = text(status).size(13).wrapping(text::Wrapping::None).font(if state.is_emphasized() {
            ui::bold()
        } else {
            Font::DEFAULT
        });
        let line = row![
            cell(name_cell.into(), Length::FillPortion(5)),
            cell(
                text(location).size(13).style(ui::dim_text).wrapping(text::Wrapping::None).into(),
                Length::FillPortion(3)
            ),
            cell(status.into(), Length::Fixed(STATUS_WIDTH)),
            cell(
                text(extra).size(13).wrapping(text::Wrapping::None).into(),
                Length::FillPortion(3)
            ),
        ]
        .spacing(8)
        .height(ROW_HEIGHT)
        .align_y(iced::Center);

        let selected = self.tree.is_selected(id);
        let cursor = self.tree.cursor() == Some(id);
        let background = container(line)
            .padding([0, 10])
            .width(Length::Fill)
            .style(move |theme: &Theme| row_style(theme, selected, cursor));
        mouse_area(background)
            .on_press(Message::RowPressed(row))
            .on_double_click(Message::RowActivated(row))
            .into()
    }

    fn console_view<'a>(&'a self, shared: &'a Shared) -> Element<'a, Message> {
        let handle = mouse_area(
            container(ui::separator(false))
                .padding([3, 0])
                .width(Length::Fill),
        )
        .interaction(mouse::Interaction::ResizingVertically)
        .on_press(Message::DragStart);

        let font = shared.mono_font();
        let mut lines = column![].spacing(0).width(Length::Fill);
        for line in &self.console {
            let mut span_font = font;
            let mut color = None;
            match line.kind {
                ConsoleKind::Command => span_font.weight = iced::font::Weight::Bold,
                ConsoleKind::Output => {}
                ConsoleKind::Error => color = Some(Color::from_rgb8(0xcc, 0x00, 0x00)),
            }
            let text = line.text.strip_suffix('\n').unwrap_or(&line.text);
            lines = lines.push(
                rich_text::<(), Message, Theme, iced::Renderer>([span(text).font(span_font).color_maybe(color)])
                    .size(13),
            );
        }
        let output = scrollable(container(lines).padding([4, 8]))
            .anchor_bottom()
            .width(Length::Fill)
            .height(Length::Fill);

        column![
            handle,
            row![
                text("Console output").size(13).font(ui::bold()),
                space::horizontal(),
                ui::text_button("Clear", (!self.console.is_empty()).then_some(Message::ClearConsole)),
            ]
            .padding([0, 8])
            .align_y(iced::Center),
            container(output)
                .width(Length::Fill)
                .height(Length::Fill)
                .padding([0, 6])
                .style(console_style),
        ]
        .spacing(2)
        .padding(iced::Padding {
            bottom: 6.0,
            ..iced::Padding::ZERO
        })
        .height(self.console_height)
        .into()
    }
}

/// Maps unconsumed key presses over the tree to navigation.
fn nav_key(key: &Key, modifiers: Modifiers) -> Option<Message> {
    if modifiers.alt() || modifiers.logo() {
        return None;
    }
    let extend = modifiers.shift();
    let nav = match key {
        Key::Named(Named::ArrowUp) => Nav::Up,
        Key::Named(Named::ArrowDown) => Nav::Down,
        Key::Named(Named::PageUp) if !modifiers.command() => Nav::PageUp,
        Key::Named(Named::PageDown) if !modifiers.command() => Nav::PageDown,
        Key::Named(Named::Home) => Nav::Home,
        Key::Named(Named::End) => Nav::End,
        Key::Named(Named::ArrowLeft) if !extend => Nav::Left,
        Key::Named(Named::ArrowRight) if !extend => Nav::Right,
        Key::Character(c) if modifiers.command() && c.eq_ignore_ascii_case("a") => Nav::SelectAll,
        _ => return None,
    };
    Some(Message::Nav(nav, extend))
}

/// How a row in a given state is drawn (`tree.py` `text_attributes` and
/// `icon_details`).
struct StateLook {
    color: Option<Color>,
    icon_tint: Option<Color>,
    italic: bool,
    bold: bool,
    strikethrough: bool,
}

impl StateLook {
    fn of(state: State, colors: &StatusColors) -> StateLook {
        let c = shared::color;
        let (color, italic, bold, strikethrough) = match state {
            State::Ignored | State::Unversioned => (Some(c(colors.unknown)), false, false, false),
            State::Normal => (None, false, false, false),
            State::NoChange | State::Spinner => (None, true, false, false),
            State::Error => (Some(c(colors.error)), false, true, false),
            State::Empty => (Some(c(colors.unknown)), true, false, false),
            State::New => (Some(c(colors.new)), false, true, false),
            State::Modified => (Some(c(colors.modified)), false, true, false),
            State::Renamed => (Some(c(colors.modified)), false, false, false),
            State::Conflict => (Some(c(colors.conflict)), false, true, false),
            State::Removed | State::Missing => (Some(c(colors.removed)), false, true, true),
            State::NonExist => (Some(c(colors.unknown)), false, false, true),
        };
        let icon_tint = match state {
            State::New => Some(c(colors.new)),
            State::Modified | State::Renamed => Some(c(colors.modified)),
            State::Conflict => Some(c(colors.conflict)),
            State::Removed => Some(c(colors.removed)),
            State::Missing | State::NonExist => Some(c(colors.unknown)),
            State::Error => Some(c(colors.error)),
            _ => None,
        };
        StateLook {
            color,
            icon_tint,
            italic,
            bold,
            strikethrough,
        }
    }
}

fn row_style(theme: &Theme, selected: bool, cursor: bool) -> container::Style {
    let palette = theme.extended_palette();
    let background = selected.then(|| Background::Color(palette.primary.weak.color.scale_alpha(0.45)));
    let border = if cursor {
        Border::default().width(1).color(palette.primary.base.color.scale_alpha(0.6))
    } else {
        Border::default()
    };
    container::Style {
        background,
        border,
        ..container::Style::default()
    }
}

fn console_style(theme: &Theme) -> container::Style {
    let palette = theme.extended_palette();
    container::Style {
        background: Some(Background::Color(palette.background.weak.color.scale_alpha(0.4))),
        border: Border::default().rounded(4).width(1).color(palette.background.strong.color),
        ..container::Style::default()
    }
}

/// A path for display, with the home folder shown as `~`.
fn display_path(path: &Path) -> String {
    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from)
        && let Ok(rel) = path.strip_prefix(&home)
    {
        return if rel.as_os_str().is_empty() {
            "~".into()
        } else {
            format!("~/{}", rel.display())
        };
    }
    path.display().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filters_toggle() {
        let mut filters = StatusFilters::default();
        Filter::Normal.toggle(&mut filters);
        Filter::Flatten.toggle(&mut filters);
        assert_eq!(filters.keys(), ["modified", "normal"]);
        assert!(Filter::Normal.get(&filters));
    }

    #[test]
    fn navigation_keys() {
        let nav = |key: Key, mods: Modifiers| match nav_key(&key, mods) {
            Some(Message::Nav(nav, extend)) => Some((nav, extend)),
            _ => None,
        };
        assert_eq!(nav(Key::Named(Named::ArrowDown), Modifiers::empty()), Some((Nav::Down, false)));
        assert_eq!(nav(Key::Named(Named::ArrowUp), Modifiers::SHIFT), Some((Nav::Up, true)));
        // Alt+Up is "previous change", handled by the window.
        assert_eq!(nav(Key::Named(Named::ArrowUp), Modifiers::ALT), None);
        assert_eq!(nav(Key::Character("a".into()), Modifiers::CTRL), Some((Nav::SelectAll, false)));
        assert_eq!(nav(Key::Character("a".into()), Modifiers::empty()), None);
        assert_eq!(nav(Key::Named(Named::PageDown), Modifiers::CTRL), None);
    }

    #[test]
    fn action_gating() {
        let s = ActionSensitivity {
            compare: true,
            commit: true,
            ..ActionSensitivity::default()
        };
        assert!(Act::Compare.enabled(&s) && Act::Commit.enabled(&s));
        assert!(!Act::Push.enabled(&s));
        assert!(Act::Commit.mutates() && !Act::Compare.mutates());
    }

    #[test]
    fn home_is_abbreviated() {
        if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
            assert_eq!(display_path(&home.join("src/x")), "~/src/x");
        }
        assert_eq!(display_path(Path::new("/nonexistent-root/x")), "/nonexistent-root/x");
    }

    /// End to end against a real Git repository, when git is installed:
    /// detection, scanning and the tree contents.
    #[test]
    fn scans_a_git_repository() {
        if !VcKind::Git.is_installed() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let git = |args: &[&str]| {
            std::process::Command::new("git")
                .args(args)
                .current_dir(root)
                .env("GIT_AUTHOR_NAME", "t")
                .env("GIT_AUTHOR_EMAIL", "t@t")
                .env("GIT_COMMITTER_NAME", "t")
                .env("GIT_COMMITTER_EMAIL", "t@t")
                .output()
                .unwrap()
        };
        git(&["init", "-q"]);
        std::fs::write(root.join("a.txt"), "one\n").unwrap();
        std::fs::create_dir(root.join("sub")).unwrap();
        std::fs::write(root.join("sub/b.txt"), "two\n").unwrap();
        git(&["add", "."]);
        git(&["commit", "-q", "-m", "init"]);
        std::fs::write(root.join("a.txt"), "changed\n").unwrap();
        std::fs::write(root.join("new.txt"), "new\n").unwrap();

        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let loaded = runtime.block_on(jobs::load(root.to_path_buf(), None)).unwrap();
        let opened = loaded.vc.unwrap().take().unwrap();
        assert_eq!(opened.worker.kind(), VcKind::Git);
        let vc: SharedVc = Arc::new(Mutex::new(opened.worker));

        let filters = StatusFilters::from_keys(&["flatten", "modified", "unknown"]);
        let events: Vec<ScanEvent> = runtime.block_on(async {
            use iced::futures::StreamExt;
            jobs::scan(vc.clone(), filters, true, Arc::new(AtomicBool::new(false)))
                .collect()
                .await
        });
        let Some(ScanEvent::Done(result)) = events.last() else {
            panic!("scan did not finish");
        };
        let output = result.as_ref().as_ref().unwrap();
        let model = TreeModel::new(&output.tree);
        let rows: Vec<(String, State)> = model
            .visible()
            .iter()
            .filter_map(|&id| model.node(id).entry().map(|e| (e.name.clone(), e.state)))
            .collect();
        assert_eq!(rows[1..], [("a.txt".to_string(), State::Modified), ("new.txt".to_string(), State::Unversioned)]);

        let diffs = runtime.block_on(jobs::prepare_diffs(vc, vec![root.join("a.txt")], DiffOptions::default()));
        match &diffs[0] {
            Ok(DiffRequest::Files { paths, temp_files, .. }) => {
                assert_eq!(std::fs::read_to_string(&paths[0]).unwrap(), "one\n");
                for t in temp_files {
                    let mut perms = std::fs::metadata(t).unwrap().permissions();
                    #[allow(clippy::permissions_set_readonly_false)]
                    perms.set_readonly(false);
                    std::fs::set_permissions(t, perms).unwrap();
                    std::fs::remove_file(t).unwrap();
                }
            }
            other => panic!("unexpected diff {other:?}"),
        }
    }
}
