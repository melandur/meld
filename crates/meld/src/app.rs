//! The Meld window (`meldwindow.py`, `meldapp.py`): the tab strip, the main
//! menu, window-level shortcuts, dialogs and quitting.

use std::sync::Mutex;
use std::sync::atomic::{AtomicI32, Ordering};

use iced::widget::{button, column, container, row, scrollable, space, text};
use iced::{Element, Length, Size, Subscription, Task, Theme, keyboard, window};

use crate::cli::Launch;
use crate::command::{self, Command};
use crate::comparison::Comparison;
use crate::dialogs;
use crate::instance;
use crate::shared::Shared;
use crate::ui::{self, icon, menu};
use crate::views::{self, Event, Tab};

/// Exit status of the process: the status of the comparison opened from
/// the command line, when it was closed (0 otherwise).
pub static EXIT_STATUS: AtomicI32 = AtomicI32::new(0);

/// Comparisons from the command line, handed to `boot`.
pub static LAUNCH: Mutex<Option<Launch>> = Mutex::new(None);

type TabId = u64;

struct TabEntry {
    id: TabId,
    tab: Tab,
    /// Remote command-line connection waiting for this tab to close.
    waiter: Option<instance::Waiter>,
    /// The tab opened by our own command line.
    primary: bool,
}

pub struct Meld {
    shared: Shared,
    tabs: Vec<TabEntry>,
    active: Option<TabId>,
    next_id: TabId,
    open_menu: Option<OpenMenu>,
    dialog: Option<Dialog>,
    /// The built-in file chooser, when no system one is available.
    chooser: Option<ui::chooser::Dialog>,
    notice: Option<String>,
    fullscreen: bool,
    window: Option<window::Id>,
    /// Tabs being closed as part of quitting.
    quitting: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenMenu {
    Main,
    Recent,
}

enum Dialog {
    Preferences(Box<dialogs::preferences::Preferences>),
    Shortcuts,
    About,
    ConfirmQuit(Vec<String>),
}

#[derive(Debug, Clone)]
pub enum Message {
    Tab(TabId, views::Message),
    SelectTab(TabId),
    CloseTab(TabId),
    NewTab,
    Command(Command),
    Open(Comparison),
    ToggleMenu(OpenMenu),
    CloseMenu,
    ShowDialog(DialogKind),
    CloseDialog,
    Preferences(dialogs::preferences::Message),
    QuitAnyway,
    WindowOpened(window::Id),
    WindowResized(Size),
    CloseRequested,
    Remote(instance::Request),
    DismissNotice,
    /// A menu item was chosen: close the menu, then act.
    MenuPick(Box<Message>),
    ChooserRequest(ui::chooser::Request),
    Chooser(ui::chooser::Message),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DialogKind {
    Preferences,
    Shortcuts,
    About,
}

impl Meld {
    pub fn boot() -> (Self, Task<Message>) {
        let shared = Shared::load();
        let mut app = Meld {
            shared,
            tabs: Vec::new(),
            active: None,
            next_id: 1,
            open_menu: None,
            dialog: None,
            chooser: None,
            notice: None,
            fullscreen: false,
            window: None,
            quitting: false,
        };
        let launch = LAUNCH.lock().ok().and_then(|mut l| l.take()).unwrap_or_default();
        let mut tasks = Vec::new();
        for (i, comparison) in launch.comparisons.into_iter().enumerate() {
            tasks.push(app.open(comparison, None, i == 0));
        }
        if app.tabs.is_empty() {
            tasks.push(app.new_tab());
        }
        if let Some(first) = app.tabs.first() {
            app.active = Some(first.id);
        }
        tasks.push(window::latest().and_then(|id| Task::done(Message::WindowOpened(id))));
        (app, Task::batch(tasks))
    }

    pub fn title(&self) -> String {
        match self.active_tab() {
            Some(entry) => format!("{} - Meld", entry.tab.title()),
            None => "Meld".into(),
        }
    }

