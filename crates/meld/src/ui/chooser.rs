//! File and folder choosers (`Gtk.FileChooserNative` in the Python
//! version), shared by every view.
//!
//! [`pick_file`], [`pick_folder`] and [`save_file`] return a task that
//! resolves to the chosen path, or `None` when the user cancels. They use,
//! in order:
//!
//! 1. the XDG desktop portal's `FileChooser`, through rfd, when the session
//!    bus has one;
//! 2. `zenity`, then `kdialog`, when installed;
//! 3. a dialog drawn in the Meld window ([`Dialog`]): a path entry with
//!    completion, so that a path can always be picked, even on a bare
//!    window manager without a portal.
//!
//! `MELD_FILE_CHOOSER=portal|zenity|kdialog|builtin` picks one directly
//! (falling back to the built-in dialog when it is not available).
//!
//! The built-in dialog lives in the window: the app subscribes to
//! [`requests`], shows a [`Dialog`] for each request and routes
//! [`Message`]s to it; the dialog answers the waiting task itself.

use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use iced::futures::SinkExt;
use iced::widget::{Id, button, column, container, scrollable, text, text_input};
use iced::{Element, Length, Subscription, Task, keyboard};
use tokio::sync::{Notify, oneshot};

use super::modal;

/// What the chooser picks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    OpenFile,
    OpenFolder,
    Save,
}

#[derive(Debug, Clone)]
struct Options {
    mode: Mode,
    title: String,
    /// The folder shown first.
    dir: Option<PathBuf>,
    /// The suggested file name when saving.
    name: Option<String>,
}

/// Asks for an existing file, starting in `dir`.
pub fn pick_file(title: &str, dir: Option<PathBuf>) -> Task<Option<PathBuf>> {
    run(Mode::OpenFile, title, dir, None)
}

/// Asks for an existing folder, starting in `dir`.
pub fn pick_folder(title: &str, dir: Option<PathBuf>) -> Task<Option<PathBuf>> {
    run(Mode::OpenFolder, title, dir, None)
}

/// Asks where to save a file, starting in `dir` with `name` filled in.
/// Replacing an existing file is confirmed by the chooser.
pub fn save_file(title: &str, dir: Option<PathBuf>, name: Option<String>) -> Task<Option<PathBuf>> {
    run(Mode::Save, title, dir, name)
}

fn run(mode: Mode, title: &str, dir: Option<PathBuf>, name: Option<String>) -> Task<Option<PathBuf>> {
    // Without a folder the portal would show its own working directory.
    let dir = dir
        .filter(|d| d.is_dir())
        .or_else(|| std::env::current_dir().ok());
    let options = Options {
        mode,
        title: title.to_string(),
        dir,
        name,
    };
    Task::perform(choose(options), std::convert::identity)
}

// --- Backends ---------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Backend {
    Portal,
    Zenity,
    Kdialog,
    Builtin,
}

fn backend_from_env(value: Option<&str>) -> Option<Backend> {
    match value?.trim().to_ascii_lowercase().as_str() {
        "portal" => Some(Backend::Portal),
        "zenity" => Some(Backend::Zenity),
        "kdialog" => Some(Backend::Kdialog),
        "builtin" | "internal" => Some(Backend::Builtin),
        _ => None,
    }
}

fn backends() -> Vec<Backend> {
    let forced = std::env::var("MELD_FILE_CHOOSER").ok();
    match backend_from_env(forced.as_deref()) {
        Some(backend) => vec![backend, Backend::Builtin],
        None => vec![Backend::Portal, Backend::Zenity, Backend::Kdialog, Backend::Builtin],
    }
}

async fn choose(options: Options) -> Option<PathBuf> {
    for backend in backends() {
        match backend {
            Backend::Portal => {
                if portal_available().await {
                    return portal(&options).await;
                }
            }
            Backend::Zenity | Backend::Kdialog => match external(backend, &options).await {
                Ok(path) => return path,
                Err(err) => log::info!("file chooser {backend:?} not available: {err}"),
            },
            Backend::Builtin => return builtin(options).await,
        }
    }
    None
}

