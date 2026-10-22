//! Image comparison (`imagediff.py`, `imagediff.ui`).
//!
//! Two or three images side by side. Beyond Meld's plain side-by-side
//! view, images of the same size are compared pixel by pixel: the
//! summary says whether they're identical and differing pixels can be
//! highlighted. Images can be fitted to their panes or shown at a fixed
//! zoom with the panes scrolling together.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use iced::widget::image::{FilterMethod, Handle};
use iced::widget::{
    column, container, image, mouse_area, row, scrollable, space, stack, text, tooltip,
};
use iced::{Background, ContentFit, Element, Length, Subscription, Task, Theme};

use crate::command::Command;
use crate::shared::Shared;
use crate::ui::{self, BannerKind, menu};
use crate::views::Event;

/// Colour of highlighted differences (premultiplied by nothing; RGBA).
const HIGHLIGHT: [u8; 4] = [255, 0, 80, 170];
const MIN_SCALE: f32 = 0.05;
const MAX_SCALE: f32 = 32.0;

/// A decoded image.
#[derive(Debug)]
pub struct Decoded {
    pub width: u32,
    pub height: u32,
    pub format: String,
    pub file_size: u64,
    pub rgba: Vec<u8>,
    handle: Handle,
}

#[derive(Debug, Clone)]
enum Pane {
    Loading,
    Loaded(Arc<Decoded>),
    Failed(String),
}

/// The result of comparing the images' pixels.
#[derive(Debug)]
pub struct Comparison {
    pub summary: String,
    pub details: Option<String>,
    pub identical: bool,
    /// A highlight layer per pane, where it differs from a neighbour.
    overlays: Vec<Option<Handle>>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Zoom {
    Fit,
    Scale(f32),
}

#[derive(Debug, Clone)]
pub enum Message {
    Loaded(u64, usize, Result<Arc<Decoded>, String>),
    Compared(u64, Arc<Comparison>),
    Focus(usize),
    Zoom(Zoom),
    ZoomIn,
    ZoomOut,
    ToggleDifferences,
    ToggleLock,
    Scrolled(usize, scrollable::Viewport),
    OpenExternal,
    OpenFolder,
    CopyPath,
    Opened(Option<String>),
    Reload,
}

pub struct ImageDiff {
    paths: Vec<PathBuf>,
    panes: Vec<Pane>,
    comparison: Option<Arc<Comparison>>,
    generation: u64,
    zoom: Zoom,
    show_differences: bool,
    lock_scrolling: bool,
    focus: usize,
    scroll_ids: Vec<iced::widget::Id>,
}

impl ImageDiff {
    pub fn new(paths: Vec<PathBuf>, _shared: &mut Shared) -> (Self, Task<Message>) {
        let paths: Vec<PathBuf> = paths.into_iter().take(3).collect();
        let mut tab = ImageDiff {
            scroll_ids: paths.iter().map(|_| iced::widget::Id::unique()).collect(),
            panes: Vec::new(),
            paths,
            comparison: None,
            generation: 0,
            zoom: Zoom::Fit,
            show_differences: true,
            lock_scrolling: true,
            focus: 0,
        };
        let task = tab.load();
        (tab, task)
    }

    fn load(&mut self) -> Task<Message> {
        self.generation += 1;
        self.comparison = None;
        self.panes = self.paths.iter().map(|_| Pane::Loading).collect();
        let generation = self.generation;
        Task::batch(self.paths.iter().cloned().enumerate().map(|(pane, path)| {
            Task::perform(background(move || decode(&path)), move |result| {
                Message::Loaded(
                    generation,
                    pane,
                    result
                        .unwrap_or_else(|| Err(tr!("The image couldn’t be loaded").into()))
                        .map(Arc::new),
                )
            })
        }))
    }

    pub fn title(&self) -> String {
        let names: Vec<String> = self.paths.iter().map(|p| p.display().to_string()).collect();
        meld_files::shorten_names(&names).join(" — ")
    }

