//! Git Compare: compare any two revisions, with why each change was made
//! (port of `meld/gitcompare.py`).
//!
//! A tab with the changed files on the left and a two-way file comparison
//! on the right ([`diffhost`]), extended with the Why column
//! ([`whycolumn`]). The header has the searchable Base and Target pickers
//! ([`picker`]). All git and Claude Code work runs on background threads
//! ([`jobs`]) and comes back as messages; nothing here blocks the UI.
//!
//! Labels that may not fit (file names, revisions, the repository) are
//! shortened with “…” like GTK's ellipsizing labels ([`ellipsis`]); the
//! file list and the Why column are resized with [`divider`]s.
//!
//! The comparison follows the repository when it involves uncommitted
//! changes or refs by name: the repository's refs and index (and the open
//! working-tree file) are checked every [`POLL_INTERVAL`], files that other
//! tabs save are reported through [`GitCompare::file_changed`], and a
//! change during a comparison compares again once it finished.

mod diffhost;
mod divider;
mod ellipsis;
mod filelist;
mod jobs;
mod picker;
mod popover;
mod whycolumn;

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use iced::widget::{button, column, container, row, scrollable, space, text, text_editor, tooltip};
use iced::{Background, Element, Length, Subscription, Task, Theme};
use meld_ailens::picker::{apply_pick, revisions_from_args};
use meld_ailens::{
    CancelToken, Card, CompareSession, Error, FileEntry, FileRow, FileView, GitOptions, PickerEntry,
    PickerSide, RepositoryInfo, Revision, RevisionKind, SessionConfig, build_file_tree,
};
use meld_config::Settings;

use self::diffhost::{DiffHost, EmbeddedFile};
use self::divider::divider;
use self::ellipsis::{Mode, ellipsized};
use self::filelist::ListRow;
use self::jobs::{Jobs, Opaque, Ticket};
use self::picker::Picker;
use self::popover::popover;
use self::whycolumn::CardStates;
use crate::command::Command;
use crate::comparison::{Comparison, FileExtras};
use crate::shared::Shared;
use crate::ui::{self, BannerKind, icon, menu};
use crate::views::Event;

/// How often the repository is checked for changes.
const POLL_INTERVAL: Duration = Duration::from_secs(1);

/// The settings a comparison reads (`config_from_settings`); empty texts
/// fall back to the defaults.
pub fn config_from_settings(settings: &Settings) -> SessionConfig {
    let defaults = SessionConfig::default();
    let or = |value: &str, default: &str| {
        let value = value.trim();
        if value.is_empty() { default.to_string() } else { value.to_string() }
    };
    SessionConfig {
        claude_path: or(&settings.ailens_claude_path, &defaults.claude_path),
        infer_enabled: settings.ailens_infer_enabled,
        infer_model: or(&settings.ailens_infer_model, &defaults.infer_model),
        build_model: or(&settings.ailens_build_model, &defaults.build_model),
        build_folder: or(&settings.ailens_build_folder, &defaults.build_folder),
        notes_ref: or(&settings.ailens_notes_ref, &defaults.notes_ref),
        author_pattern: Some(settings.ailens_author_pattern.trim().to_string()).filter(|p| !p.is_empty()),
        ..defaults
    }
}

/// What the area right of the file list shows.
#[derive(Debug, Clone, PartialEq)]
enum Content {
    Diff,
    Message {
        icon: &'static str,
        title: String,
        description: Option<String>,
        /// Offer the folder chooser (no repository yet).
        choose: bool,
    },
}

#[derive(Debug, Clone)]
enum BannerAction {
    Open(PathBuf),
}

struct Banner {
    kind: BannerKind,
    text: String,
    action: Option<(String, BannerAction)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DragTarget {
    FileList,
    WhyColumn,
}

struct Drag {
    target: DragTarget,
    start_width: f32,
    /// Where the pointer pressed the divider.
    origin_x: f32,
    moved: bool,
}

/// The width of the panel `target` while its divider, pressed at
/// `origin_x` when the panel was `start_width` wide, is dragged to `x`. The
/// file list is left of its divider; the Why column right of its own.
fn dragged_width(target: DragTarget, start_width: f32, origin_x: f32, x: f32) -> f32 {
    let dx = x - origin_x;
    match target {
        DragTarget::FileList => {
            let (lo, hi) = meld_config::settings::AILENS_FILE_LIST_WIDTH_RANGE;
            (start_width + dx).round().clamp(lo as f32, hi as f32)
        }
        DragTarget::WhyColumn => (start_width - dx).clamp(whycolumn::MIN_WIDTH, whycolumn::MAX_WIDTH),
    }
}

/// A commit's full message, shown when its card's SHA is clicked.
struct CommitPopup {
    sha: String,
    /// Read-only, but selectable and copyable.
    message: text_editor::Content,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Popup {
    Picker(PickerSide),
    More,
}

pub struct GitCompare {
    location: PathBuf,
    config: SessionConfig,
    info: Option<Arc<RepositoryInfo>>,
    session: Option<Arc<CompareSession>>,
    base: Revision,
    target: Revision,
    title: String,
    tooltip: String,
    repo_label: String,
    repo_tooltip: String,
    files_title: String,
    relation: String,
    base_picker: Picker,
    target_picker: Picker,
    popup: Option<Popup>,
    busy: Option<String>,
    jobs: Jobs,
    banner: Option<Banner>,
    entries: Vec<FileEntry>,
    tree: Vec<FileRow>,
    rows: Vec<ListRow>,
    collapsed: HashSet<String>,
    current: Option<FileEntry>,
    view: Option<FileView>,
    view_generation: u64,
    content: Content,
    diff: DiffHost,
    temp: Option<tempfile::TempDir>,
    temp_count: u64,
    cards: CardStates,
    commit: Option<CommitPopup>,
    why_width: f32,
    drag: Option<Drag>,
    files_scroll_id: iced::widget::Id,
    files_viewport: (f32, f32),
    /// `ailens-file-row-spacing`, for scrolling rows into view
    row_spacing_cache: f32,
    scroll_pending: bool,
    /// The repository changed while a comparison was running: compare
    /// again once it finished.
    reload_queued: bool,
    /// Another tab saved a file of the repository.
    changed_elsewhere: bool,
    watch: Vec<PathBuf>,
    watch_signature: Option<Vec<Option<(SystemTime, u64)>>>,
    disk_signature: Option<(PathBuf, Option<(SystemTime, u64)>)>,
}

#[derive(Debug, Clone)]
pub enum Outcome {
    Opened(Result<(Opaque<RepositoryInfo>, Vec<PathBuf>), Error>),
    Pickers(Result<(Vec<PickerEntry>, Vec<PickerEntry>), Error>),
    Search(PickerSide, String, Result<Vec<PickerEntry>, Error>),
    Compared(Option<String>, Result<(Opaque<CompareSession>, Vec<FileEntry>), Error>),
    File(FileEntry, Result<Box<FileView>, Error>),
    Explained(String, Result<(), Error>),
    Cards(u64, Result<Vec<Card>, Error>),
    ExplainedAll(Result<usize, Error>),
    Built(Result<PathBuf, Error>),
    AgentsSetUp(Result<Vec<String>, Error>),
    CommitMessage(String, Result<String, Error>),
}

#[derive(Debug, Clone)]
pub enum Message {
    Job(Ticket, Outcome),
    Progress(Ticket, String),
    Diff(diffhost::Message),
    ChooseRepository,
    RepositoryChosen(Option<PathBuf>),
    TogglePopup(PopupKind),
    ClosePopup,
    Picker(PickerSide, picker::Message),
    SearchDue(PickerSide, u64),
    Swap,
    Reload,
    ExplainAll,
    Build,
    SetUpAgents,
    OpenBuilds,
    OpenFile,
    BannerAction,
    DismissBanner,
    SelectFile(FileEntry),
    ToggleFolder(String),
    FilesScrolled(scrollable::Viewport),
    ExplainCard(String),
    ShowCommit(String),
    CommitAction(text_editor::Action),
    CloseCommit,
    CompareWithParent(String),
    OpenBuild(String),
    HoverCard(Option<String>),
    ExpandCard(String),
    StartDrag(DragTarget, f32),
    DragMove(f32),
    DragEnd,
    ResetWhyWidth,
    Poll,
}

/// Which popup a header button opens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PopupKind {
    Base,
    Target,
    More,
}

fn side_name(side: PickerSide) -> &'static str {
    match side {
        PickerSide::Base => "base",
        PickerSide::Target => "target",
    }
}