async fn portal(options: &Options) -> Option<PathBuf> {
    let mut dialog = rfd::AsyncFileDialog::new().set_title(options.title.as_str());
    if let Some(dir) = &options.dir {
        dialog = dialog.set_directory(dir);
    }
    if let Some(name) = &options.name {
        dialog = dialog.set_file_name(name.as_str());
    }
    let handle = match options.mode {
        Mode::OpenFile => dialog.pick_file().await,
        Mode::OpenFolder => dialog.pick_folder().await,
        Mode::Save => dialog.save_file().await,
    };
    handle.map(|h| h.path().to_path_buf())
}

const PORTAL_TIMEOUT: Duration = Duration::from_secs(10);

/// Whether the session bus has a portal with a `FileChooser`. Without one
/// rfd would fall back to zenity and, when that is missing too, return
/// nothing as if the user had cancelled.
async fn portal_available() -> bool {
    static AVAILABLE: Mutex<Option<bool>> = Mutex::new(None);
    if let Some(known) = *AVAILABLE.lock().unwrap_or_else(|e| e.into_inner()) {
        return known;
    }
    let probe = async {
        let conn = zbus::Connection::session().await?;
        conn.call_method(
            Some("org.freedesktop.portal.Desktop"),
            "/org/freedesktop/portal/desktop",
            Some("org.freedesktop.DBus.Properties"),
            "Get",
            &("org.freedesktop.portal.FileChooser", "version"),
        )
        .await?;
        Ok::<(), zbus::Error>(())
    };
    let available = match tokio::time::timeout(PORTAL_TIMEOUT, probe).await {
        Ok(Ok(())) => true,
        Ok(Err(err)) => {
            log::info!("no file chooser portal: {err}");
            false
        }
        Err(_) => {
            log::warn!("the desktop portal did not answer; not using it for file choosers");
            false
        }
    };
    *AVAILABLE.lock().unwrap_or_else(|e| e.into_inner()) = Some(available);
    available
}

/// The `--filename`/start argument: the folder (with a trailing slash so
/// that the dialog opens inside it), plus the suggested name.
fn start_path(options: &Options) -> Option<PathBuf> {
    let dir = options.dir.as_ref()?;
    let mut start = dir.as_os_str().to_owned();
    if !start.as_bytes().ends_with(b"/") {
        start.push("/");
    }
    if let Some(name) = &options.name {
        start.push(name);
    }
    Some(PathBuf::from(start))
}

fn zenity_args(options: &Options) -> Vec<std::ffi::OsString> {
    let mut args: Vec<std::ffi::OsString> = vec!["--file-selection".into()];
    args.push(format!("--title={}", options.title).into());
    match options.mode {
        Mode::OpenFile => {}
        Mode::OpenFolder => args.push("--directory".into()),
        // Overwrite confirmation is the default (and the flag a no-op)
        // from zenity 4 on.
        Mode::Save => args.extend(["--save".into(), "--confirm-overwrite".into()]),
    }
    if let Some(start) = start_path(options) {
        let mut arg = std::ffi::OsString::from("--filename=");
        arg.push(start);
        args.push(arg);
    }
    args
}

fn kdialog_args(options: &Options) -> Vec<std::ffi::OsString> {
    let flag = match options.mode {
        Mode::OpenFile => "--getopenfilename",
        Mode::OpenFolder => "--getexistingdirectory",
        Mode::Save => "--getsavefilename",
    };
    let start = start_path(options).unwrap_or_else(|| PathBuf::from("."));
    vec!["--title".into(), options.title.clone().into(), flag.into(), start.into_os_string()]
}