    pub fn tooltip(&self) -> String {
        let names: Vec<String> = self.paths.iter().map(|p| p.display().to_string()).collect();
        format!("{}\n{}", tr!("File comparison:"), names.join("\n"))
    }

    pub fn unsaved(&self) -> Vec<String> {
        Vec::new()
    }

    pub fn update(
        &mut self,
        message: Message,
        _shared: &mut Shared,
        events: &mut Vec<Event>,
    ) -> Task<Message> {
        match message {
            Message::Loaded(generation, pane, result) => {
                if generation != self.generation || pane >= self.panes.len() {
                    return Task::none();
                }
                self.panes[pane] = match result {
                    Ok(decoded) => Pane::Loaded(decoded),
                    Err(err) => Pane::Failed(err),
                };
                let loaded: Option<Vec<Arc<Decoded>>> = self
                    .panes
                    .iter()
                    .map(|p| match p {
                        Pane::Loaded(d) => Some(d.clone()),
                        _ => None,
                    })
                    .collect();
                if let Some(images) = loaded {
                    return Task::perform(background(move || compare(&images)), move |result| {
                        Message::Compared(
                            generation,
                            Arc::new(result.unwrap_or(Comparison {
                                summary: tr!("The images couldn’t be compared").into(),
                                details: None,
                                identical: false,
                                overlays: Vec::new(),
                            })),
                        )
                    });
                }
            }
            Message::Compared(generation, comparison) => {
                if generation == self.generation {
                    self.comparison = Some(comparison);
                }
            }
            Message::Focus(pane) => self.focus = pane,
            Message::Zoom(zoom) => self.zoom = zoom,
            Message::ZoomIn | Message::ZoomOut => {
                let current = match self.zoom {
                    Zoom::Fit => 1.0,
                    Zoom::Scale(s) => s,
                };
                let factor = if matches!(message, Message::ZoomIn) {
                    1.25
                } else {
                    0.8
                };
                let next = if self.zoom == Zoom::Fit {
                    current
                } else {
                    current * factor
                };
                self.zoom = Zoom::Scale(next.clamp(MIN_SCALE, MAX_SCALE));
            }
            Message::ToggleDifferences => self.show_differences = !self.show_differences,
            Message::ToggleLock => self.lock_scrolling = !self.lock_scrolling,
            Message::Scrolled(pane, viewport) => {
                if self.lock_scrolling && matches!(self.zoom, Zoom::Scale(_)) {
                    let offset = viewport.absolute_offset();
                    return Task::batch(
                        self.scroll_ids
                            .iter()
                            .enumerate()
                            .filter(|(p, _)| *p != pane)
                            .map(|(_, id)| {
                                iced::widget::operation::scroll_to(
                                    id.clone(),
                                    iced::widget::operation::AbsoluteOffset {
                                        x: Some(offset.x),
                                        y: Some(offset.y),
                                    },
                                )
                            }),
                    );
                }
            }
            Message::OpenExternal | Message::OpenFolder => {
                let Some(path) = self.paths.get(self.focus).cloned() else {
                    return Task::none();
                };
                let parent = matches!(message, Message::OpenFolder);
                return Task::perform(
                    background(move || {
                        let settings = meld_files::ExternalEditorSettings::default();
                        meld_files::open_external_command(&path, 1, &settings, parent)
                            .map_err(|e| e.to_string())
                            .and_then(|argv| {
                                meld_files::spawn_detached(&argv).map_err(|e| e.to_string())
                            })
                            .err()
                    }),
                    |error| Message::Opened(error.flatten()),
                );
            }
            Message::Opened(error) => {
                if let Some(error) = error {
                    events.push(Event::Notify(error));
                }
            }
            Message::CopyPath => {
                if let Some(path) = self.paths.get(self.focus) {
                    return iced::clipboard::write(path.display().to_string());
                }
            }
            Message::Reload => return self.load(),
        }
        Task::none()
    }