fn file_signature(path: &Path) -> Option<(SystemTime, u64)> {
    let meta = std::fs::metadata(path).ok()?;
    Some((meta.modified().ok()?, meta.len()))
}

/// 1-based inclusive ranges as 0-based half-open ones.
fn zero_based(ranges: &[meld_ailens::LineRange]) -> Vec<std::ops::Range<usize>> {
    ranges
        .iter()
        .map(|r| r.start.saturating_sub(1)..r.end.max(r.start))
        .collect()
}

impl GitCompare {
    pub fn new(location: PathBuf, base: Option<String>, target: Option<String>, shared: &mut Shared) -> (Self, Task<Message>) {
        let mut tab = GitCompare {
            location: location.clone(),
            config: config_from_settings(&shared.settings),
            info: None,
            session: None,
            base: Revision::commit("HEAD"),
            target: Revision::working_tree(None),
            title: "Git Compare".into(),
            tooltip: location.display().to_string(),
            repo_label: "No repository".into(),
            repo_tooltip: "Choose a repository or worktree".into(),
            files_title: String::new(),
            relation: String::new(),
            base_picker: Picker::new(PickerSide::Base),
            target_picker: Picker::new(PickerSide::Target),
            popup: None,
            busy: None,
            jobs: Jobs::default(),
            banner: None,
            entries: Vec::new(),
            tree: Vec::new(),
            rows: Vec::new(),
            collapsed: HashSet::new(),
            current: None,
            view: None,
            view_generation: 0,
            content: Content::Message {
                icon: "📁",
                title: String::new(),
                description: None,
                choose: false,
            },
            diff: DiffHost::embedded(shared),
            temp: None,
            temp_count: 0,
            cards: CardStates::default(),
            commit: None,
            why_width: whycolumn::DEFAULT_WIDTH,
            drag: None,
            files_scroll_id: iced::widget::Id::unique(),
            files_viewport: (0.0, 0.0),
            row_spacing_cache: filelist::row_spacing(shared.settings.ailens_file_row_spacing),
            scroll_pending: false,
            reload_queued: false,
            changed_elsewhere: false,
            watch: Vec::new(),
            watch_signature: None,
            disk_signature: None,
        };
        match revisions_from_args(base.as_deref(), target.as_deref()) {
            Ok((base, target)) => {
                tab.base = base;
                tab.target = target;
            }
            Err(err) => tab.show_banner(BannerKind::Error, err, None),
        }
        tab.update_picker_labels(false);
        let task = if location.as_os_str().is_empty() {
            tab.show_no_repository(None);
            Task::none()
        } else {
            tab.set_location(location)
        };
        (tab, task)
    }

    pub fn title(&self) -> String {
        self.title.clone()
    }

    pub fn tooltip(&self) -> String {
        self.tooltip.clone()
    }

    pub fn unsaved(&self) -> Vec<String> {
        self.diff.unsaved()
    }

    // -- jobs --

    /// Runs `work` in the background as job `name`; its result comes back
    /// as `wrap(result)` unless a newer job of the name replaced it.
    fn run<T, W>(
        &mut self,
        name: &str,
        exclusive: bool,
        work: W,
        wrap: impl Fn(Result<T, Error>) -> Outcome + Send + Sync + 'static,
    ) -> Task<Message>
    where
        T: Send + 'static,
        W: FnOnce(&CancelToken, &dyn Fn(&str)) -> Result<T, Error> + Send + 'static,
    {
        let (ticket, cancel) = self.jobs.start(name, exclusive);
        jobs::spawn(cancel, work).map(move |event| match event {
            jobs::Event::Progress(text) => Message::Progress(ticket.clone(), text),
            jobs::Event::Done(result) => Message::Job(ticket.clone(), wrap(result)),
        })
    }

    // -- loading --

    /// Open the repository that `path` is in and compare base → target.
    fn set_location(&mut self, path: PathBuf) -> Task<Message> {
        self.location = path.clone();
        self.repo_label = path.display().to_string();
        self.show_message("⌛", "Opening repository…", None);
        self.run(
            "open",
            true,
            move |cancel, _| {
                let info = RepositoryInfo::open(&path, &GitOptions::default(), cancel)?;
                let watch = info.watch_paths();
                Ok((Opaque(Arc::new(info)), watch))
            },
            Outcome::Opened,
        )
    }

    fn load_pickers(&mut self) -> Task<Message> {
        let Some(info) = self.info.clone() else {
            return Task::none();
        };
        self.run(
            "picker",
            true,
            move |cancel, _| {
                Ok((
                    info.picker_entries(PickerSide::Base, cancel)?,
                    info.picker_entries(PickerSide::Target, cancel)?,
                ))
            },
            Outcome::Pickers,
        )
    }

    /// Resolve base and target again and reload the files, keeping the
    /// selected file if it is still changed.
    fn reload(&mut self, shared: &Shared) -> Task<Message> {
        let Some(info) = self.info.clone() else {
            return Task::none();
        };
        self.config = config_from_settings(&shared.settings);
        self.reload_queued = false;
        let keep = self.current.as_ref().map(|e| e.change.path.clone());
        let (base, target, config) = (self.base.clone(), self.target.clone(), self.config.clone());
        // Until the comparison resolves them, the pickers show what was
        // picked, not the previous comparison's revisions
        self.update_picker_labels(false);
        self.busy = Some("Comparing…".into());
        self.run(
            "compare",
            true,
            move |cancel, _| {
                let session = CompareSession::open(info, &base, &target, config, cancel)?;
                let files = session.files(cancel)?;
                Ok((Opaque(Arc::new(session)), files))
            },
            move |result| Outcome::Compared(keep.clone(), result),
        )
    }

    /// The pickers' labels and the relation text: from the comparison if
    /// `resolved`, else from the picked revisions.
    fn update_picker_labels(&mut self, resolved: bool) {
        match self.session.as_ref().filter(|_| resolved) {
            None => {
                self.base_picker.set_label(picker::describe_revision(&self.base), None);
                self.target_picker.set_label(picker::describe_revision(&self.target), None);
                self.relation.clear();
            }
            Some(session) => {
                self.base_picker
                    .set_label(session.base_label(), Some(session.base_tooltip()));
                self.target_picker
                    .set_label(session.target_label(), Some(session.target_tooltip()));
                self.relation = session.relation_text();
                let title = format!("{} → {}", session.base_label(), session.target_label());
                self.tooltip = format!(
                    "{}\n{title}",
                    self.info.as_ref().map(|i| i.root.display().to_string()).unwrap_or_default()
                );
                self.files_title = title.clone();
                self.title = title;
            }
        }
    }