/// The outcome of a zenity/kdialog run: the path, `None` when cancelled,
/// or an error when the program could not show its dialog.
fn external_result(code: Option<i32>, stdout: &[u8], stderr: &[u8]) -> Result<Option<PathBuf>, String> {
    let stderr_text = String::from_utf8_lossy(stderr);
    let lower = stderr_text.to_lowercase();
    // GTK and Qt exit with 1 when there is no display, like a cancel.
    let broken = lower.contains("open display") || lower.contains("connect to display");
    match code {
        Some(0) => {
            let mut path = stdout;
            while let [rest @ .., b'\n' | b'\r'] = path {
                path = rest;
            }
            Ok((!path.is_empty()).then(|| PathBuf::from(OsStr::from_bytes(path))))
        }
        Some(1) if !broken => Ok(None),
        _ => Err(format!("exit status {code:?}: {}", stderr_text.trim())),
    }
}

async fn external(backend: Backend, options: &Options) -> Result<Option<PathBuf>, String> {
    let (program, args) = match backend {
        Backend::Zenity => ("zenity", zenity_args(options)),
        _ => ("kdialog", kdialog_args(options)),
    };
    let output = tokio::process::Command::new(program)
        .args(&args)
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true)
        .output()
        .await
        .map_err(|e| format!("{program}: {e}"))?;
    external_result(output.status.code(), &output.stdout, &output.stderr)
}

// --- Built-in dialog --------------------------------------------------------

/// A request for the built-in dialog, answered through [`Dialog`].
#[derive(Clone)]
pub struct Request {
    options: Options,
    reply: Arc<Mutex<Option<oneshot::Sender<Option<PathBuf>>>>>,
}

impl std::fmt::Debug for Request {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Request").field("options", &self.options).finish_non_exhaustive()
    }
}

impl Request {
    fn new(options: Options) -> (Self, oneshot::Receiver<Option<PathBuf>>) {
        let (tx, rx) = oneshot::channel();
        let request = Request {
            options,
            reply: Arc::new(Mutex::new(Some(tx))),
        };
        (request, rx)
    }

    fn answer(&self, path: Option<PathBuf>) {
        if let Some(tx) = self.reply.lock().ok().and_then(|mut r| r.take()) {
            let _ = tx.send(path);
        }
    }
}

static QUEUE: Mutex<Vec<Request>> = Mutex::new(Vec::new());

fn notify() -> &'static Notify {
    static NOTIFY: OnceLock<Notify> = OnceLock::new();
    NOTIFY.get_or_init(Notify::new)
}

async fn builtin(options: Options) -> Option<PathBuf> {
    let (request, reply) = Request::new(options);
    QUEUE.lock().unwrap_or_else(|e| e.into_inner()).push(request);
    notify().notify_one();
    reply.await.ok().flatten()
}

/// Requests for the built-in dialog. The app subscribes to this for its
/// whole life and opens a [`Dialog`] for each request.
pub fn requests() -> Subscription<Request> {
    Subscription::run(|| {
        iced::stream::channel(4, async |mut output| {
            loop {
                let pending = std::mem::take(&mut *QUEUE.lock().unwrap_or_else(|e| e.into_inner()));
                for request in pending {
                    if output.send(request).await.is_err() {
                        return;
                    }
                }
                notify().notified().await;
            }
        })
    })
}

/// Tab completes and Escape cancels, even while the path entry has focus
/// (it would keep Escape to itself). Subscribe while a dialog is open.
pub fn keys() -> Subscription<Message> {
    iced::event::listen_with(|event, _status, _window| match event {
        iced::Event::Keyboard(keyboard::Event::KeyPressed {
            key: keyboard::Key::Named(named),
            modifiers,
            ..
        }) if modifiers.is_empty() => match named {
            keyboard::key::Named::Tab => Some(Message::Complete),
            keyboard::key::Named::Escape => Some(Message::Cancel),
            _ => None,
        },
        _ => None,
    })
}

