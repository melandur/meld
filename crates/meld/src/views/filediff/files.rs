//! Loading, comparing, saving and closing (`set_files`, `file_loaded`,
//! `_diff_files`, `save_file`, `request_close`, ...).

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use iced::Task;
use meld_diff::TextBuffer;
use meld_files::{LoadOptions, SaveOptions};
use meld_widgets::Position as WPos;

use super::io::{self, DiffDone, DiffJob, DiffKind, LoadFailure, Loaded, SaveFailure, Saved};
use super::{
    Banner, BannerAction, CloseState, Dialog, FileDiff, Job, LoadState, Message, MsgId, Pending,
};
use crate::shared::Shared;
use crate::ui::BannerKind;
use crate::views::Event;

impl FileDiff {
    /// `pre_comparison_init`: forget the comparison and transient
    /// messages before loading.
    fn pre_comparison_init(&mut self) {
        if let Some(job) = self.job.take() {
            job.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        self.diff_gen += 1;
        for pane in &mut self.panes {
            pane.inline.clear();
            if pane.banner.as_ref().is_some_and(|b| b.id.transient()) {
                pane.banner = None;
            }
        }
    }

    /// Clears the undo history; panes keep their modified flags.
    fn clear_undo(&mut self) {
        self.process_undo_events();
        self.undo.clear();
        self.undo.take_events();
    }

    /// `set_files`: load files into the panes (`None` keeps a pane's
    /// text) and compare them.
    pub(super) fn load_files(
        &mut self,
        paths: Vec<Option<PathBuf>>,
        encodings: Vec<Option<&'static encoding_rs::Encoding>>,
    ) -> Task<Message> {
        self.pre_comparison_init();
        self.clear_undo();
        self.load_gen += 1;
        let mut tasks = Vec::new();
        for (pane, path) in paths.into_iter().enumerate().take(self.panes.len()) {
            match path {
                Some(path) => {
                    // A reloaded file starts unmodified.
                    self.panes[pane].extra_modified = false;
                    let encoding = encodings.get(pane).copied().flatten();
                    tasks.push(self.load_pane(pane, path, encoding));
                }
                None => {
                    let p = &mut self.panes[pane];
                    p.file.state = LoadState::Loaded;
                    p.pending = Some(TextBuffer::from_text(&p.buffer.lines().join("\n")));
                }
            }
        }
        if tasks.is_empty() {
            return self.start_diff(DiffKind::Initial);
        }
        self.refresh_read_only();
        Task::batch(tasks)
    }

    /// `set_file`: reload a single pane.
    pub(super) fn set_file(
        &mut self,
        pane: usize,
        path: PathBuf,
        encoding: Option<&'static encoding_rs::Encoding>,
    ) -> Task<Message> {
        self.pre_comparison_init();
        self.clear_undo();
        self.load_gen += 1;
        self.panes[pane].extra_modified = false;
        let task = self.load_pane(pane, path, encoding);
        self.refresh_read_only();
        task
    }

    fn load_pane(
        &mut self,
        pane: usize,
        path: PathBuf,
        encoding: Option<&'static encoding_rs::Encoding>,
    ) -> Task<Message> {
        let generation = self.load_gen;
        let p = &mut self.panes[pane];
        p.banner = None;
        if p.file.path.as_ref() != Some(&path) {
            p.file.savefile = if pane == 1 {
                self.merge_output.clone()
            } else {
                None
            };
            if p.file.path.is_some() {
                p.file.custom_label = None;
            }
            p.language_chosen = false;
        }
        p.file.path = Some(path.clone());
        p.file.state = LoadState::Loading;
        let options = LoadOptions {
            detect_encodings: self.detect_encodings.clone(),
            encoding,
            check_line_length: !self.force_load,
        };
        let target = p.file.savefile.clone();
        Task::perform(
            io::blocking(move || io::load(path, target, options)),
            move |r| Message::Loaded(generation, pane, r.map(Box::new)),
        )
    }

    pub(super) fn file_loaded(
        &mut self,
        generation: u64,
        pane: usize,
        result: Result<Box<Loaded>, LoadFailure>,
        _shared: &mut Shared,
    ) -> Task<Message> {
        if generation != self.load_gen || pane >= self.panes.len() {
            return Task::none();
        }
        let path = self.panes[pane].file.path.clone().unwrap_or_default();
        let display = path.display().to_string();
        match result {
            Ok(loaded) => {
                let Loaded {
                    text,
                    mtime,
                    writable,
                } = *loaded;
                let p = &mut self.panes[pane];
                p.file.encoding = text.encoding;
                p.file.newlines = text.newlines.clone();
                p.file.newline = text.newlines.primary();
                p.file.mtime = mtime;
                p.file.disk_mtime = mtime;
                p.file.writable = writable && !p.file.force_read_only;
                p.file.state = LoadState::Loaded;
                // GtkSourceView's implicit trailing newline.
                let body = meld_files::text::strip_trailing_newline(&text.text);
                let tb = TextBuffer::from_text(body);
                p.buffer.set_lines(tb.lines().to_vec());
                p.highlighter.reset();
                p.scroll = iced::Vector::ZERO;
                p.pending = Some(tb);
                // `update_buffer_writable`
                p.editable = p.file.writable;
                let has_savefile = p.file.savefile.is_some();
                self.undo.checkpoint(pane);
                self.process_undo_events();
                // Saving elsewhere than loaded: needs saving to persist.
                self.panes[pane].extra_modified = has_savefile;
                let p = &mut self.panes[pane];
                if text.binary {
                    p.banner = Some(Banner {
                        id: MsgId::Binary,
                        kind: BannerKind::Warning,
                        primary: tr!("File {} appears to be a binary file.", display),
                        secondary: Some(
                            tr!("Do you want to open the file using the default application?")
                                .into(),
                        ),
                        actions: vec![
                            (tr!("Open").into(), BannerAction::OpenExternally),
                            (tr!("Hide").into(), BannerAction::Hide),
                        ],
                    });
                } else if text.lossy {
                    p.banner = Some(Banner {
                        id: MsgId::Decode,
                        kind: BannerKind::Warning,
                        primary: tr!("There was a problem opening the file “{}”.", display),
                        secondary: Some(tr!(
                            "The file could not be decoded with the encoding “{}”; invalid characters were replaced. Saving would change the file.",
                            text.encoding.name()
                        )),
                        actions: vec![
                            (tr!("Choose Encoding…").into(), BannerAction::ChooseEncoding),
                            (tr!("Hide").into(), BannerAction::Hide),
                        ],
                    });
                }
            }
            Err(LoadFailure::LineTooLong(message)) => {
                let p = &mut self.panes[pane];
                p.file.state = LoadState::Error;
                p.banner = Some(Banner {
                    id: MsgId::LineTooLong,
                    kind: BannerKind::Warning,
                    primary: tr!("There was a problem opening the file “{}”.", display),
                    secondary: Some(message),
                    actions: vec![
                        (tr!("Hide").into(), BannerAction::Hide),
                        (tr!("Load anyway").into(), BannerAction::LoadAnyway),
                    ],
                });
            }
            Err(LoadFailure::Other(message)) => {
                let p = &mut self.panes[pane];
                p.file.state = LoadState::Error;
                p.banner = Some(Banner {
                    id: MsgId::Error,
                    kind: BannerKind::Error,
                    primary: tr!("There was a problem opening the file “{}”.", display),
                    secondary: Some(message),
                    actions: vec![(tr!("Hide").into(), BannerAction::Hide)],
                });
            }
        }
        if self.panes.iter().all(|p| p.file.state == LoadState::Loaded) {
            return self.start_diff(DiffKind::Initial);
        }
        Task::none()
    }

    /// Starts a comparison job over the panes' current text.
    pub(super) fn start_diff(&mut self, kind: DiffKind) -> Task<Message> {
        if let Some(job) = self.job.take() {
            job.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        let buffers: Vec<TextBuffer> = (0..self.panes.len())
            .map(|i| {
                let from_cmp = || self.cmp.as_ref().and_then(|c| c.buffer(i)).cloned();
                match kind {
                    DiffKind::Initial => self.panes[i].pending.take(),
                    DiffKind::Refresh => None,
                }
                .or_else(from_cmp)
                .unwrap_or_else(|| TextBuffer::from_text(&self.panes[i].buffer.lines().join("\n")))
            })
            .collect();
        for pane in &mut self.panes {
            pane.pending = None;
        }
        if kind == DiffKind::Initial {
            self.cmp = None;
        }
        let unresolved = self
            .cmp
            .as_ref()
            .map(|c| c.differ().unresolved().to_vec())
            .unwrap_or_default();
        let job = DiffJob {
            kind,
            buffers,
            filters: self.active_filters(),
            ignore_blanks: self.ignore_blank_lines,
            auto_merge: self.auto_merge,
            syncpoints: self.syncpoints.valid_points(),
            unresolved,
            paths: self.panes.iter().map(|p| p.file.path.clone()).collect(),
        };
        let cancel = Arc::new(AtomicBool::new(false));
        self.job = Some(Job {
            cancel: cancel.clone(),
        });
        self.diff_gen += 1;
        let generation = self.diff_gen;
        self.refresh_read_only();
        Task::perform(io::blocking(move || io::run_diff(job, cancel)), move |r| {
            Message::Diffed(generation, r.map(Box::new))
        })
    }

    /// `refresh_comparison`
    pub(super) fn refresh_comparison(&mut self) -> Task<Message> {
        if self.cmp.is_none() {
            return Task::none();
        }
        for pane in &mut self.panes {
            if pane.banner.as_ref().is_some_and(|b| b.id.transient()) {
                pane.banner = None;
            }
        }
        self.start_diff(DiffKind::Refresh)
    }

    pub(super) fn show_stopped(&mut self) {
        self.refresh_read_only();
        let pane = if self.panes.len() > 1 { 1 } else { 0 };
        self.panes[pane].banner = Some(Banner {
            id: MsgId::Stopped,
            kind: BannerKind::Info,
            primary: tr!("The comparison was stopped").into(),
            secondary: Some(tr!("Editing is disabled until the files are compared.").into()),
            actions: vec![(tr!("Compare").into(), BannerAction::CompareAgain)],
        });
    }

    pub(super) fn diff_done(
        &mut self,
        generation: u64,
        result: Result<Box<DiffDone>, meld_diff::DiffError>,
    ) -> Task<Message> {
        if generation != self.diff_gen {
            return Task::none();
        }
        self.job = None;
        let done = match result {
            Ok(done) => done,
            Err(meld_diff::DiffError::Cancelled) => {
                self.show_stopped();
                return Task::none();
            }
            Err(err) => {
                self.refresh_read_only();
                self.dialog = Some(Dialog::Error {
                    primary: tr!("Comparison results will be inaccurate").into(),
                    secondary: err.to_string(),
                });
                return Task::none();
            }
        };
        let DiffDone {
            kind,
            comparison,
            languages,
            self_compare,
        } = *done;
        self.self_compare = self_compare;
        let cmp = *comparison;
        if kind == DiffKind::Initial && self.auto_merge && self.panes.len() == 3 {
            // `_merge_files`: the merged text replaces the middle pane.
            if let Some(merged) = cmp.buffer(1) {
                let p = &mut self.panes[1];
                p.buffer.set_lines(merged.lines().to_vec());
                p.highlighter.reset();
                p.extra_modified = true;
            }
        }
        self.cmp = Some(cmp);
        for pane in &mut self.panes {
            pane.sync_lines.clear();
        }
        for (i, pane) in self.panes.iter_mut().enumerate() {
            pane.sync_lines = self.syncpoints.lines(i);
        }
        self.after_change();
        self.refresh_read_only();

        if kind != DiffKind::Initial {
            return Task::none();
        }
        if let Some(langs) = languages {
            for (pane, lang) in self.panes.iter_mut().zip(langs) {
                if !pane.language_chosen {
                    pane.language = lang;
                    pane.highlighter = match lang {
                        Some(id) => meld_widgets::Highlighter::for_language(id),
                        None => meld_widgets::Highlighter::plain(),
                    };
                }
            }
        }
        for pane in &mut self.panes {
            pane.buffer.set_cursor(WPos::new(0, 0), false);
        }
        let focus = if self.panes.len() < 2 { 0 } else { 1 };
        self.last_focused = Some(focus);
        let target = {
            let differ = self.cmp.as_ref().map(|c| c.differ());
            differ.and_then(|d| {
                d.conflicts().first().copied().or_else(|| {
                    let loc = d.locate_chunk(1.min(self.panes.len() - 1), 0);
                    loc.current.or(loc.next)
                })
            })
        };
        self.update_cursor(focus, true);
        self.go_to_chunk(target, Some(focus), true);
        iced::widget::operation::focus(self.panes[focus].id.clone())
    }

    /// `save_file`
    pub(super) fn save_file(&mut self, pane: usize, save_as: bool, force: bool) -> Task<Message> {
        let Some(p) = self.panes.get(pane) else {
            return Task::none();
        };
        if save_as || p.file.target().is_none() || !p.file.writable {
            let title = match pane {
                0 => tr!("Save Left Pane As"),
                1 if self.panes.len() == 3 => tr!("Save Middle Pane As"),
                _ => tr!("Save Right Pane As"),
            };
            let dir = p.file.target().and_then(|t| t.parent()).map(PathBuf::from);
            let name = p
                .file
                .target()
                .and_then(|t| t.file_name())
                .map(|n| n.to_string_lossy().into_owned());
            return crate::ui::chooser::save_file(title, dir, name)
                .map(move |path| Message::SaveAsChosen(pane, path));
        }
        self.do_save(pane, force)
    }

    pub(super) fn save_as_chosen(&mut self, pane: usize, path: PathBuf) -> Task<Message> {
        let p = &mut self.panes[pane];
        p.file.custom_label = None;
        p.file.path = Some(path);
        p.file.savefile = None;
        p.file.writable = true;
        if pane == 1 {
            self.merge_output = None;
        }
        self.do_save(pane, true)
    }

    fn do_save(&mut self, pane: usize, force: bool) -> Task<Message> {
        let p = &mut self.panes[pane];
        let Some(target) = p.file.target().cloned() else {
            return Task::none();
        };
        if !force && p.file.mtime.is_some() && p.file.disk_mtime != p.file.mtime {
            self.save_conflict(pane, &target);
            return Task::none();
        }
        let lines = p.buffer.lines();
        let mut text = lines.join("\n");
        if !text.is_empty() || lines.len() > 1 {
            text.push('\n');
        }
        // Saving a different file than the one loaded: no mtime check.
        let same_file = p.file.savefile.is_none();
        let options = SaveOptions {
            encoding: p.file.encoding,
            newline: Some(p.file.newline),
            backup: false,
            expected_mtime: if force || !same_file {
                None
            } else {
                p.file.mtime
            },
        };
        Task::perform(
            io::blocking(move || io::save(target, text, options)),
            move |r| Message::Saved(pane, r),
        )
    }

    fn save_conflict(&mut self, pane: usize, target: &std::path::Path) {
        self.panes[pane].banner = Some(Banner {
            id: MsgId::SaveConflict,
            kind: BannerKind::Warning,
            primary: tr!(
                "File {} has changed on disk since it was opened",
                target.display()
            ),
            secondary: Some(tr!("If you save it, any external changes will be lost.").into()),
            actions: vec![
                (tr!("Save Anyway").into(), BannerAction::SaveAnyway),
                (tr!("Don’t Save").into(), BannerAction::Hide),
            ],
        });
        self.close_state = CloseState::Normal;
    }

    pub(super) fn file_saved(
        &mut self,
        pane: usize,
        result: Result<Saved, SaveFailure>,
        _shared: &mut Shared,
        events: &mut Vec<Event>,
    ) -> Task<Message> {
        match result {
            Ok(saved) => {
                if let Some(target) = self.panes[pane].file.target() {
                    events.push(Event::FileChanged(target.clone()));
                }
                self.undo.checkpoint(pane);
                self.process_undo_events();
                let three_way = self.panes.len() == 3;
                let p = &mut self.panes[pane];
                p.extra_modified = false;
                p.file.mtime = saved.mtime;
                if p.file.savefile.is_none() {
                    p.file.disk_mtime = saved.mtime;
                }
                if pane == 1 && three_way {
                    self.middle_saved = true;
                }
                if p.banner
                    .as_ref()
                    .is_some_and(|b| b.id == MsgId::SaveConflict || b.id == MsgId::Changed)
                {
                    p.banner = None;
                }
                if self.close_state == CloseState::Closing
                    && !self.modified_flags().iter().any(|m| *m)
                {
                    return self.request_close(_shared, events);
                }
                Task::none()
            }
            Err(SaveFailure::ExternallyModified(path)) => {
                self.save_conflict(pane, &path);
                Task::none()
            }
            Err(SaveFailure::Error { path, message }) => {
                self.close_state = CloseState::Normal;
                self.dialog = Some(Dialog::Error {
                    primary: tr!("Could not save file {}.", path.display()),
                    secondary: message,
                });
                Task::none()
            }
        }
    }

    /// `request_close_cb("discard")`: offer to mark the conflict
    /// resolved, clean up and close.
    pub(super) fn finish_close(&mut self, events: &mut Vec<Event>) -> Task<Message> {
        if self.extras.prompt_resolve && self.middle_saved && !self.resolve_asked {
            self.resolve_asked = true;
            self.dialog = Some(Dialog::Resolve);
            return Task::none();
        }
        let temp_files = std::mem::take(&mut self.extras.temp_files);
        if !temp_files.is_empty() {
            std::thread::spawn(move || {
                for file in temp_files {
                    if let Ok(meta) = std::fs::metadata(&file) {
                        let mut perms = meta.permissions();
                        #[allow(clippy::permissions_set_readonly_false)]
                        perms.set_readonly(false);
                        let _ = std::fs::set_permissions(&file, perms);
                    }
                    let _ = std::fs::remove_file(&file);
                }
            });
        }
        if let Some(job) = self.job.take() {
            job.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        events.push(Event::Close {
            status: self.exit_status(),
        });
        Task::none()
    }

    /// The exit status reported to the command line: merges that were
    /// not saved, or saved with unresolved conflict markers, fail.
    fn exit_status(&self) -> i32 {
        let merging = self.merge_output.is_some() || self.auto_merge || self.extras.prompt_resolve;
        if !merging || self.panes.len() < 2 {
            return 0;
        }
        let pane = &self.panes[1];
        let unsaved = self.is_modified(1) || !self.middle_saved && self.auto_merge;
        let markers = pane
            .buffer
            .lines()
            .iter()
            .any(|l| l.starts_with(meld_diff::CONFLICT_MARKER));
        if unsaved || markers { 1 } else { 0 }
    }

    /// `confirm_unsaved_change_action`
    pub(super) fn confirm_discard(
        &mut self,
        panes: Option<Vec<usize>>,
        then: Pending,
    ) -> Task<Message> {
        let panes = panes.unwrap_or_else(|| (0..self.panes.len()).collect());
        let unsaved: Vec<String> = panes
            .into_iter()
            .filter(|&i| self.is_modified(i))
            .map(|i| self.panes[i].file.label())
            .collect();
        if unsaved.is_empty() {
            return self.run_pending(then);
        }
        self.dialog = Some(Dialog::ConfirmDiscard { unsaved, then });
        Task::none()
    }

    pub(super) fn run_pending(&mut self, pending: Pending) -> Task<Message> {
        match pending {
            Pending::Revert => {
                let paths: Vec<Option<PathBuf>> =
                    self.panes.iter().map(|p| p.file.path.clone()).collect();
                let encodings = self
                    .panes
                    .iter()
                    .map(|p| Some(p.file.encoding.encoding))
                    .collect();
                for pane in &mut self.panes {
                    pane.extra_modified = false;
                }
                self.undo.clear();
                self.load_files(paths, encodings)
            }
            Pending::ReloadPane(pane, encoding) => {
                let Some(path) = self.panes[pane].file.path.clone() else {
                    return Task::none();
                };
                let encoding = encoding.or(Some(self.panes[pane].file.encoding.encoding));
                self.set_file(pane, path, encoding)
            }
            Pending::OpenFile(pane, path) => {
                self.panes[pane].file.custom_label = None;
                self.set_file(pane, path, None)
            }
        }
    }

    pub(super) fn check_disk(&mut self) -> Task<Message> {
        if self
            .panes
            .iter()
            .any(|p| p.file.state == LoadState::Loading)
        {
            return Task::none();
        }
        let paths: Vec<Option<PathBuf>> = self.panes.iter().map(|p| p.file.path.clone()).collect();
        if paths.iter().all(Option::is_none) {
            return Task::none();
        }
        let generation = self.load_gen;
        Task::perform(io::blocking(move || io::mtimes(paths)), move |t| {
            Message::DiskTimes(generation, t)
        })
    }

    /// File monitor: offer to reload files changed on disk.
    pub(super) fn disk_times(
        &mut self,
        generation: u64,
        times: Vec<Option<std::time::SystemTime>>,
    ) {
        if generation != self.load_gen {
            return;
        }
        for (pane, time) in self.panes.iter_mut().zip(times) {
            let (Some(now), Some(seen)) = (time, pane.file.disk_mtime) else {
                continue;
            };
            if now > seen {
                pane.file.disk_mtime = Some(now);
                let name = pane
                    .file
                    .path
                    .as_ref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_default();
                pane.banner = Some(Banner {
                    id: MsgId::Changed,
                    kind: BannerKind::Warning,
                    primary: tr!("File {} has changed on disk", name),
                    secondary: Some(tr!("Do you want to reload the file?").into()),
                    actions: vec![
                        (tr!("Reload").into(), BannerAction::Reload),
                        (tr!("Hide").into(), BannerAction::Hide),
                    ],
                });
            }
        }
    }

    pub(super) fn banner_action(&mut self, pane: usize, action: BannerAction) -> Task<Message> {
        let clear_all = |diff: &mut FileDiff| {
            for p in &mut diff.panes {
                p.banner = None;
            }
        };
        match action {
            BannerAction::Hide => {
                let id = self
                    .panes
                    .get(pane)
                    .and_then(|p| p.banner.as_ref())
                    .map(|b| b.id);
                if matches!(
                    id,
                    Some(
                        MsgId::Same
                            | MsgId::SlowHighlight
                            | MsgId::LineTooLong
                            | MsgId::SelfCompare
                    )
                ) {
                    clear_all(self);
                } else if let Some(p) = self.panes.get_mut(pane) {
                    p.banner = None;
                }
                Task::none()
            }
            BannerAction::ShowUnfiltered => {
                clear_all(self);
                for f in &mut self.text_filters {
                    f.active = false;
                }
                self.refresh_comparison()
            }
            BannerAction::KeepHighlighting => {
                clear_all(self);
                self.force_highlight = true;
                self.refresh_comparison()
            }
            BannerAction::LoadAnyway => {
                clear_all(self);
                self.force_load = true;
                let paths = self.panes.iter().map(|p| p.file.path.clone()).collect();
                let encodings = self
                    .panes
                    .iter()
                    .map(|p| Some(p.file.encoding.encoding))
                    .collect();
                self.load_files(paths, encodings)
            }
            BannerAction::OpenExternally => {
                if let Some(p) = self.panes.get_mut(pane) {
                    p.banner = None;
                    if let Some(path) = p.file.path.clone() {
                        return Task::perform(
                            io::blocking(move || {
                                let argv = meld_files::external::default_open_command(&path);
                                meld_files::spawn_detached(&argv).map_err(|e| e.to_string())
                            }),
                            Message::Done,
                        );
                    }
                }
                Task::none()
            }
            BannerAction::Reload => {
                if let Some(p) = self.panes.get_mut(pane) {
                    p.banner = None;
                }
                self.confirm_discard(Some(vec![pane]), Pending::ReloadPane(pane, None))
            }
            BannerAction::SaveAnyway => {
                if let Some(p) = self.panes.get_mut(pane) {
                    p.banner = None;
                }
                self.do_save(pane, true)
            }
            BannerAction::ChooseEncoding => {
                if let Some(p) = self.panes.get_mut(pane) {
                    p.banner = None;
                }
                self.dialog = Some(Dialog::Encoding {
                    pane,
                    filter: String::new(),
                });
                iced::widget::operation::focus(self.dialog_input_id.clone())
            }
            BannerAction::CompareAgain => {
                clear_all(self);
                if self.cmp.is_some() {
                    self.start_diff(DiffKind::Refresh)
                } else {
                    self.start_diff(DiffKind::Initial)
                }
            }
        }
    }

    /// The patch file chosen in the patch dialog.
    pub(super) fn save_patch(&mut self, path: Option<PathBuf>) -> Task<Message> {
        let (Some(path), Some(Dialog::Patch { text, .. })) = (path, &self.dialog) else {
            return Task::none();
        };
        let text = text.clone();
        self.dialog = None;
        Task::perform(
            io::blocking(move || {
                std::fs::write(&path, text).map_err(|e| {
                    format!(
                        "{}\n{}",
                        tr!("Could not save file {}.", path.display()),
                        tr!("Couldn’t save file due to:\n{}", e)
                    )
                })
            }),
            Message::Done,
        )
    }
}