    pub fn theme(&self) -> Theme {
        self.shared.theme.clone()
    }

    pub fn window_settings(shared_size: Size) -> window::Settings {
        window::Settings {
            size: shared_size,
            min_size: Some(Size::new(500.0, 350.0)),
            exit_on_close_request: false,
            icon: window_icon(),
            #[cfg(target_os = "linux")]
            platform_specific: window::settings::PlatformSpecific {
                application_id: APP_ID.into(),
                ..Default::default()
            },
            ..window::Settings::default()
        }
    }

    fn active_tab(&self) -> Option<&TabEntry> {
        self.active.and_then(|id| self.tabs.iter().find(|t| t.id == id))
    }

    fn tab_mut(&mut self, id: TabId) -> Option<&mut TabEntry> {
        self.tabs.iter_mut().find(|t| t.id == id)
    }

    fn add_tab(&mut self, tab: Tab, task: Task<views::Message>, waiter: Option<instance::Waiter>, primary: bool) -> Task<Message> {
        let id = self.next_id;
        self.next_id += 1;
        // Open next to the current tab, like Meld does.
        let index = self
            .active
            .and_then(|a| self.tabs.iter().position(|t| t.id == a))
            .map_or(self.tabs.len(), |i| i + 1);
        self.tabs.insert(
            index,
            TabEntry {
                id,
                tab,
                waiter,
                primary,
            },
        );
        self.active = Some(id);
        task.map(move |m| Message::Tab(id, m))
    }

    fn new_tab(&mut self) -> Task<Message> {
        let tab = Tab::new_comparison(&self.shared);
        self.add_tab(tab, Task::none(), None, false)
    }

    fn open(&mut self, comparison: Comparison, waiter: Option<instance::Waiter>, primary: bool) -> Task<Message> {
        match Tab::open(comparison.clone(), &mut self.shared) {
            Ok((tab, task)) => {
                self.shared.add_recent(&comparison);
                self.add_tab(tab, task, waiter, primary)
            }
            Err(err) => {
                if let Some(w) = waiter {
                    w.finish(1, Some(&err));
                }
                self.notice = Some(err);
                Task::none()
            }
        }
    }