    fn rebuild_rows(&mut self) {
        self.rows = filelist::visible_rows(&self.tree, &self.collapsed);
    }

    fn set_entries(&mut self, entries: Vec<FileEntry>, keep: Option<String>, shared: &mut Shared) -> Task<Message> {
        if !entries.is_empty() && entries == self.entries && self.current.is_some() {
            // Same files (e.g. something only touched the index): keep the
            // tree as it is and refresh the open file in place
            if let Some(current) = self.current.clone().filter(|c| self.entries.contains(c)) {
                return self.load_file(current);
            }
        }
        self.entries = entries;
        self.tree = build_file_tree(&self.entries);
        self.collapsed.clear();
        self.rebuild_rows();
        if self.entries.is_empty() {
            self.current = None;
            self.view = None;
            if let Some(session) = &self.session {
                let title = format!(
                    "No changes between {} and {}",
                    session.base_label(),
                    session.target_label()
                );
                self.show_message("✓", &title, None);
            }
            return Task::none();
        }
        let ordered = filelist::ordered_entries(&self.tree);
        let wanted = ordered
            .iter()
            .find(|e| keep.as_deref() == Some(e.change.path.as_str()))
            .or(ordered.first())
            .map(|e| (*e).clone());
        match wanted {
            Some(entry) => self.select_entry(entry, shared),
            None => Task::none(),
        }
    }

    fn select_entry(&mut self, entry: FileEntry, _shared: &mut Shared) -> Task<Message> {
        let mut opened = false;
        for key in filelist::ancestor_keys(&self.tree, &entry) {
            opened |= self.collapsed.remove(&key);
        }
        if opened {
            self.rebuild_rows();
        }
        let scroll = self.scroll_to_row(&entry);
        let load = if self.current.as_ref() != Some(&entry) || self.view.is_none() {
            self.load_file(entry)
        } else {
            Task::none()
        };
        Task::batch([scroll, load])
    }

    /// Scrolls the file list so that `entry`'s row is visible.
    fn scroll_to_row(&self, entry: &FileEntry) -> Task<Message> {
        let Some(index) = self
            .rows
            .iter()
            .position(|r| matches!(&r.kind, filelist::RowKind::File(e) if e == entry))
        else {
            return Task::none();
        };
        let (offset, height) = self.files_viewport;
        if height <= 0.0 {
            return Task::none();
        }
        // The spacing is cached from the settings, which live in Shared
        let row_height = filelist::row_height(self.row_spacing_cache);
        match filelist::scroll_target(&self.rows, index, row_height, offset, height) {
            Some(y) => iced::widget::operation::scroll_to(
                self.files_scroll_id.clone(),
                scrollable::AbsoluteOffset { x: None, y: Some(y) },
            ),
            None => Task::none(),
        }
    }

    fn load_file(&mut self, entry: FileEntry) -> Task<Message> {
        let Some(session) = self.session.clone() else {
            return Task::none();
        };
        self.current = Some(entry.clone());
        self.busy = Some(format!("Loading {}…", entry.change.path));
        let change = entry.change.clone();
        self.run(
            "file",
            true,
            move |cancel, _| session.file_view(&change, cancel).map(Box::new),
            move |result| Outcome::File(entry.clone(), result),
        )
    }

    fn show_file(&mut self, view: FileView, shared: &mut Shared) -> Task<Message> {
        let same = self.view.as_ref().is_some_and(|previous| {
            view.message.is_empty()
                && previous.message.is_empty()
                && previous.change() == view.change()
                && previous.base_text == view.base_text
                && previous.target_text == view.target_text
                && previous.base_label == view.base_label
                && previous.target_label == view.target_label
        }) && self.content == Content::Diff;
        self.view_generation += 1;
        if same {
            // Same contents: only the cards may have changed. Reloading the
            // files would lose the scroll position.
            self.view = Some(view);
            return Task::none();
        }
        self.cards.errors.clear();
        self.cards.expanded.clear();
        self.cards.hovered = None;
        if !view.message.is_empty() {
            let message = view.message.clone();
            self.view = Some(view);
            self.show_message("ℹ", &message, None);
            return Task::none();
        }
        let files = match self.write_temp_files(&view) {
            Ok(files) => files,
            Err(err) => {
                self.view = Some(view);
                self.show_message("⚠", &format!("Couldn’t write temporary files: {err}"), None);
                return Task::none();
            }
        };
        self.disk_signature = view
            .target_disk_path
            .as_ref()
            .map(|p| (p.clone(), file_signature(p)));
        self.view = Some(view);
        self.content = Content::Diff;
        self.scroll_pending = true;
        self.diff.set_files(files, shared).map(Message::Diff)
    }

    /// Both sides as files for the comparison: revisions in a temporary
    /// folder (read-only, named like the file so its language is
    /// recognised), the working-tree file itself when the target is on disk.
    fn write_temp_files(&mut self, view: &FileView) -> std::io::Result<Vec<EmbeddedFile>> {
        if self.temp.is_none() {
            self.temp = Some(tempfile::Builder::new().prefix("meld-gitcompare-").tempdir()?);
        }
        let root = self.temp.as_ref().map(|t| t.path().to_path_buf()).unwrap_or_default();
        // A fresh folder per file view: the comparison may still be reading
        // the previous files, and both sides can have the same name
        self.temp_count += 1;
        let folder = root.join(self.temp_count.to_string());
        let mut files = Vec::new();
        let sides = [
            ("base", &view.base_name, &view.base_text, &view.base_label, None),
            (
                "target",
                &view.target_name,
                &view.target_text,
                &view.target_label,
                view.target_disk_path.as_ref(),
            ),
        ];
        for (side, name, content, label, disk) in sides {
            if let Some(disk) = disk {
                files.push(EmbeddedFile {
                    path: disk.clone(),
                    label: label.clone(),
                    read_only: false,
                });
                continue;
            }
            let dir = folder.join(side);
            std::fs::create_dir_all(&dir)?;
            let file_name = Path::new(name.as_str())
                .file_name()
                .map(|n| n.to_os_string())
                .unwrap_or_else(|| "file".into());
            let path = dir.join(file_name);
            std::fs::write(&path, content.as_deref().unwrap_or_default())?;
            set_read_only(&path);
            files.push(EmbeddedFile {
                path,
                label: label.clone(),
                read_only: true,
            });
        }
        Ok(files)
    }

    fn refresh_cards(&mut self) -> Task<Message> {
        let (Some(session), Some(view)) = (self.session.clone(), self.view.clone()) else {
            return Task::none();
        };
        let generation = self.view_generation;
        self.run(
            "cards",
            true,
            move |cancel, _| session.cards(&view, cancel),
            move |result| Outcome::Cards(generation, result),
        )
    }

    // -- results --