    pub fn command(
        &mut self,
        command: Command,
        shared: &mut Shared,
        events: &mut Vec<Event>,
    ) -> Task<Message> {
        let n = self.paths.len().max(1);
        match command {
            Command::OpenExternal => self.update(Message::OpenExternal, shared, events),
            Command::Refresh => self.load(),
            Command::NextPane => {
                self.focus = (self.focus + 1) % n;
                Task::none()
            }
            Command::PreviousPane => {
                self.focus = (self.focus + n - 1) % n;
                Task::none()
            }
            _ => Task::none(),
        }
    }

    pub fn request_close(
        &mut self,
        _shared: &mut Shared,
        events: &mut Vec<Event>,
    ) -> Task<Message> {
        events.push(Event::Close { status: 0 });
        Task::none()
    }

    pub fn settings_changed(&mut self, _shared: &Shared) -> Task<Message> {
        Task::none()
    }

    pub fn subscription(&self) -> Subscription<Message> {
        Subscription::none()
    }

    pub fn toolbar<'a>(&'a self, _shared: &'a Shared) -> Element<'a, Message> {
        let scale_label = match self.zoom {
            Zoom::Fit => tr!("Fit").to_string(),
            Zoom::Scale(s) => format!("{:.0}%", s * 100.0),
        };
        row![
            space::horizontal(),
            text(scale_label).size(13).style(ui::dim_text),
            ui::tool_button("−", tr!("Zoom out"), Some(Message::ZoomOut)),
            ui::tool_button("+", tr!("Zoom in"), Some(Message::ZoomIn)),
            ui::tool_button(
                "1:1",
                tr!("Actual size"),
                Some(Message::Zoom(Zoom::Scale(1.0)))
            ),
            ui::tool_button("⤢", tr!("Zoom to fit"), Some(Message::Zoom(Zoom::Fit))),
            ui::tool_button(
                "◐",
                tr!("Highlight differences"),
                Some(Message::ToggleDifferences)
            ),
            tool_button(
                ui::icon::REFRESH,
                tr!("Reload"),
                "Ctrl+R",
                Some(Message::Reload)
            ),
        ]
        .spacing(2)
        .align_y(iced::Center)
        .into()
    }

    pub fn menu_items(&self, _shared: &Shared) -> Vec<menu::Item<Message>> {
        vec![
            menu::Item::toggle(
                tr!("Zoom to Fit"),
                self.zoom == Zoom::Fit,
                Message::Zoom(Zoom::Fit),
            ),
            menu::Item::action(tr!("Actual Size"), None, Message::Zoom(Zoom::Scale(1.0))),
            menu::Item::action(tr!("Zoom Image In"), None, Message::ZoomIn),
            menu::Item::action(tr!("Zoom Image Out"), None, Message::ZoomOut),
            menu::Item::toggle(
                tr!("Highlight Differences"),
                self.show_differences,
                Message::ToggleDifferences,
            ),
            menu::Item::toggle(
                tr!("Lock Scrolling"),
                self.lock_scrolling,
                Message::ToggleLock,
            ),
            menu::Item::Separator,
            menu::Item::action(
                tr!("Open Externally"),
                Some("Ctrl+Shift+O"),
                Message::OpenExternal,
            ),
            menu::Item::action(tr!("Open Containing Folder"), None, Message::OpenFolder),
            menu::Item::action(tr!("Copy Full Path"), None, Message::CopyPath),
            menu::Item::action(tr!("Reload"), Some("Ctrl+R"), Message::Reload),
        ]
    }

    pub fn view<'a>(&'a self, _shared: &'a Shared) -> Element<'a, Message> {
        let mut content = column![].width(Length::Fill).height(Length::Fill);
        if let Some(comparison) = &self.comparison {
            let kind = if comparison.identical {
                BannerKind::Info
            } else {
                BannerKind::Warning
            };
            content = content.push(ui::banner(
                kind,
                comparison.summary.as_str(),
                comparison.details.clone(),
                Vec::new(),
            ));
        }
        let mut panes = row![].width(Length::Fill).height(Length::Fill);
        for pane in 0..self.paths.len() {
            if pane > 0 {
                panes = panes.push(ui::separator(true));
            }
            panes = panes.push(self.pane_view(pane));
        }
        content.push(panes).into()
    }