    pub fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::Tab(id, message) => {
                let mut events = Vec::new();
                let Some(entry) = self.tabs.iter_mut().find(|t| t.id == id) else {
                    return Task::none();
                };
                let task = entry.tab.update(message, &mut self.shared, &mut events);
                let task = task.map(move |m| Message::Tab(id, m));
                Task::batch([task, self.handle_events(id, events)])
            }
            Message::SelectTab(id) => {
                self.active = Some(id);
                self.open_menu = None;
                Task::none()
            }
            Message::CloseTab(id) => self.request_close(id),
            Message::NewTab => {
                self.open_menu = None;
                self.new_tab()
            }
            Message::Command(command) => self.command(command),
            Message::Open(comparison) => {
                self.open_menu = None;
                self.open(comparison, None, false)
            }
            Message::ToggleMenu(menu) => {
                self.open_menu = if self.open_menu == Some(menu) { None } else { Some(menu) };
                Task::none()
            }
            Message::CloseMenu => {
                self.open_menu = None;
                Task::none()
            }
            Message::ShowDialog(kind) => {
                self.open_menu = None;
                self.dialog = Some(match kind {
                    DialogKind::Preferences => {
                        Dialog::Preferences(Box::new(dialogs::preferences::Preferences::new(&self.shared)))
                    }
                    DialogKind::Shortcuts => Dialog::Shortcuts,
                    DialogKind::About => Dialog::About,
                });
                Task::none()
            }
            Message::CloseDialog => {
                self.dialog = None;
                self.quitting = false;
                Task::none()
            }
            Message::Preferences(message) => {
                let Some(Dialog::Preferences(prefs)) = &mut self.dialog else {
                    return Task::none();
                };
                let outcome = prefs.update(message, &mut self.shared);
                let mut tasks = Vec::new();
                if outcome.changed {
                    self.shared.save_settings();
                    tasks.push(self.settings_changed());
                }
                if outcome.close {
                    self.dialog = None;
                }
                tasks.push(outcome.task.map(Message::Preferences));
                Task::batch(tasks)
            }
            Message::QuitAnyway => self.exit(),
            Message::WindowOpened(id) => {
                self.window = Some(id);
                if self.shared.settings.window_state.is_maximized {
                    window::maximize(id, true)
                } else {
                    Task::none()
                }
            }
            Message::WindowResized(size) => {
                if !self.fullscreen {
                    self.shared.settings.window_state.width = size.width as i32;
                    self.shared.settings.window_state.height = size.height as i32;
                }
                Task::none()
            }
            Message::CloseRequested => self.quit(),
            Message::Remote(request) => {
                let mut tasks = Vec::new();
                let waiter = request.waiter;
                let count = request.launch.comparisons.len();
                if count == 0 {
                    tasks.push(self.new_tab());
                    waiter.finish(0, None);
                } else {
                    // The remote command line waits for its first comparison.
                    let mut waiter = Some(waiter);
                    for comparison in request.launch.comparisons {
                        tasks.push(self.open(comparison, waiter.take(), false));
                    }
                }
                if let Some(id) = self.window {
                    tasks.push(window::gain_focus(id));
                }
                Task::batch(tasks)
            }
            Message::MenuPick(message) => {
                self.open_menu = None;
                self.update(*message)
            }
            Message::DismissNotice => {
                self.notice = None;
                Task::none()
            }
            Message::ChooserRequest(request) => {
                if let Some(old) = self.chooser.take() {
                    old.cancel();
                }
                self.open_menu = None;
                let (chooser, task) = ui::chooser::Dialog::new(request);
                self.chooser = Some(chooser);
                task.map(Message::Chooser)
            }
            Message::Chooser(message) => {
                let Some(chooser) = &mut self.chooser else {
                    return Task::none();
                };
                let (closed, task) = chooser.update(message);
                if closed {
                    self.chooser = None;
                }
                task.map(Message::Chooser)
            }
        }
    }

    fn handle_events(&mut self, id: TabId, events: Vec<Event>) -> Task<Message> {
        let mut tasks = Vec::new();
        for event in events {
            match event {
                Event::Open(comparison) => tasks.push(self.open(comparison, None, false)),
                Event::Replace(comparison) => match Tab::open(comparison.clone(), &mut self.shared) {
                    Ok((tab, task)) => {
                        self.shared.add_recent(&comparison);
                        if let Some(entry) = self.tab_mut(id) {
                            entry.tab = tab;
                        }
                        tasks.push(task.map(move |m| Message::Tab(id, m)));
                    }
                    Err(err) => self.notice = Some(err),
                },
                Event::Close { status } => tasks.push(self.remove_tab(id, status)),
                Event::Notify(notice) => self.notice = Some(notice),
                Event::SettingsChanged => {
                    self.shared.save_settings();
                    tasks.push(self.settings_changed());
                }
                Event::Recent(comparison) => self.shared.add_recent(&comparison),
                Event::FileChanged(path) => {
                    for entry in self.tabs.iter_mut().filter(|t| t.id != id) {
                        let other = entry.id;
                        tasks.push(entry.tab.file_changed(&path).map(move |m| Message::Tab(other, m)));
                    }
                }
            }
        }
        Task::batch(tasks)
    }

    fn settings_changed(&mut self) -> Task<Message> {
        self.shared.refresh_theme();
        let shared = &self.shared;
        Task::batch(self.tabs.iter_mut().map(|entry| {
            let id = entry.id;
            entry.tab.settings_changed(shared).map(move |m| Message::Tab(id, m))
        }))
    }