    fn finished(&mut self, outcome: Outcome, shared: &mut Shared) -> Task<Message> {
        match outcome {
            Outcome::Opened(Ok((info, watch))) => {
                let info = info.0;
                self.repo_label = info.display_name();
                self.repo_tooltip = format!("{}\nClick to choose another repository", info.root.display());
                self.tooltip = info.root.display().to_string();
                self.title = info.display_name();
                self.info = Some(info);
                self.watch = watch;
                self.watch_signature = Some(self.signature());
                let pickers = self.load_pickers();
                Task::batch([pickers, self.reload(shared)])
            }
            Outcome::Opened(Err(err)) => {
                if !err.is_cancelled() {
                    self.show_no_repository(Some(err.to_string()));
                }
                Task::none()
            }
            Outcome::Pickers(Ok((base, target))) => {
                self.base_picker.set_entries(base);
                self.target_picker.set_entries(target);
                Task::none()
            }
            Outcome::Pickers(Err(err)) => {
                if !err.is_cancelled() {
                    self.base_picker.set_status(err.to_string());
                    self.target_picker.set_status(err.to_string());
                }
                Task::none()
            }
            Outcome::Search(side, query, result) => {
                let picker = self.picker_mut(side);
                match result {
                    Ok(entries) => picker.set_search_results(&query, entries),
                    Err(err) if !err.is_cancelled() => picker.set_status(err.to_string()),
                    Err(_) => {}
                }
                Task::none()
            }
            Outcome::Compared(keep, Ok((session, entries))) => {
                self.session = Some(session.0);
                self.update_picker_labels(true);
                self.busy = None;
                if self.banner.as_ref().is_some_and(|b| b.kind == BannerKind::Error) {
                    self.banner = None;
                }
                let task = self.set_entries(entries, keep, shared);
                if self.reload_queued {
                    return Task::batch([task, self.reload(shared)]);
                }
                task
            }
            Outcome::Compared(_, Err(err)) => {
                if err.is_cancelled() {
                    return Task::none();
                }
                self.session = None;
                self.busy = None;
                self.entries.clear();
                self.tree.clear();
                self.rows.clear();
                self.current = None;
                self.view = None;
                self.show_error(&err);
                Task::none()
            }
            Outcome::File(entry, result) => {
                self.busy = None;
                if self.current.as_ref() != Some(&entry) {
                    return Task::none();
                }
                match result {
                    Ok(view) => self.show_file(*view, shared),
                    Err(err) => {
                        if !err.is_cancelled() {
                            self.show_message("⚠", &err.to_string(), None);
                        }
                        Task::none()
                    }
                }
            }
            Outcome::Explained(key, result) => {
                self.cards.busy.remove(&key);
                match result {
                    Ok(()) => self.refresh_cards(),
                    Err(err) => {
                        if !err.is_cancelled() {
                            self.cards.errors.insert(key, err.to_string());
                        }
                        Task::none()
                    }
                }
            }
            Outcome::Cards(generation, result) => {
                match result {
                    Ok(cards) => {
                        if generation == self.view_generation
                            && let Some(view) = &mut self.view
                        {
                            view.cards = cards;
                        }
                    }
                    Err(err) if !err.is_cancelled() => self.show_error(&err),
                    Err(_) => {}
                }
                Task::none()
            }
            Outcome::ExplainedAll(result) => {
                self.busy = None;
                match result {
                    Ok(count) => {
                        let text = if count > 0 {
                            format!("Explained {count} commits and changes")
                        } else {
                            "Everything is explained already".into()
                        };
                        self.show_banner(BannerKind::Info, text, None);
                    }
                    Err(err) if !err.is_cancelled() => self.show_error(&err),
                    Err(_) => {}
                }
                self.refresh_cards()
            }
            Outcome::Built(result) => {
                self.busy = None;
                match result {
                    Ok(path) => {
                        let root = self.session.as_ref().map(|s| s.root.clone()).unwrap_or_default();
                        let shown = path.strip_prefix(&root).unwrap_or(&path).display().to_string();
                        self.show_banner(
                            BannerKind::Info,
                            format!("Wrote {shown}"),
                            Some(("Open".into(), BannerAction::Open(path))),
                        );
                        return self.refresh_cards();
                    }
                    Err(err) if !err.is_cancelled() => self.show_error(&err),
                    Err(_) => {}
                }
                Task::none()
            }
            Outcome::AgentsSetUp(result) => {
                match result {
                    Ok(changed) if changed.is_empty() => {
                        self.show_banner(BannerKind::Info, "Agent instructions are already set up".into(), None)
                    }
                    Ok(changed) => self.show_banner(BannerKind::Info, format!("Updated {}", changed.join(", ")), None),
                    Err(err) if !err.is_cancelled() => self.show_error(&err),
                    Err(_) => {}
                }
                Task::none()
            }
            Outcome::CommitMessage(sha, result) => {
                match result {
                    Ok(message) => {
                        // The dialog covers the card, which then never
                        // sees the pointer leave
                        self.hover_card(None);
                        self.commit = Some(CommitPopup {
                            sha,
                            message: text_editor::Content::with_text(message.trim_end()),
                        });
                    }
                    Err(err) if !err.is_cancelled() => self.show_error(&err),
                    Err(_) => {}
                }
                Task::none()
            }
        }
    }

    fn picker_mut(&mut self, side: PickerSide) -> &mut Picker {
        match side {
            PickerSide::Base => &mut self.base_picker,
            PickerSide::Target => &mut self.target_picker,
        }
    }

    // -- actions --

    fn swap(&mut self, shared: &Shared) -> Task<Message> {
        if self.base.kind == RevisionKind::WorkingTree
            || (self.target.uncommitted() && self.base.kind == RevisionKind::Commit)
        {
            self.show_banner(BannerKind::Warning, "Only the target can be the working tree or the index".into(), None);
            return Task::none();
        }
        if self.base.kind == RevisionKind::Index {
            self.show_banner(BannerKind::Warning, "Staged → Working Tree can’t be swapped".into(), None);
            return Task::none();
        }
        std::mem::swap(&mut self.base, &mut self.target);
        self.reload(shared)
    }

    fn explain_all(&mut self) -> Task<Message> {
        let Some(session) = self.session.clone() else {
            return Task::none();
        };
        self.busy = Some("Explaining changes with Claude Code…".into());
        self.run(
            "explain-all",
            true,
            move |cancel, progress| session.explain_all(cancel, Some(progress), false),
            Outcome::ExplainedAll,
        )
    }

    fn generate_build(&mut self) -> Task<Message> {
        let Some(session) = self.session.clone() else {
            return Task::none();
        };
        if !session.can_build() {
            self.show_banner(BannerKind::Warning, session.build_unavailable_reason(), None);
            return Task::none();
        }
        self.busy = Some("Writing the build…".into());
        self.run(
            "build",
            true,
            move |cancel, progress| session.generate_build(cancel, Some(progress)),
            Outcome::Built,
        )
    }

    fn set_up_agents(&mut self) -> Task<Message> {
        let Some(info) = self.info.clone() else {
            return Task::none();
        };
        let config = self.config.clone();
        self.run(
            "setup",
            true,
            move |cancel, _| info.set_up_agents(&config, cancel),
            Outcome::AgentsSetUp,
        )
    }

    fn open_builds_folder(&mut self) {
        let Some(info) = &self.info else {
            return;
        };
        match info.build_folder(&self.config) {
            Ok(folder) if folder.is_dir() => open_path(&folder),
            Ok(_) => self.show_banner(BannerKind::Info, "There are no builds yet".into(), None),
            Err(err) => self.show_error(&err),
        }
    }