#[derive(Debug, Clone)]
pub enum Message {
    Input(String),
    /// Tab: complete the name being typed.
    Complete,
    /// A row of the listing was clicked.
    Entry(String),
    Accept,
    Cancel,
    /// A click outside the dialog: keep it open.
    Ignore,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Entry {
    name: String,
    is_dir: bool,
}

const MAX_ENTRIES: usize = 500;

/// The built-in chooser: a path entry with completion and a listing of
/// the folder being typed.
pub struct Dialog {
    request: Request,
    input: String,
    entries: Vec<Entry>,
    error: Option<String>,
    /// Saving over this existing file waits for a second confirmation.
    confirm: Option<PathBuf>,
    input_id: Id,
}

impl Dialog {
    pub fn new(request: Request) -> (Self, Task<Message>) {
        let mut input = match &request.options.dir {
            Some(dir) => {
                let mut s = dir.to_string_lossy().into_owned();
                if !s.ends_with('/') {
                    s.push('/');
                }
                s
            }
            None => String::new(),
        };
        if let Some(name) = &request.options.name {
            input.push_str(name);
        }
        let mut dialog = Dialog {
            request,
            input,
            entries: Vec::new(),
            error: None,
            confirm: None,
            input_id: Id::unique(),
        };
        dialog.refresh();
        let id = dialog.input_id.clone();
        let task = Task::batch([
            iced::widget::operation::focus(id.clone()),
            iced::widget::operation::move_cursor_to_end(id),
        ]);
        (dialog, task)
    }

    /// Answers the request with `None`, as if cancelled.
    pub fn cancel(&self) {
        self.request.answer(None);
    }

    fn mode(&self) -> Mode {
        self.request.options.mode
    }

    /// Updates the dialog; returns `true` once it has answered and should
    /// close.
    pub fn update(&mut self, message: Message) -> (bool, Task<Message>) {
        match message {
            Message::Input(value) => {
                self.input = value;
                self.error = None;
                self.confirm = None;
                self.refresh();
            }
            Message::Complete => {
                let (_, prefix) = split_input(&self.input);
                if let Some(completed) = complete(prefix, &self.entries) {
                    let dir_len = self.input.len() - prefix.len();
                    self.input.truncate(dir_len);
                    self.input.push_str(&completed);
                    self.confirm = None;
                    self.refresh();
                }
                return (false, self.cursor_to_end());
            }
            Message::Entry(name) if name == ".." => {
                let (dir, _) = split_input(&self.input);
                let dir = resolve(if dir.is_empty() { "./" } else { dir }, &self.base());
                let parent = dir.parent().unwrap_or(&dir).to_path_buf();
                return self.navigate(&parent);
            }
            Message::Entry(name) => {
                let (dir, _) = split_input(&self.input);
                let is_dir = self.entries.iter().any(|e| e.name == name && e.is_dir);
                self.input = format!("{dir}{name}{}", if is_dir { "/" } else { "" });
                self.error = None;
                self.confirm = None;
                self.refresh();
                return (false, self.cursor_to_end());
            }
            Message::Accept => return self.accept(),
            Message::Cancel => {
                self.cancel();
                return (true, Task::none());
            }
            Message::Ignore => {}
        }
        (false, Task::none())
    }

    fn cursor_to_end(&self) -> Task<Message> {
        let id = self.input_id.clone();
        Task::batch([
            iced::widget::operation::focus(id.clone()),
            iced::widget::operation::move_cursor_to_end(id),
        ])
    }

    fn base(&self) -> PathBuf {
        self.request
            .options
            .dir
            .clone()
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_else(|| PathBuf::from("/"))
    }

    fn navigate(&mut self, dir: &Path) -> (bool, Task<Message>) {
        let mut s = dir.to_string_lossy().into_owned();
        if !s.ends_with('/') {
            s.push('/');
        }
        self.input = s;
        self.error = None;
        self.confirm = None;
        self.refresh();
        (false, self.cursor_to_end())
    }