    fn request_close(&mut self, id: TabId) -> Task<Message> {
        let mut events = Vec::new();
        let Some(entry) = self.tabs.iter_mut().find(|t| t.id == id) else {
            return Task::none();
        };
        let task = entry.tab.request_close(&mut self.shared, &mut events);
        let task = task.map(move |m| Message::Tab(id, m));
        Task::batch([task, self.handle_events(id, events)])
    }

    fn remove_tab(&mut self, id: TabId, status: i32) -> Task<Message> {
        let Some(index) = self.tabs.iter().position(|t| t.id == id) else {
            return Task::none();
        };
        let entry = self.tabs.remove(index);
        if entry.primary {
            EXIT_STATUS.store(status, Ordering::SeqCst);
        }
        if let Some(waiter) = entry.waiter {
            waiter.finish(status, None);
        }
        if self.active == Some(id) {
            self.active = self
                .tabs
                .get(index.min(self.tabs.len().saturating_sub(1)))
                .map(|t| t.id);
        }
        if self.quitting {
            return self.quit();
        }
        // Never leave an empty window: offer a new comparison instead.
        if self.tabs.is_empty() {
            return self.new_tab();
        }
        Task::none()
    }

    /// Closes every tab, asking about unsaved changes first, then exits.
    fn quit(&mut self) -> Task<Message> {
        let unsaved: Vec<String> = self.tabs.iter().flat_map(|t| t.tab.unsaved()).collect();
        if !unsaved.is_empty() {
            self.dialog = Some(Dialog::ConfirmQuit(unsaved));
            return Task::none();
        }
        self.exit()
    }

    fn exit(&mut self) -> Task<Message> {
        for entry in self.tabs.drain(..) {
            if let Some(waiter) = entry.waiter {
                waiter.finish(0, None);
            }
        }
        self.shared.save_settings();
        iced::exit()
    }

    fn command(&mut self, command: Command) -> Task<Message> {
        // The file chooser handles its own keys (`chooser::keys`).
        if self.chooser.is_some() {
            return Task::none();
        }
        // Escape closes menus and dialogs before it reaches a tab.
        if command == Command::Stop && (self.open_menu.is_some() || self.dialog.is_some()) {
            self.open_menu = None;
            self.dialog = None;
            self.quitting = false;
            return Task::none();
        }
        if self.dialog.is_some() {
            return Task::none();
        }
        let index = self
            .active
            .and_then(|a| self.tabs.iter().position(|t| t.id == a));
        match command {
            Command::Quit => self.quit(),
            Command::NewTab => self.new_tab(),
            Command::Preferences => self.update(Message::ShowDialog(DialogKind::Preferences)),
            Command::Help => {
                let _ = open::that_detached("https://meld.app/help/");
                Task::none()
            }
            Command::GearMenu => self.update(Message::ToggleMenu(OpenMenu::Main)),
            Command::CloseTab => match self.active {
                Some(id) => self.request_close(id),
                None => Task::none(),
            },
            Command::NextTab | Command::PreviousTab => {
                if let Some(i) = index {
                    let n = self.tabs.len();
                    let next = if command == Command::NextTab { (i + 1) % n } else { (i + n - 1) % n };
                    self.active = Some(self.tabs[next].id);
                }
                Task::none()
            }
            Command::SelectTab(i) => {
                if let Some(entry) = self.tabs.get(i) {
                    self.active = Some(entry.id);
                }
                Task::none()
            }
            Command::Fullscreen => {
                self.fullscreen = !self.fullscreen;
                let mode = if self.fullscreen {
                    window::Mode::Fullscreen
                } else {
                    window::Mode::Windowed
                };
                match self.window {
                    Some(id) => window::set_mode(id, mode),
                    None => Task::none(),
                }
            }
            Command::ZoomIn | Command::ZoomOut | Command::ZoomReset => {
                self.shared.zoom = match command {
                    Command::ZoomIn => (self.shared.zoom + 1.0).min(24.0),
                    Command::ZoomOut => (self.shared.zoom - 1.0).max(-6.0),
                    _ => 0.0,
                };
                self.settings_changed()
            }
            _ => {
                let Some(id) = self.active else {
                    return Task::none();
                };
                let mut events = Vec::new();
                let Some(entry) = self.tabs.iter_mut().find(|t| t.id == id) else {
                    return Task::none();
                };
                let task = entry.tab.command(command, &mut self.shared, &mut events);
                let task = task.map(move |m| Message::Tab(id, m));
                Task::batch([task, self.handle_events(id, events)])
            }
        }
    }