    fn pane_view(&self, pane: usize) -> Element<'_, Message> {
        let path = &self.paths[pane];
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string());
        let info = match &self.panes[pane] {
            Pane::Loaded(d) => format!(
                "{} · {} × {} · {}",
                d.format,
                d.width,
                d.height,
                byte_size(d.file_size)
            ),
            Pane::Loading => tr!("Loading…").to_string(),
            Pane::Failed(_) => tr!("Couldn’t load image").to_string(),
        };
        let focused = pane == self.focus;
        let header = tooltip(
            container(
                row![
                    text(name)
                        .size(13)
                        .font(if focused {
                            ui::bold()
                        } else {
                            iced::Font::DEFAULT
                        })
                        .wrapping(text::Wrapping::None),
                    text(info)
                        .size(12)
                        .style(ui::dim_text)
                        .wrapping(text::Wrapping::None),
                ]
                .spacing(10)
                .align_y(iced::Center),
            )
            .padding([6, 10])
            .width(Length::Fill)
            .clip(true)
            .style(ui::header_bar),
            container(text(path.display().to_string()).size(13))
                .padding([4, 8])
                .style(ui::tooltip_box),
            tooltip::Position::Bottom,
        );

        let body: Element<'_, Message> = match &self.panes[pane] {
            Pane::Loading => container(text(tr!("Loading…")).style(ui::dim_text))
                .center(Length::Fill)
                .into(),
            Pane::Failed(err) => container(text(err.as_str()).style(ui::dim_text))
                .padding(20)
                .center(Length::Fill)
                .into(),
            Pane::Loaded(decoded) => {
                let overlay = self
                    .show_differences
                    .then(|| self.comparison.as_ref()?.overlays.get(pane)?.clone())
                    .flatten();
                match self.zoom {
                    Zoom::Fit => {
                        let mut layers = stack![
                            image(decoded.handle.clone())
                                .content_fit(ContentFit::ScaleDown)
                                .width(Length::Fill)
                                .height(Length::Fill)
                        ];
                        if let Some(overlay) = overlay {
                            layers = layers.push(
                                image(overlay)
                                    .content_fit(ContentFit::ScaleDown)
                                    .filter_method(FilterMethod::Nearest)
                                    .width(Length::Fill)
                                    .height(Length::Fill),
                            );
                        }
                        container(layers)
                            .padding(8)
                            .width(Length::Fill)
                            .height(Length::Fill)
                            .into()
                    }
                    Zoom::Scale(scale) => {
                        let w = decoded.width as f32 * scale;
                        let h = decoded.height as f32 * scale;
                        let filter = if scale >= 2.0 {
                            FilterMethod::Nearest
                        } else {
                            FilterMethod::Linear
                        };
                        let mut layers = stack![
                            image(decoded.handle.clone())
                                .content_fit(ContentFit::Fill)
                                .filter_method(filter)
                                .width(w)
                                .height(h)
                        ];
                        if let Some(overlay) = overlay {
                            layers = layers.push(
                                image(overlay)
                                    .content_fit(ContentFit::Fill)
                                    .filter_method(FilterMethod::Nearest)
                                    .width(w)
                                    .height(h),
                            );
                        }
                        scrollable(container(layers).padding(8))
                            .id(self.scroll_ids[pane].clone())
                            .direction(scrollable::Direction::Both {
                                vertical: scrollable::Scrollbar::default(),
                                horizontal: scrollable::Scrollbar::default(),
                            })
                            .on_scroll(move |viewport| Message::Scrolled(pane, viewport))
                            .width(Length::Fill)
                            .height(Length::Fill)
                            .into()
                    }
                }
            }
        };
        let body = container(body)
            .width(Length::Fill)
            .height(Length::Fill)
            .style(|theme: &Theme| container::Style {
                background: Some(Background::Color(
                    theme.extended_palette().background.weak.color,
                )),
                ..container::Style::default()
            });
        mouse_area(column![header, ui::separator(false), body].width(Length::FillPortion(1)))
            .on_press(Message::Focus(pane))
            .into()
    }
}