    fn accept(&mut self) -> (bool, Task<Message>) {
        if self.input.trim().is_empty() {
            self.error = Some("Type a path".into());
            return (false, Task::none());
        }
        let path = resolve(&self.input, &self.base());
        let typed_folder = self.input.ends_with('/');
        match self.mode() {
            Mode::OpenFile => {
                if path.is_dir() {
                    return self.navigate(&path);
                }
                if !path.is_file() {
                    self.error = Some(format!("No file named “{}”", path.display()));
                    return (false, Task::none());
                }
            }
            Mode::OpenFolder => {
                if !path.is_dir() {
                    self.error = Some(format!("No folder named “{}”", path.display()));
                    return (false, Task::none());
                }
            }
            Mode::Save => {
                if typed_folder || path.is_dir() {
                    if path.is_dir() {
                        return self.navigate(&path);
                    }
                    self.error = Some(format!("No folder named “{}”", path.display()));
                    return (false, Task::none());
                }
                match path.parent() {
                    Some(parent) if parent.is_dir() => {}
                    Some(parent) => {
                        self.error = Some(format!("No folder named “{}”", parent.display()));
                        return (false, Task::none());
                    }
                    None => {
                        self.error = Some("Type a file name".into());
                        return (false, Task::none());
                    }
                }
                if path.exists() && self.confirm.as_ref() != Some(&path) {
                    self.confirm = Some(path);
                    return (false, Task::none());
                }
            }
        }
        self.request.answer(Some(path));
        (true, Task::none())
    }

    /// Lists the folder typed so far, filtered by the name being typed.
    fn refresh(&mut self) {
        let (dir, prefix) = split_input(&self.input);
        let dir = resolve(if dir.is_empty() { "./" } else { dir }, &self.base());
        self.entries = list_dir(&dir, prefix, self.mode() == Mode::OpenFolder);
        if prefix.is_empty() && dir.parent().is_some() {
            self.entries.insert(
                0,
                Entry {
                    name: "..".into(),
                    is_dir: true,
                },
            );
        }
    }

    pub fn view(&self) -> Element<'_, Message> {
        let mut list = column![].spacing(0).width(Length::Fill);
        for entry in &self.entries {
            let label = if entry.is_dir {
                format!("{}/", entry.name)
            } else {
                entry.name.clone()
            };
            list = list.push(
                button(text(label).size(13))
                    .width(Length::Fill)
                    .padding([3, 8])
                    .style(super::flat_button)
                    .on_press(Message::Entry(entry.name.clone())),
            );
        }
        if self.entries.is_empty() {
            list = list.push(container(text("No matches").size(13).style(super::dim_text)).padding([3, 8]));
        }

        let status: Element<'_, Message> = if let Some(path) = &self.confirm {
            let name = path.file_name().map(|n| n.to_string_lossy()).unwrap_or_default();
            text(format!("A file named “{name}” already exists. Replace it?"))
                .size(13)
                .font(super::bold())
                .into()
        } else if let Some(error) = &self.error {
            text(error.as_str())
                .size(13)
                .style(|theme: &iced::Theme| text::Style {
                    color: Some(theme.extended_palette().danger.base.color),
                })
                .into()
        } else {
            text("Tab completes the name; Enter opens folders").size(12).style(super::dim_text).into()
        };

        let body = column![
            text_input("Path", &self.input)
                .id(self.input_id.clone())
                .size(14)
                .padding([4, 8])
                .on_input(Message::Input)
                .on_submit(Message::Accept),
            container(scrollable(list).height(280)).width(Length::Fill).style(listing_box),
            status,
        ]
        .spacing(8);

        let accept_label = match (self.mode(), self.confirm.is_some()) {
            (Mode::Save, true) => "Replace",
            (Mode::Save, false) => "Save",
            (Mode::OpenFolder, _) => "Select",
            (Mode::OpenFile, _) => "Open",
        };
        let accept_style = if self.confirm.is_some() {
            button::danger
        } else {
            button::primary
        };
        modal::dialog(
            self.request.options.title.as_str(),
            body,
            vec![
                button(text("Cancel")).style(button::secondary).on_press(Message::Cancel).into(),
                button(text(accept_label)).style(accept_style).on_press(Message::Accept).into(),
            ],
        )
    }
}