    pub fn subscription(&self) -> Subscription<Message> {
        let mut subs = vec![
            keyboard::listen().filter_map(|event| match event {
                keyboard::Event::KeyPressed { key, modifiers, .. } => {
                    command::from_key(&key, modifiers).map(Message::Command)
                }
                _ => None,
            }),
            window::close_requests().map(|_| Message::CloseRequested),
            window::resize_events().map(|(_, size)| Message::WindowResized(size)),
            instance::listen().map(Message::Remote),
            ui::chooser::requests().map(Message::ChooserRequest),
        ];
        if self.chooser.is_some() {
            subs.push(ui::chooser::keys().map(Message::Chooser));
        }
        for entry in &self.tabs {
            let id = entry.id;
            subs.push(entry.tab.subscription().with(id).map(|(id, m)| Message::Tab(id, m)));
        }
        Subscription::batch(subs)
    }

    pub fn view(&self) -> Element<'_, Message> {
        let header = self.header();
        let body: Element<'_, Message> = match self.active_tab() {
            Some(entry) => {
                let id = entry.id;
                entry.tab.view(&self.shared).map(move |m| Message::Tab(id, m))
            }
            None => container(text(tr!("No comparisons open")).style(ui::dim_text))
                .center(Length::Fill)
                .into(),
        };

        let mut content = column![header, ui::separator(false)];
        if let Some(notice) = &self.notice {
            content = content.push(ui::banner(
                ui::BannerKind::Error,
                notice.as_str(),
                None,
                vec![(tr!("Dismiss").to_string(), Message::DismissNotice)],
            ));
        }
        let mut base: Element<'_, Message> = content.push(body).into();

        if let Some(open_menu) = self.open_menu {
            let (items, anchor) = match open_menu {
                OpenMenu::Main => (self.main_menu(), menu::Anchor::TopRight(6.0)),
                OpenMenu::Recent => (self.recent_menu(), menu::Anchor::TopRight(48.0)),
            };
            let items = menu::map_items(items, |m| Message::MenuPick(Box::new(m)));
            base = menu::overlay(base, items, anchor, Message::CloseMenu);
        }

        let page = match &self.dialog {
            None => base,
            Some(Dialog::Preferences(prefs)) => ui::modal::modal(
                base,
                prefs.view(&self.shared).map(Message::Preferences),
                Message::CloseDialog,
            ),
            Some(Dialog::Shortcuts) => ui::modal::modal(base, dialogs::shortcuts(Message::CloseDialog), Message::CloseDialog),
            Some(Dialog::About) => ui::modal::modal(base, dialogs::about(Message::CloseDialog), Message::CloseDialog),
            Some(Dialog::ConfirmQuit(unsaved)) => ui::modal::modal(
                base,
                dialogs::confirm_quit(unsaved, Message::CloseDialog, Message::QuitAnyway),
                Message::CloseDialog,
            ),
        };
        match &self.chooser {
            Some(chooser) => ui::modal::modal(
                page,
                chooser.view().map(Message::Chooser),
                Message::Chooser(ui::chooser::Message::Ignore),
            ),
            None => page,
        }
    }

    fn header(&self) -> Element<'_, Message> {
        let mut tabs = row![].spacing(2).align_y(iced::Center);
        for entry in &self.tabs {
            let selected = Some(entry.id) == self.active;
            let modified = !entry.tab.unsaved().is_empty();
            // The title is clipped to a fixed width so the close button
            // always keeps its place, whatever the title's length.
            let mut label = row![].spacing(6).align_y(iced::Center);
            if modified {
                label = label.push(text(icon::MODIFIED).size(10));
            }
            label = label.push(
                container(text(shorten_middle(&entry.tab.title(), 30)).size(14).wrapping(text::Wrapping::None))
                    .max_width(240)
                    .clip(true),
            );
            let select = button(label)
                .padding(iced::Padding::from([4, 6]).right(2))
                .style(ui::flat_button)
                .on_press(Message::SelectTab(entry.id));
            let close = button(text(icon::CLOSE).size(11))
                .padding([1, 5])
                .style(ui::flat_button)
                .on_press(Message::CloseTab(entry.id));
            let tab_button = iced::widget::mouse_area(
                container(row![select, close].spacing(2).align_y(iced::Center))
                    .padding(iced::Padding::ZERO.right(4))
                    .style(ui::tab_container(selected)),
            )
            .on_middle_press(Message::CloseTab(entry.id));
            tabs = tabs.push(iced::widget::tooltip(
                tab_button,
                container(text(entry.tab.tooltip()).size(13)).padding([4, 8]).style(ui::tooltip_box),
                iced::widget::tooltip::Position::Bottom,
            ));
        }
        tabs = tabs.push(ui::tool_button(icon::ADD, tr!("New comparison (Ctrl+N)"), Some(Message::NewTab)));

        let toolbar: Element<'_, Message> = match self.active_tab() {
            Some(entry) => {
                let id = entry.id;
                entry.tab.toolbar(&self.shared).map(move |m| Message::Tab(id, m))
            }
            None => space::horizontal().into(),
        };

        container(
            row![
                scrollable(tabs)
                    .direction(scrollable::Direction::Horizontal(
                        scrollable::Scrollbar::new().width(2).scroller_width(2)
                    ))
                    .width(Length::FillPortion(3)),
                container(toolbar).width(Length::FillPortion(2)).align_right(Length::FillPortion(2)),
                ui::tool_button("🕘", tr!("Recent comparisons"), Some(Message::ToggleMenu(OpenMenu::Recent))),
                ui::tool_button(icon::MENU, tr!("Main menu (F10)"), Some(Message::ToggleMenu(OpenMenu::Main))),
            ]
            .spacing(8)
            .align_y(iced::Center),
        )
        .padding([0, 6])
        .height(ui::HEADER_HEIGHT)
        .align_y(iced::Center)
        .style(ui::header_bar)
        .into()
    }

    fn main_menu(&self) -> Vec<menu::Item<Message>> {
        let mut items = Vec::new();
        if let Some(entry) = self.active_tab() {
            let id = entry.id;
            let tab_items = entry.tab.menu_items(&self.shared);
            if !tab_items.is_empty() {
                items.extend(tab_items.into_iter().map(|item| match item {
                    menu::Item::Action {
                        label,
                        shortcut,
                        message,
                    } => menu::Item::Action {
                        label,
                        shortcut,
                        message: message.map(|m| Message::Tab(id, m)),
                    },
                    menu::Item::Toggle {
                        label,
                        checked,
                        message,
                    } => menu::Item::Toggle {
                        label,
                        checked,
                        message: Message::Tab(id, message),
                    },
                    menu::Item::Separator => menu::Item::Separator,
                    menu::Item::Heading(h) => menu::Item::Heading(h),
                }));
                items.push(menu::Item::Separator);
            }
        }
        items.push(menu::Item::toggle(tr!("Fullscreen"), self.fullscreen, Message::Command(Command::Fullscreen)));
        items.push(menu::Item::action(tr!("Zoom In"), Some("Ctrl++"), Message::Command(Command::ZoomIn)));
        items.push(menu::Item::action(tr!("Zoom Out"), Some("Ctrl+-"), Message::Command(Command::ZoomOut)));
        items.push(menu::Item::action(tr!("Reset Zoom"), Some("Ctrl+0"), Message::Command(Command::ZoomReset)));
        items.push(menu::Item::Separator);
        items.push(menu::Item::action(tr!("Preferences"), Some("Ctrl+,"), Message::ShowDialog(DialogKind::Preferences)));
        items.push(menu::Item::action(tr!("Keyboard Shortcuts"), None, Message::ShowDialog(DialogKind::Shortcuts)));
        items.push(menu::Item::action(tr!("Help"), Some("F1"), Message::Command(Command::Help)));
        items.push(menu::Item::action(tr!("About Meld"), None, Message::ShowDialog(DialogKind::About)));
        items.push(menu::Item::Separator);
        items.push(menu::Item::action(tr!("Quit"), Some("Ctrl+Q"), Message::Command(Command::Quit)));
        items
    }

    fn recent_menu(&self) -> Vec<menu::Item<Message>> {
        let recent = self.shared.recent_comparisons();
        if recent.is_empty() {
            return vec![menu::Item::disabled(tr!("No recent comparisons"))];
        }
        recent
            .into_iter()
            .map(|(label, comparison)| menu::Item::action(label, None, Message::Open(comparison)))
            .collect()
    }
}