fn background<T: Send + 'static>(
    f: impl FnOnce() -> T + Send + 'static,
) -> impl Future<Output = Option<T>> {
    let (tx, rx) = iced::futures::channel::oneshot::channel();
    std::thread::spawn(move || {
        let _ = tx.send(f());
    });
    async move { rx.await.ok() }
}

fn decode(path: &Path) -> Result<Decoded, String> {
    let file_size = std::fs::metadata(path).map_err(|e| e.to_string())?.len();
    let reader = ::image::ImageReader::open(path)
        .map_err(|e| e.to_string())?
        .with_guessed_format()
        .map_err(|e| e.to_string())?;
    let format = reader
        .format()
        .map(|f| {
            f.extensions_str()
                .first()
                .copied()
                .unwrap_or("image")
                .to_ascii_uppercase()
        })
        .unwrap_or_else(|| tr!("Unknown").into());
    let rgba = reader.decode().map_err(|e| e.to_string())?.to_rgba8();
    let (width, height) = rgba.dimensions();
    let rgba = rgba.into_raw();
    let handle = Handle::from_rgba(width, height, rgba.clone());
    Ok(Decoded {
        width,
        height,
        format,
        file_size,
        rgba,
        handle,
    })
}

/// Which pixels differ between two same-sized RGBA buffers.
fn difference_mask(a: &[u8], b: &[u8]) -> Vec<bool> {
    a.chunks_exact(4)
        .zip(b.chunks_exact(4))
        .map(|(p, q)| p != q)
        .collect()
}

/// Compares neighbouring images (left–middle, middle–right).
fn compare(images: &[Arc<Decoded>]) -> Comparison {
    let n = images.len();
    if n < 2 {
        return Comparison {
            summary: tr!("Nothing to compare").into(),
            details: None,
            identical: true,
            overlays: vec![None; n],
        };
    }
    let mut masks: Vec<Option<Vec<bool>>> = vec![None; n];
    let mut parts = Vec::new();
    let mut identical = true;
    let mut size_mismatch = false;
    for i in 0..n - 1 {
        let (a, b) = (&images[i], &images[i + 1]);
        if (a.width, a.height) != (b.width, b.height) {
            size_mismatch = true;
            identical = false;
            parts.push(tr!(
                "Panes {} and {} have different sizes ({} × {} and {} × {})",
                i + 1,
                i + 2,
                a.width,
                a.height,
                b.width,
                b.height
            ));
            continue;
        }
        let mask = difference_mask(&a.rgba, &b.rgba);
        let differing = mask.iter().filter(|d| **d).count();
        if differing > 0 {
            identical = false;
            let total = mask.len().max(1);
            let percent = format!("{:.2}", differing as f64 * 100.0 / total as f64);
            parts.push(if n == 2 {
                tr!(
                    "{differing} of {total} pixels differ ({percent}%)",
                    differing = differing,
                    total = total,
                    percent = percent
                )
            } else {
                tr!(
                    "Panes {first} and {second}: {differing} of {total} pixels differ ({percent}%)",
                    first = i + 1,
                    second = i + 2,
                    differing = differing,
                    total = total,
                    percent = percent
                )
            });
            for pane in [i, i + 1] {
                let merged = match masks[pane].take() {
                    Some(existing) => existing.iter().zip(&mask).map(|(x, y)| *x || *y).collect(),
                    None => mask.clone(),
                };
                masks[pane] = Some(merged);
            }
        }
    }
    let overlays = masks
        .into_iter()
        .zip(images)
        .map(|(mask, image)| {
            let mask = mask?;
            let mut pixels = vec![0u8; mask.len() * 4];
            for (pixel, differs) in pixels.chunks_exact_mut(4).zip(&mask) {
                if *differs {
                    pixel.copy_from_slice(&HIGHLIGHT);
                }
            }
            Some(Handle::from_rgba(image.width, image.height, pixels))
        })
        .collect();
    let summary = if identical {
        tr!("Images are identical").to_string()
    } else if size_mismatch {
        tr!("Image sizes differ").to_string()
    } else {
        tr!("Images differ").to_string()
    };
    Comparison {
        summary,
        details: (!parts.is_empty()).then(|| parts.join("\n")),
        identical,
        overlays,
    }
}