fn listing_box(theme: &iced::Theme) -> container::Style {
    let palette = theme.extended_palette();
    container::Style {
        border: iced::Border::default().rounded(4).width(1).color(palette.background.strong.color),
        ..container::Style::default()
    }
}

/// Splits the typed text into the folder part (up to and including the
/// last `/`) and the name being typed.
fn split_input(input: &str) -> (&str, &str) {
    match input.rfind('/') {
        Some(i) => input.split_at(i + 1),
        None => ("", input),
    }
}

/// Turns typed text into an absolute path: `~` is the home folder, and
/// relative paths are relative to `base`. `.` and trailing slashes go;
/// `..` is kept (it may cross a symbolic link).
fn resolve(input: &str, base: &Path) -> PathBuf {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let expanded = match (input, home) {
        ("~", Some(home)) => home,
        (s, Some(home)) if s.starts_with("~/") => home.join(&s[2..]),
        (s, _) => PathBuf::from(s),
    };
    let absolute = if expanded.is_absolute() {
        expanded
    } else {
        base.join(expanded)
    };
    absolute
        .components()
        .filter(|c| !matches!(c, Component::CurDir))
        .collect()
}

fn list_dir(dir: &Path, prefix: &str, folders_only: bool) -> Vec<Entry> {
    let Ok(read) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let show_hidden = prefix.starts_with('.');
    let mut entries: Vec<Entry> = read
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            if !name.starts_with(prefix) || (name.starts_with('.') && !show_hidden) {
                return None;
            }
            // Follows symbolic links, like the file dialogs.
            let is_dir = e.path().is_dir();
            (is_dir || !folders_only).then_some(Entry { name, is_dir })
        })
        .take(MAX_ENTRIES)
        .collect();
    entries.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
            .then_with(|| a.name.cmp(&b.name))
    });
    entries
}