    /// Opens the current file in a regular, editable comparison tab.
    fn open_current_file(&mut self, events: &mut Vec<Event>) {
        let Some(view) = self.view.clone() else {
            return;
        };
        let mut paths = Vec::new();
        let mut temp_files = Vec::new();
        let sides = [
            (&view.base_name, &view.base_text, None),
            (&view.target_name, &view.target_text, view.target_disk_path.as_ref()),
        ];
        for (name, content, real) in sides {
            if let Some(real) = real {
                paths.push(Some(real.clone()));
                continue;
            }
            let base = Path::new(name.as_str())
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            let written = tempfile::Builder::new()
                .prefix("meld-tmp-")
                .suffix(&format!("-{base}"))
                .tempfile()
                .and_then(|mut file| {
                    use std::io::Write;
                    file.write_all(content.as_deref().unwrap_or_default())?;
                    file.keep().map_err(|e| e.error)
                });
            match written {
                Ok((_, path)) => {
                    set_read_only(&path);
                    temp_files.push(path.clone());
                    paths.push(Some(path));
                }
                Err(err) => {
                    self.show_banner(BannerKind::Error, format!("Couldn’t write a temporary file: {err}"), None);
                    return;
                }
            }
        }
        events.push(Event::Open(Comparison::Files {
            paths,
            labels: vec![view.base_label.clone(), view.target_label.clone()],
            merge_output: None,
            auto_merge: false,
            extras: FileExtras {
                temp_files,
                ..FileExtras::default()
            },
        }));
    }

    fn hover_card(&mut self, key: Option<String>) {
        let card = key
            .as_ref()
            .and_then(|k| self.view.as_ref()?.cards.iter().find(|c| c.key == *k));
        let lines = match card {
            Some(card) => [zero_based(&card.deleted), zero_based(&card.added)],
            None => [Vec::new(), Vec::new()],
        };
        self.diff.set_hover(lines);
        self.cards.hovered = key;
    }

    // -- status --

    fn show_message(&mut self, icon: &'static str, title: &str, description: Option<String>) {
        self.content = Content::Message {
            icon,
            title: title.to_string(),
            description,
            choose: false,
        };
    }

    fn show_no_repository(&mut self, error: Option<String>) {
        self.repo_label = "No repository".into();
        self.repo_tooltip = "Choose a repository or worktree".into();
        self.info = None;
        self.session = None;
        self.busy = None;
        self.content = Content::Message {
            icon: "📁",
            title: "Choose a Git repository or worktree".into(),
            description: Some(
                "Then pick any branch, tag or commit, the staged or unstaged changes, or another worktree to compare."
                    .into(),
            ),
            choose: true,
        };
        if let Some(error) = error {
            self.show_banner(BannerKind::Error, error, None);
        }
    }

    fn show_error(&mut self, err: &Error) {
        let message = err.to_string();
        if self.session.is_none() {
            self.show_message("⚠", &message, None);
        }
        self.show_banner(BannerKind::Error, message, None);
    }

    fn show_banner(&mut self, kind: BannerKind, text: String, action: Option<(String, BannerAction)>) {
        self.banner = Some(Banner { kind, text, action });
    }

    /// Modification times of the watched files, to notice repository
    /// changes (Python watches them with file monitors).
    fn signature(&self) -> Vec<Option<(SystemTime, u64)>> {
        self.watch.iter().map(|p| file_signature(p)).collect()
    }

    fn poll(&mut self, shared: &Shared) -> Task<Message> {
        let follows = self
            .session
            .as_ref()
            .is_some_and(|s| s.follows_repository_changes());
        let signature = self.signature();
        let mut changed = self.watch_signature.as_ref() != Some(&signature);
        changed |= std::mem::take(&mut self.changed_elsewhere);
        self.watch_signature = Some(signature);
        if let Some((path, previous)) = &self.disk_signature {
            let now = file_signature(path);
            if now != *previous {
                changed = true;
                self.disk_signature = Some((path.clone(), now));
            }
        }
        if changed && follows {
            if self.jobs.is_running("compare") {
                // What the running comparison read may be out of date
                self.reload_queued = true;
            } else {
                return self.reload(shared);
            }
        }
        Task::none()
    }

    /// Another tab saved `path` (`on_file_changed()`): if it is in the
    /// repository, the next check compares again (when the comparison
    /// follows the repository's changes).
    pub fn file_changed(&mut self, path: &Path) -> Task<Message> {
        if self.info.as_ref().is_some_and(|info| path.starts_with(&info.root)) {
            self.changed_elsewhere = true;
        }
        Task::none()
    }

    // -- the tab interface --