/// A toolbar button like [`ui::tool_button`], whose tooltip is the
/// translated `tip` followed by its shortcut.
fn tool_button<'a>(
    glyph: &'a str,
    tip: &str,
    shortcut: &str,
    on_press: Option<Message>,
) -> Element<'a, Message> {
    ui::tool_button(glyph, ui::with_shortcut(tip, shortcut), on_press)
}

fn byte_size(size: u64) -> String {
    const SUFFIXES: [&str; 5] = ["B", "kB", "MB", "GB", "TB"];
    let mut value = size as f64;
    let mut unit = 0;
    while value > 1000.0 && unit < SUFFIXES.len() - 1 {
        value /= 1000.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{size} B")
    } else {
        format!("{value:.1} {}", SUFFIXES[unit])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decoded(width: u32, height: u32, pixels: Vec<u8>) -> Arc<Decoded> {
        Arc::new(Decoded {
            width,
            height,
            format: "PNG".into(),
            file_size: 0,
            handle: Handle::from_rgba(width, height, pixels.clone()),
            rgba: pixels,
        })
    }

    #[test]
    fn identical_images() {
        let a = decoded(2, 1, vec![1, 2, 3, 255, 4, 5, 6, 255]);
        let b = decoded(2, 1, vec![1, 2, 3, 255, 4, 5, 6, 255]);
        let result = compare(&[a, b]);
        assert!(result.identical);
        assert_eq!(result.summary, "Images are identical");
        assert!(result.overlays.iter().all(Option::is_none));
    }

    #[test]
    fn differing_pixels_are_counted_and_highlighted() {
        let a = decoded(2, 1, vec![1, 2, 3, 255, 4, 5, 6, 255]);
        let b = decoded(2, 1, vec![1, 2, 3, 255, 4, 5, 7, 255]);
        let result = compare(&[a, b]);
        assert!(!result.identical);
        assert_eq!(
            result.details.as_deref(),
            Some("1 of 2 pixels differ (50.00%)")
        );
        assert!(result.overlays.iter().all(Option::is_some));
        assert_eq!(
            difference_mask(&[0, 0, 0, 0, 1, 1, 1, 1], &[0, 0, 0, 0, 1, 1, 1, 2]),
            [false, true]
        );
    }

    #[test]
    fn size_mismatch_and_three_panes() {
        let a = decoded(1, 1, vec![0; 4]);
        let b = decoded(2, 1, vec![0; 8]);
        let c = decoded(2, 1, vec![0, 0, 0, 0, 9, 9, 9, 9]);
        let result = compare(&[a, b, c]);
        assert_eq!(result.summary, "Image sizes differ");
        let details = result.details.unwrap();
        assert!(details.contains("Panes 1 and 2 have different sizes (1 × 1 and 2 × 1)"));
        assert!(details.contains("Panes 2 and 3: 1 of 2 pixels differ"));
        assert!(result.overlays[0].is_none());
        assert!(result.overlays[1].is_some() && result.overlays[2].is_some());
    }

    #[test]
    fn decodes_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("x.png");
        ::image::RgbaImage::from_pixel(3, 2, ::image::Rgba([10, 20, 30, 255]))
            .save(&path)
            .unwrap();
        let d = decode(&path).unwrap();
        assert_eq!((d.width, d.height), (3, 2));
        assert_eq!(d.format, "PNG");
        assert_eq!(d.rgba.len(), 24);
        assert!(decode(&dir.path().join("missing.png")).is_err());
    }
}