/// Completes `prefix` against the listing: to the only match (with a `/`
/// for folders), or to the longest prefix the matches share. `None` when
/// that adds nothing.
fn complete(prefix: &str, entries: &[Entry]) -> Option<String> {
    let matches: Vec<&Entry> = entries.iter().filter(|e| e.name.starts_with(prefix)).collect();
    match matches.as_slice() {
        [] => None,
        [only] => Some(format!("{}{}", only.name, if only.is_dir { "/" } else { "" })),
        [first, rest @ ..] => {
            let mut common = first.name.as_str();
            for e in rest {
                let len = common
                    .char_indices()
                    .zip(e.name.chars())
                    .find(|((_, a), b)| a != b)
                    .map(|((i, _), _)| i)
                    .unwrap_or_else(|| common.len().min(e.name.len()));
                common = &common[..len];
            }
            (common.len() > prefix.len()).then(|| common.to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn options(mode: Mode, dir: Option<&Path>, name: Option<&str>) -> Options {
        Options {
            mode,
            title: "Pick".into(),
            dir: dir.map(Path::to_path_buf),
            name: name.map(str::to_string),
        }
    }

    fn dialog(mode: Mode, dir: &Path, name: Option<&str>) -> (Dialog, oneshot::Receiver<Option<PathBuf>>) {
        let (request, rx) = Request::new(options(mode, Some(dir), name));
        (Dialog::new(request).0, rx)
    }

    /// Updates the dialog; whether it closed.
    fn send(d: &mut Dialog, message: Message) -> bool {
        d.update(message).0
    }

    fn tree() -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir(tmp.path().join("alpha")).unwrap();
        std::fs::create_dir(tmp.path().join("alps")).unwrap();
        std::fs::write(tmp.path().join("apple.txt"), "x").unwrap();
        std::fs::write(tmp.path().join("banana.txt"), "x").unwrap();
        std::fs::write(tmp.path().join(".hidden"), "x").unwrap();
        tmp
    }

    #[test]
    fn env_selects_a_backend() {
        assert_eq!(backend_from_env(Some("Builtin")), Some(Backend::Builtin));
        assert_eq!(backend_from_env(Some("zenity")), Some(Backend::Zenity));
        assert_eq!(backend_from_env(Some("bogus")), None);
        assert_eq!(backend_from_env(None), None);
    }

    #[test]
    fn external_programs_get_title_mode_and_start() {
        let o = options(Mode::Save, Some(Path::new("/srv/x")), Some("a b.txt"));
        let args: Vec<String> = zenity_args(&o).iter().map(|a| a.to_string_lossy().into_owned()).collect();
        assert_eq!(
            args,
            ["--file-selection", "--title=Pick", "--save", "--confirm-overwrite", "--filename=/srv/x/a b.txt"]
        );
        let o = options(Mode::OpenFolder, Some(Path::new("/srv/x/")), None);
        let args: Vec<String> = zenity_args(&o).iter().map(|a| a.to_string_lossy().into_owned()).collect();
        assert_eq!(args, ["--file-selection", "--title=Pick", "--directory", "--filename=/srv/x/"]);
        let args: Vec<String> = kdialog_args(&o).iter().map(|a| a.to_string_lossy().into_owned()).collect();
        assert_eq!(args, ["--title", "Pick", "--getexistingdirectory", "/srv/x/"]);
    }

    #[test]
    fn external_results_tell_cancel_from_failure() {
        assert_eq!(external_result(Some(0), b"/a b/c\n", b""), Ok(Some(PathBuf::from("/a b/c"))));
        assert_eq!(external_result(Some(1), b"", b""), Ok(None));
        assert!(external_result(Some(1), b"", b"Gtk-WARNING: cannot open display: :9").is_err());
        assert!(external_result(Some(255), b"", b"").is_err());
        assert!(external_result(None, b"", b"").is_err());
    }

    #[test]
    fn resolves_typed_paths() {
        let base = Path::new("/base/dir");
        assert_eq!(resolve("x/./y/", base), PathBuf::from("/base/dir/x/y"));
        assert_eq!(resolve("/abs/f.txt", base), PathBuf::from("/abs/f.txt"));
        assert_eq!(resolve("../f", base), PathBuf::from("/base/dir/../f"));
        if let Some(home) = std::env::var_os("HOME") {
            assert_eq!(resolve("~/f", base), PathBuf::from(home).join("f"));
        }
    }

    #[test]
    fn lists_and_completes() {
        let tmp = tree();
        let all = list_dir(tmp.path(), "", false);
        let names: Vec<&str> = all.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["alpha", "alps", "apple.txt", "banana.txt"]);
        let folders = list_dir(tmp.path(), "", true);
        assert_eq!(folders.len(), 2);
        assert_eq!(list_dir(tmp.path(), ".", false).len(), 1);

        let a = list_dir(tmp.path(), "a", false);
        assert_eq!(complete("a", &a), None);
        let al = list_dir(tmp.path(), "al", false);
        assert_eq!(complete("al", &al).as_deref(), Some("alp"));
        let alph = list_dir(tmp.path(), "alph", false);
        assert_eq!(complete("alph", &alph).as_deref(), Some("alpha/"));
        let b = list_dir(tmp.path(), "b", false);
        assert_eq!(complete("b", &b).as_deref(), Some("banana.txt"));
    }

    #[test]
    fn dialog_starts_in_the_folder_with_the_name() {
        let tmp = tree();
        let (d, _rx) = dialog(Mode::Save, tmp.path(), Some("new.txt"));
        assert_eq!(d.input, format!("{}/new.txt", tmp.path().display()));
        let (d, _rx) = dialog(Mode::OpenFile, tmp.path(), None);
        assert_eq!(d.entries.len(), 5);
        assert_eq!(d.entries[0].name, "..");
    }

    #[test]
    fn tab_completes_and_folders_open() {
        let tmp = tree();
        let (mut d, _rx) = dialog(Mode::OpenFile, tmp.path(), None);
        send(&mut d, Message::Input(format!("{}/alph", tmp.path().display())));
        send(&mut d, Message::Complete);
        assert_eq!(d.input, format!("{}/alpha/", tmp.path().display()));
        // Enter on a folder opens it instead of answering.
        send(&mut d, Message::Input(format!("{}/alps", tmp.path().display())));
        let closed = send(&mut d, Message::Accept);
        assert!(!closed);
        assert_eq!(d.input, format!("{}/alps/", tmp.path().display()));
        send(&mut d, Message::Entry("..".into()));
        assert_eq!(d.input, format!("{}/", tmp.path().display()));
    }

    #[test]
    fn open_file_answers_existing_files_only() {
        let tmp = tree();
        let (mut d, mut rx) = dialog(Mode::OpenFile, tmp.path(), None);
        send(&mut d, Message::Input("missing.txt".into()));
        let closed = send(&mut d, Message::Accept);
        assert!(!closed && d.error.is_some());
        send(&mut d, Message::Input(format!("{}/", tmp.path().display())));
        send(&mut d, Message::Entry("banana.txt".into()));
        let closed = send(&mut d, Message::Accept);
        assert!(closed);
        assert_eq!(rx.try_recv().unwrap(), Some(tmp.path().join("banana.txt")));
    }

    #[test]
    fn open_folder_answers_the_typed_folder() {
        let tmp = tree();
        let (mut d, mut rx) = dialog(Mode::OpenFolder, tmp.path(), None);
        assert!(d.entries.iter().all(|e| e.is_dir));
        assert_eq!(d.entries.len(), 3);
        send(&mut d, Message::Entry("alpha".into()));
        let closed = send(&mut d, Message::Accept);
        assert!(closed);
        assert_eq!(rx.try_recv().unwrap(), Some(tmp.path().join("alpha")));
    }

    #[test]
    fn save_confirms_replacing_a_file() {
        let tmp = tree();
        let (mut d, mut rx) = dialog(Mode::Save, tmp.path(), Some("apple.txt"));
        let closed = send(&mut d, Message::Accept);
        assert!(!closed);
        assert_eq!(d.confirm, Some(tmp.path().join("apple.txt")));
        // Editing the path drops the confirmation.
        send(&mut d, Message::Input(format!("{}/apple.tx", tmp.path().display())));
        assert_eq!(d.confirm, None);
        send(&mut d, Message::Input(format!("{}/apple.txt", tmp.path().display())));
        send(&mut d, Message::Accept);
        let closed = send(&mut d, Message::Accept);
        assert!(closed);
        assert_eq!(rx.try_recv().unwrap(), Some(tmp.path().join("apple.txt")));
    }

    #[test]
    fn save_needs_an_existing_folder() {
        let tmp = tree();
        let (mut d, mut rx) = dialog(Mode::Save, tmp.path(), None);
        send(&mut d, Message::Input(format!("{}/nope/new.txt", tmp.path().display())));
        let closed = send(&mut d, Message::Accept);
        assert!(!closed && d.error.is_some());
        send(&mut d, Message::Input("new.txt".into()));
        let closed = send(&mut d, Message::Accept);
        assert!(closed);
        assert_eq!(rx.try_recv().unwrap(), Some(tmp.path().join("new.txt")));
    }

    #[test]
    fn cancel_answers_none_and_dropping_does_too() {
        let tmp = tree();
        let (mut d, mut rx) = dialog(Mode::OpenFile, tmp.path(), None);
        let closed = send(&mut d, Message::Cancel);
        assert!(closed);
        assert_eq!(rx.try_recv().unwrap(), None);
        let (d, mut rx) = dialog(Mode::OpenFile, tmp.path(), None);
        drop(d);
        assert!(rx.try_recv().is_err());
    }
}