    pub fn update(&mut self, message: Message, shared: &mut Shared, events: &mut Vec<Event>) -> Task<Message> {
        self.row_spacing_cache = filelist::row_spacing(shared.settings.ailens_file_row_spacing);
        match message {
            Message::Job(ticket, outcome) => {
                if !self.jobs.finish(&ticket) {
                    return Task::none();
                }
                self.finished(outcome, shared)
            }
            Message::Progress(ticket, text) => {
                if self.jobs.is_current(&ticket) && self.busy.is_some() {
                    self.busy = Some(text);
                }
                Task::none()
            }
            Message::Diff(message) => {
                let task = self.diff.update(message, shared, events).map(Message::Diff);
                if self.scroll_pending && !self.diff.is_loading() {
                    self.scroll_pending = false;
                    if let Some(line) = self.diff.first_change() {
                        return Task::batch([task, self.diff.scroll_to_line(1, line).map(Message::Diff)]);
                    }
                }
                task
            }
            Message::ChooseRepository => {
                let mut dialog = rfd::AsyncFileDialog::new().set_title("Choose a Git Repository or Worktree");
                if let Some(info) = &self.info {
                    dialog = dialog.set_directory(&info.root);
                } else if self.location.is_dir() {
                    dialog = dialog.set_directory(&self.location);
                }
                Task::perform(dialog.pick_folder(), |h| {
                    Message::RepositoryChosen(h.map(|h| h.path().to_path_buf()))
                })
            }
            Message::RepositoryChosen(Some(path)) => {
                self.jobs.cancel_all();
                self.info = None;
                self.session = None;
                self.banner = None;
                self.base = Revision::commit("HEAD");
                self.target = Revision::working_tree(None);
                self.entries.clear();
                self.tree.clear();
                self.rows.clear();
                self.current = None;
                self.view = None;
                self.update_picker_labels(false);
                self.set_location(path)
            }
            Message::RepositoryChosen(None) => Task::none(),
            Message::TogglePopup(kind) => {
                let popup = match kind {
                    PopupKind::Base => Popup::Picker(PickerSide::Base),
                    PopupKind::Target => Popup::Picker(PickerSide::Target),
                    PopupKind::More => Popup::More,
                };
                if self.popup == Some(popup) {
                    self.popup = None;
                    return Task::none();
                }
                self.popup = Some(popup);
                match popup {
                    Popup::Picker(side) => {
                        let picker = self.picker_mut(side);
                        picker.reset();
                        iced::widget::operation::focus(picker.input_id.clone())
                    }
                    Popup::More => Task::none(),
                }
            }
            Message::ClosePopup => {
                self.popup = None;
                Task::none()
            }
            Message::Picker(side, message) => {
                let moved = matches!(message, picker::Message::Move(_));
                match self.picker_mut(side).update(message) {
                    picker::Outcome::None if moved => self.picker_mut(side).scroll_to_selected(),
                    picker::Outcome::None => Task::none(),
                    picker::Outcome::ScheduleSearch(generation) => Task::perform(
                        tokio::time::sleep(picker::SEARCH_DELAY),
                        move |_| Message::SearchDue(side, generation),
                    ),
                    picker::Outcome::Picked(entry) => {
                        self.popup = None;
                        let heads = self.info.as_ref().map(|i| i.worktree_heads()).unwrap_or_default();
                        let (base, target) = apply_pick(&self.base, &self.target, &entry, side, &heads);
                        self.base = base;
                        self.target = target;
                        self.reload(shared)
                    }
                }
            }
            Message::SearchDue(side, generation) => {
                let Some(info) = self.info.clone() else {
                    return Task::none();
                };
                let picker = self.picker_mut(side);
                if picker.search_generation() != generation {
                    return Task::none();
                }
                let query = picker.query().trim().to_string();
                if query.is_empty() {
                    return Task::none();
                }
                let for_result = query.clone();
                self.run(
                    &format!("search-{}", side_name(side)),
                    true,
                    move |cancel, _| info.search_commits(&query, cancel),
                    move |result| Outcome::Search(side, for_result.clone(), result),
                )
            }
            Message::Swap => self.swap(shared),
            Message::Reload => self.reload(shared),
            Message::ExplainAll => self.explain_all(),
            Message::Build => self.generate_build(),
            Message::SetUpAgents => {
                self.popup = None;
                self.set_up_agents()
            }
            Message::OpenBuilds => {
                self.popup = None;
                self.open_builds_folder();
                Task::none()
            }
            Message::OpenFile => {
                self.popup = None;
                self.open_current_file(events);
                Task::none()
            }
            Message::BannerAction => {
                if let Some(Banner {
                    action: Some((_, action)),
                    ..
                }) = self.banner.take()
                {
                    match action {
                        BannerAction::Open(path) => open_path(&path),
                    }
                }
                Task::none()
            }
            Message::DismissBanner => {
                self.banner = None;
                Task::none()
            }
            Message::SelectFile(entry) => self.select_entry(entry, shared),
            Message::ToggleFolder(key) => {
                if !self.collapsed.remove(&key) {
                    self.collapsed.insert(key);
                }
                self.rebuild_rows();
                Task::none()
            }
            Message::FilesScrolled(viewport) => {
                self.files_viewport = (viewport.absolute_offset().y, viewport.bounds().height);
                Task::none()
            }
            Message::ExplainCard(key) => {
                let (Some(session), Some(view)) = (self.session.clone(), self.view.clone()) else {
                    return Task::none();
                };
                self.cards.busy.insert(key.clone());
                self.cards.errors.remove(&key);
                let for_result = key.clone();
                self.run(
                    &format!("explain-{key}"),
                    false,
                    move |cancel, _| session.explain_card(&view, &key, cancel),
                    move |result| Outcome::Explained(for_result.clone(), result),
                )
            }
            Message::ShowCommit(sha) => {
                let Some(session) = self.session.clone() else {
                    return Task::none();
                };
                let for_result = sha.clone();
                self.run(
                    "message",
                    true,
                    move |cancel, _| session.commit_message(&sha, cancel),
                    move |result| Outcome::CommitMessage(for_result.clone(), result),
                )
            }
            Message::CommitAction(action) => {
                if let Some(commit) = &mut self.commit
                    && !action.is_edit()
                {
                    commit.message.perform(action);
                }
                Task::none()
            }
            Message::CloseCommit => {
                self.commit = None;
                Task::none()
            }
            Message::CompareWithParent(sha) => {
                self.commit = None;
                self.base = Revision::commit(format!("{sha}^"));
                self.target = Revision::commit(sha);
                self.reload(shared)
            }
            Message::OpenBuild(name) => {
                if let Some(info) = &self.info {
                    match info.build_folder(&self.config) {
                        Ok(folder) => open_path(&folder.join(name)),
                        Err(err) => self.show_error(&err),
                    }
                }
                Task::none()
            }
            Message::HoverCard(key) => {
                self.hover_card(key);
                Task::none()
            }
            Message::ExpandCard(key) => {
                if !self.cards.expanded.remove(&key) {
                    self.cards.expanded.insert(key);
                }
                Task::none()
            }
            Message::StartDrag(target, origin_x) => {
                let start_width = match target {
                    DragTarget::FileList => shared.settings.ailens_file_list_width as f32,
                    DragTarget::WhyColumn => self.why_width,
                };
                self.drag = Some(Drag {
                    target,
                    start_width,
                    origin_x,
                    moved: false,
                });
                Task::none()
            }
            Message::DragMove(x) => {
                if let Some(drag) = &mut self.drag {
                    drag.moved = true;
                    let width = dragged_width(drag.target, drag.start_width, drag.origin_x, x);
                    match drag.target {
                        DragTarget::FileList => shared.settings.ailens_file_list_width = width as i32,
                        DragTarget::WhyColumn => self.why_width = width,
                    }
                }
                Task::none()
            }
            Message::DragEnd => {
                if let Some(drag) = self.drag.take()
                    && drag.target == DragTarget::FileList
                    && drag.moved
                {
                    events.push(Event::SettingsChanged);
                }
                Task::none()
            }
            Message::ResetWhyWidth => {
                self.why_width = whycolumn::DEFAULT_WIDTH;
                Task::none()
            }
            Message::Poll => self.poll(shared),
        }
    }

    pub fn command(&mut self, command: Command, shared: &mut Shared, events: &mut Vec<Event>) -> Task<Message> {
        match command {
            Command::NextFile | Command::PreviousFile => {
                let step = if command == Command::NextFile { 1 } else { -1 };
                match filelist::relative_entry(&self.tree, self.current.as_ref(), step) {
                    Some(entry) => self.select_entry(entry, shared),
                    None => Task::none(),
                }
            }
            Command::Refresh => self.reload(shared),
            Command::Stop => {
                if self.popup.is_some() || self.commit.is_some() {
                    self.popup = None;
                    self.commit = None;
                } else {
                    self.jobs.cancel_all();
                    self.busy = None;
                    self.cards.busy.clear();
                }
                Task::none()
            }
            _ if self.content == Content::Diff => self.diff.command(command, shared, events).map(Message::Diff),
            _ => Task::none(),
        }
    }

    pub fn request_close(&mut self, shared: &mut Shared, events: &mut Vec<Event>) -> Task<Message> {
        self.diff.request_close(shared, events).map(Message::Diff)
    }

    pub fn settings_changed(&mut self, shared: &Shared) -> Task<Message> {
        self.config = config_from_settings(&shared.settings);
        self.row_spacing_cache = filelist::row_spacing(shared.settings.ailens_file_row_spacing);
        self.diff.settings_changed(shared).map(Message::Diff)
    }

    pub fn subscription(&self) -> Subscription<Message> {
        let mut subs = vec![self.diff.subscription().map(Message::Diff)];
        if self.info.is_some() {
            subs.push(iced::time::every(POLL_INTERVAL).map(|_| Message::Poll));
        }
        if self.drag.is_some() {
            subs.push(iced::event::listen_with(|event, _status, _window| match event {
                iced::Event::Mouse(iced::mouse::Event::CursorMoved { position }) => {
                    Some(Message::DragMove(position.x))
                }
                iced::Event::Mouse(iced::mouse::Event::ButtonReleased(iced::mouse::Button::Left)) => {
                    Some(Message::DragEnd)
                }
                _ => None,
            }));
        }
        Subscription::batch(subs)
    }

    pub fn toolbar<'a>(&'a self, shared: &'a Shared) -> Element<'a, Message> {
        if self.content == Content::Diff {
            self.diff.toolbar(shared).map(Message::Diff)
        } else {
            space::horizontal().into()
        }
    }