/// Wayland app id; matches `crates/meld/data/org.gnome.Meld.Rust.desktop`,
/// which is how compositors find the icon. Kept apart from the Python
/// build's `org.gnome.Meld` so the two installs don't shadow each other.
pub const APP_ID: &str = "org.gnome.Meld.Rust";

/// The dark Meld icon, rasterised from the bundled SVG for the window.
fn window_icon() -> Option<window::Icon> {
    const SVG: &[u8] = include_bytes!("../../../data/icons/hicolor/scalable/apps/org.gnome.Meld-dark.svg");
    const SIZE: u32 = 256;

    let tree = resvg::usvg::Tree::from_data(SVG, &resvg::usvg::Options::default()).ok()?;
    let mut pixmap = resvg::tiny_skia::Pixmap::new(SIZE, SIZE)?;
    let scale = SIZE as f32 / tree.size().width().max(tree.size().height());
    resvg::render(&tree, resvg::tiny_skia::Transform::from_scale(scale, scale), &mut pixmap.as_mut());

    let rgba = pixmap
        .pixels()
        .iter()
        .flat_map(|p| {
            let c = p.demultiply();
            [c.red(), c.green(), c.blue(), c.alpha()]
        })
        .collect();
    window::icon::from_rgba(rgba, SIZE, SIZE).ok()
}

#[cfg(test)]
mod tests {
    #[test]
    fn window_icon_renders() {
        assert!(super::window_icon().is_some());
    }
}

/// Shortens `text` to at most `max` characters with an ellipsis in the
/// middle, keeping both ends readable (tab labels).
fn shorten_middle(text: &str, max: usize) -> String {
    let count = text.chars().count();
    if count <= max {
        return text.to_string();
    }
    let keep = max.saturating_sub(1);
    let head = keep.div_ceil(2);
    let tail = keep - head;
    let start: String = text.chars().take(head).collect();
    let end: String = text.chars().skip(count - tail).collect();
    format!("{start}…{end}")
}

#[cfg(test)]
mod title_tests {
    use super::shorten_middle;

    #[test]
    fn shortens_long_tab_titles_in_the_middle() {
        assert_eq!(shorten_middle("short", 36), "short");
        let long = "7182167 → Staged (agent-a0836974b5990db9e)";
        let short = shorten_middle(long, 20);
        assert_eq!(short.chars().count(), 20);
        assert!(short.starts_with("7182167") && short.ends_with("db9e)"));
    }
}