    pub fn menu_items(&self, shared: &Shared) -> Vec<menu::Item<Message>> {
        let mut items = vec![menu::Item::Heading("Git Compare".into())];
        let file = |label: &str, shortcut, step: isize| match filelist::relative_entry(&self.tree, self.current.as_ref(), step) {
            Some(entry) if Some(&entry) != self.current.as_ref() => {
                menu::Item::action(label, Some(shortcut), Message::SelectFile(entry))
            }
            _ => menu::Item::disabled(label),
        };
        items.push(file("Next Changed File", "Alt+F7", 1));
        items.push(file("Previous Changed File", "Alt+Shift+F7", -1));
        items.push(menu::Item::action("Reload", Some("Ctrl+R"), Message::Reload));
        items.push(menu::Item::Separator);
        let if_info = |label: &str, message: Message| {
            if self.info.is_some() {
                menu::Item::action(label, None, message)
            } else {
                menu::Item::disabled(label)
            }
        };
        items.push(if_info("Set Up Agent Instructions", Message::SetUpAgents));
        items.push(if_info("Open Builds Folder", Message::OpenBuilds));
        items.push(if self.view.is_some() {
            menu::Item::action("Open File in New Tab", None, Message::OpenFile)
        } else {
            menu::Item::disabled("Open File in New Tab")
        });
        if self.content == Content::Diff {
            let diff_items = self.diff.menu_items(shared);
            if !diff_items.is_empty() {
                items.push(menu::Item::Separator);
                items.extend(diff_items.into_iter().map(|item| map_item(item, Message::Diff)));
            }
        }
        items
    }

    // -- view --

    pub fn view<'a>(&'a self, shared: &'a Shared) -> Element<'a, Message> {
        let mut page = column![self.header(), ui::separator(false)];
        if let Some(banner) = &self.banner {
            let mut actions = Vec::new();
            if let Some((label, _)) = &banner.action {
                actions.push((label.clone(), Message::BannerAction));
            }
            actions.push(("Dismiss".to_string(), Message::DismissBanner));
            let (primary, secondary) = condense(&banner.text);
            page = page.push(ui::banner(banner.kind, primary, secondary, actions));
        }
        let files_width = shared.settings.ailens_file_list_width as f32;
        let body = row![
            container(self.file_panel(shared)).width(files_width).height(Length::Fill),
            divider(|x| Message::StartDrag(DragTarget::FileList, x), None),
            self.content_view(shared),
        ]
        .height(Length::Fill);
        let page: Element<'a, Message> = page.push(body).into();

        match &self.commit {
            Some(commit) => ui::modal::modal(page, commit_dialog(commit), Message::CloseCommit),
            None => page,
        }
    }

    fn header(&self) -> Element<'_, Message> {
        let repo = tooltip(
            // The end of a path tells most (`set_ellipsize(START)`)
            button(ellipsized(self.repo_label.as_str(), Mode::Start).size(14).max_width(200.0))
            .padding([4, 8])
            .style(ui::flat_button)
            .on_press(Message::ChooseRepository),
            container(text(self.repo_tooltip.as_str()).size(12))
                .padding([4, 8])
                .style(ui::tooltip_box),
            tooltip::Position::Bottom,
        );
        let base = self.base_picker.view(
            self.popup == Some(Popup::Picker(PickerSide::Base)),
            Message::TogglePopup(PopupKind::Base),
            Message::ClosePopup,
            |m| Message::Picker(PickerSide::Base, m),
        );
        let target = self.target_picker.view(
            self.popup == Some(Popup::Picker(PickerSide::Target)),
            Message::TogglePopup(PopupKind::Target),
            Message::ClosePopup,
            |m| Message::Picker(PickerSide::Target, m),
        );
        let has_session = self.session.is_some();
        let idle = self.busy.is_none();
        let status: Element<'_, Message> = match &self.busy {
            Some(busy) => row![
                text("⏳").size(13),
                ellipsized(busy.as_str(), Mode::End)
                    .size(13)
                    .style(ui::dim_text)
                    .width(Length::Fill)
            ]
            .spacing(6)
            .align_y(iced::Center)
            .into(),
            None if self.relation.is_empty() => space::horizontal().into(),
            None => tooltip(
                ellipsized(self.relation.as_str(), Mode::End)
                    .size(13)
                    .style(ui::dim_text)
                    .width(Length::Fill),
                container(text(self.relation.as_str()).size(12))
                    .padding([4, 8])
                    .style(ui::tooltip_box),
                tooltip::Position::Bottom,
            )
            .delay(Duration::from_millis(600))
            .into(),
        };
        let mut bar = row![
            repo,
            base,
            ui::tool_button(icon::SWAP, "Swap base and target", self.info.is_some().then_some(Message::Swap)),
            target,
            container(status).width(Length::Fill).clip(true).padding([0, 6]),
            ui::tool_button(icon::REFRESH, "Reload the comparison", self.info.is_some().then_some(Message::Reload)),
        ]
        .spacing(6)
        .align_y(iced::Center);
        if self.config.infer_enabled {
            bar = bar.push(tooltip(
                button(text("Explain").size(14))
                    .padding([5, 12])
                    .style(button::secondary)
                    .on_press_maybe((has_session && idle).then_some(Message::ExplainAll)),
                container(text("Explain every change that isn’t explained yet, with Claude Code").size(12))
                    .padding([4, 8])
                    .style(ui::tooltip_box),
                tooltip::Position::Bottom,
            ));
        }
        let (build_tip, can_build) = match &self.session {
            Some(session) => {
                let reason = session.build_unavailable_reason();
                if reason.is_empty() {
                    ("Write a build file for this comparison".to_string(), true)
                } else {
                    (reason, false)
                }
            }
            None => ("Pick a base and a target first".to_string(), false),
        };
        bar = bar.push(tooltip(
            button(
                text(if self.config.infer_enabled { "Explain & Build" } else { "Generate Build" }).size(14),
            )
            .padding([5, 12])
            .style(button::primary)
            .on_press_maybe((can_build && idle).then_some(Message::Build)),
            container(text(build_tip).size(12)).padding([4, 8]).style(ui::tooltip_box),
            tooltip::Position::Bottom,
        ));
        let more_open = self.popup == Some(Popup::More);
        let more_button: Element<'_, Message> = if more_open {
            // Without its tooltip, which would cover the menu
            button(text("⋯").size(16).center())
                .padding([4, 8])
                .style(ui::flat_button)
                .on_press(Message::TogglePopup(PopupKind::More))
                .into()
        } else {
            ui::tool_button("⋯", "More AI Lens actions", Some(Message::TogglePopup(PopupKind::More)))
        };
        bar = bar.push(
            popover(more_button, more_open.then(|| self.more_menu()), Message::ClosePopup).align_right(true),
        );
        container(bar).padding([6, 6]).width(Length::Fill).into()
    }

    fn more_menu(&self) -> Element<'_, Message> {
        let item = |label: &'static str, message: Option<Message>| {
            button(text(label).size(14))
                .width(Length::Fill)
                .padding([6, 10])
                .style(ui::flat_button)
                .on_press_maybe(message)
        };
        let has_info = self.info.is_some();
        container(
            column![
                item("Set Up Agent Instructions", has_info.then_some(Message::SetUpAgents)),
                item("Open Builds Folder", has_info.then_some(Message::OpenBuilds)),
                item("Open File in New Tab", self.view.is_some().then_some(Message::OpenFile)),
            ]
            .width(260),
        )
        .padding(6)
        .style(ui::popover)
        .into()
    }

    fn file_panel<'a>(&'a self, shared: &'a Shared) -> Element<'a, Message> {
        let title = container(
            ellipsized(self.files_title.as_str(), Mode::End)
                .size(13)
                .font(ui::bold())
                .width(Length::Fill),
        )
        .padding([4, 8]);
        let spacing = filelist::row_spacing(shared.settings.ailens_file_row_spacing);
        let list = filelist::view(
            &self.rows,
            self.current.as_ref(),
            spacing,
            self.files_scroll_id.clone(),
            Message::FilesScrolled,
            filelist::Callbacks {
                select: Message::SelectFile,
                toggle: Message::ToggleFolder,
            },
        );
        column![title, list].into()
    }

    fn content_view<'a>(&'a self, shared: &'a Shared) -> Element<'a, Message> {
        match &self.content {
            Content::Message {
                icon,
                title,
                description,
                choose,
            } => {
                let mut page = column![
                    text(*icon).size(48),
                    text(title.as_str()).size(20).font(ui::bold()).center(),
                ]
                .spacing(12)
                .max_width(560)
                .align_x(iced::Center);
                if let Some(description) = description {
                    page = page.push(text(description.as_str()).size(14).style(ui::dim_text).center());
                }
                if *choose {
                    page = page.push(
                        button(text("Choose Folder…").size(14))
                            .padding([8, 20])
                            .style(button::primary)
                            .on_press(Message::ChooseRepository),
                    );
                }
                container(page).center(Length::Fill).padding(20).into()
            }
            Content::Diff => row![
                container(self.diff.view(shared).map(Message::Diff)).width(Length::Fill).height(Length::Fill),
                divider(|x| Message::StartDrag(DragTarget::WhyColumn, x), Some(Message::ResetWhyWidth)),
                self.why_panel(),
            ]
            .height(Length::Fill)
            .into(),
        }
    }

    fn why_panel(&self) -> Element<'_, Message> {
        let title = container(text("Why").size(13).font(ui::bold()))
            .height(self.diff.header_height())
            .padding([0, 8])
            .align_y(iced::Center);
        let callbacks = whycolumn::Callbacks {
            explain: Message::ExplainCard,
            commit: Message::ShowCommit,
            build: Message::OpenBuild,
            hover: Message::HoverCard,
            expand: Message::ExpandCard,
        };
        let cards: &[Card] = match (&self.view, self.diff.is_loading()) {
            (Some(view), false) => &view.cards,
            _ => &[],
        };
        let now = meld_ailens::time::now_timestamp();
        let elements: Vec<Element<'_, Message>> = cards
            .iter()
            .map(|card| whycolumn::card_view(card, &self.cards, callbacks, now))
            .collect();
        let wanted = whycolumn::wanted_tops(cards, [self.diff.pane_geometry(0), self.diff.pane_geometry(1)]);
        let placeholder = self
            .view
            .as_ref()
            .filter(|v| v.cards.is_empty() && !v.cards_placeholder.is_empty())
            .map(|v| {
                container(text(v.cards_placeholder.as_str()).size(13).style(ui::dim_text).center())
                    .padding([0, 12])
                    .center_x(Length::Fill)
                    .into()
            });
        container(column![title, whycolumn::card_column(elements, wanted, placeholder)])
            .width(self.why_width)
            .height(Length::Fill)
            .style(|theme: &Theme| container::Style {
                background: Some(Background::Color(theme.extended_palette().background.weak.color)),
                ..container::Style::default()
            })
            .into()
    }
}

fn commit_dialog(commit: &CommitPopup) -> Element<'_, Message> {
    let short = meld_ailens::picker::short_sha(&commit.sha);
    // An editor that ignores edits: the message can be selected and copied
    let message = text_editor(&commit.message)
        .on_action(Message::CommitAction)
        .font(iced::Font::MONOSPACE)
        .size(13)
        .padding(8)
        .max_height(420)
        .wrapping(text::Wrapping::WordOrGlyph);
    let dialog = ui::modal::dialog(
        format!("Commit {short}"),
        container(message).width(600),
        vec![
            button(text("Compare with Parent"))
                .style(button::secondary)
                .on_press(Message::CompareWithParent(commit.sha.clone()))
                .into(),
            button(text("Close")).on_press(Message::CloseCommit).into(),
        ],
    );
    // As wide as the message and the dialog's padding, not the window
    container(dialog).max_width(640).into()
}

fn map_item<M, N>(item: menu::Item<M>, f: impl Fn(M) -> N) -> menu::Item<N> {
    match item {
        menu::Item::Action {
            label,
            shortcut,
            message,
        } => menu::Item::Action {
            label,
            shortcut,
            message: message.map(&f),
        },
        menu::Item::Toggle {
            label,
            checked,
            message,
        } => menu::Item::Toggle {
            label,
            checked,
            message: f(message),
        },
        menu::Item::Separator => menu::Item::Separator,
        menu::Item::Heading(h) => menu::Item::Heading(h),
    }
}

/// A message's first line, and the rest on one line and cut short:
/// errors from git can run to many lines, banners have room for two.
pub fn condense(message: &str) -> (String, Option<String>) {
    let mut lines = message.lines().map(str::trim).filter(|l| !l.is_empty());
    let first = lines.next().unwrap_or_default().to_string();
    let rest: Vec<&str> = lines.collect();
    if rest.is_empty() {
        return (first, None);
    }
    (first, Some(whycolumn::shorten(&rest.join(" "), 240)))
}

fn set_read_only(path: &Path) {
    if let Ok(meta) = std::fs::metadata(path) {
        let mut permissions = meta.permissions();
        permissions.set_readonly(true);
        let _ = std::fs::set_permissions(path, permissions);
    }
}

fn open_path(path: &Path) {
    if let Err(err) = open::that_detached(path) {
        log::warn!("Can't open {}: {err}", path.display());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_follows_the_settings_with_defaults_for_empty_texts() {
        let mut settings = Settings::default();
        settings.ailens_claude_path = "/opt/claude".into();
        settings.ailens_infer_enabled = false;
        settings.ailens_infer_model = " ".into();
        settings.ailens_notes_ref = "lens".into();
        settings.ailens_author_pattern = "bot@".into();
        let config = config_from_settings(&settings);
        assert_eq!(config.claude_path, "/opt/claude");
        assert!(!config.infer_enabled);
        assert_eq!(config.infer_model, SessionConfig::default().infer_model);
        assert_eq!(config.notes_ref, "lens");
        assert_eq!(config.build_folder, "agent");
        assert_eq!(config.author_pattern.as_deref(), Some("bot@"));
        settings.ailens_author_pattern.clear();
        assert_eq!(config_from_settings(&settings).author_pattern, None);
    }

    #[test]
    fn long_errors_are_condensed_for_banners() {
        assert_eq!(condense("One line"), ("One line".to_string(), None));
        let (first, rest) = condense("git notes failed\n\n*** Please tell me who you are.\n\nRun\n  git config");
        assert_eq!(first, "git notes failed");
        assert_eq!(rest.as_deref(), Some("*** Please tell me who you are. Run git config"));
    }

    #[test]
    fn dividers_follow_the_pointer_from_where_it_pressed() {
        // No move yet: no jump
        assert_eq!(dragged_width(DragTarget::WhyColumn, 300.0, 1203.0, 1203.0), 300.0);
        assert_eq!(dragged_width(DragTarget::FileList, 300.0, 301.5, 301.5), 300.0);
        // The Why column is right of its divider: dragging left widens it
        assert_eq!(dragged_width(DragTarget::WhyColumn, 300.0, 1203.0, 1190.0), 313.0);
        assert_eq!(dragged_width(DragTarget::FileList, 300.0, 301.0, 341.0), 340.0);
        // Within their ranges
        assert_eq!(dragged_width(DragTarget::WhyColumn, 300.0, 1000.0, 1500.0), whycolumn::MIN_WIDTH);
        assert_eq!(dragged_width(DragTarget::FileList, 300.0, 300.0, 0.0), 120.0);
    }

    #[test]
    fn card_ranges_become_zero_based() {
        use meld_ailens::LineRange;
        assert_eq!(zero_based(&[LineRange::new(1, 3), LineRange::new(10, 10)]), [0..3, 9..10]);
    }
}
