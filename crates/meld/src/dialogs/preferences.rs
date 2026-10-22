//! The preferences dialog (`preferences.py`, `resources/ui/preferences.ui`),
//! with the editable filter list (`ui/listwidget.py`, `filter-list.ui`)
//! and folder column list (`ui/columnlist.py`, `column-list.ui`).
//!
//! Every control edits `Shared::settings` directly, like the Python
//! dialog's GSettings bindings; the app saves the settings and tells the
//! tabs after each change.
//!
//! Filter patterns are checked the way `meld/filters.py` compiles them:
//! text filters as Python regular expressions and filename filters as
//! shell wildcards translated by `shell_to_regex`. The checker follows
//! the syntax rules of Python's `re` module (see [`check_regex`]).

use std::collections::HashMap;

use iced::widget::{
    Id, button, checkbox, column, container, mouse_area, pick_list, row, scrollable, space, text,
    text_input, toggler, tooltip,
};
use iced::{Background, Border, Element, Length, Task, Theme};
use meld_config::settings::DIFF_COLOR_OPACITY_RANGE;
use meld_config::{
    DiffColorMode, DiffSide, FilterSetting, FolderColumn, FontSpec, MergeFileOrder,
    OverviewMapStyle, Settings, StyleAttr, StyleVariant, TIMESTAMP_RESOLUTIONS, WrapMode,
};

use crate::shared::Shared;
use crate::ui::{self, icon};

/// Families offered by the font picker; any other installed family can be
/// typed in.
const MONOSPACE_FAMILIES: &[&str] = &[
    "Monospace",
    "Adwaita Mono",
    "Cascadia Code",
    "Consolas",
    "DejaVu Sans Mono",
    "Fira Code",
    "Fira Mono",
    "Hack",
    "IBM Plex Mono",
    "Inconsolata",
    "Iosevka",
    "JetBrains Mono",
    "Liberation Mono",
    "Menlo",
    "Noto Sans Mono",
    "Roboto Mono",
    "Source Code Pro",
    "Ubuntu Mono",
];

/// The labels of `TIMESTAMP_RESOLUTIONS`, marked for translation.
#[allow(dead_code)]
const TIMESTAMP_LABELS: [&str; 5] = [
    n_!("1ns (ext4)"),
    n_!("100ns (NTFS)"),
    n_!("1s (ext2/ext3)"),
    n_!("2s (VFAT)"),
    n_!("Ignore timestamp"),
];

/// Font sizes the size control accepts, in points.
const FONT_SIZE_RANGE: (f32, f32) = (4.0, 96.0);

/// `ColumnList.available_columns`: folder comparison columns and labels.
const AVAILABLE_COLUMNS: [(&str, &str); 4] = [
    ("size", n_!("Size")),
    ("modification time", n_!("Modification time")),
    ("iso-time", n_!("Modification time (ISO)")),
    ("permissions", n_!("Permissions")),
];

const DIALOG_WIDTH: f32 = 860.0;
const DIALOG_HEIGHT: f32 = 600.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Page {
    Editor,
    Folders,
    VersionControl,
    GitCompare,
    Filters,
}

impl Page {
    const ALL: [Page; 5] = [
        Page::Editor,
        Page::Folders,
        Page::VersionControl,
        Page::GitCompare,
        Page::Filters,
    ];

    fn title(self) -> &'static str {
        match self {
            Page::Editor => tr!("Editor"),
            Page::Folders => tr!("Folder Comparisons"),
            Page::VersionControl => tr!("Version Control"),
            Page::GitCompare => tr!("Git Compare"),
            Page::Filters => tr!("Filters"),
        }
    }
}

/// Boolean settings shown as switches.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BoolKey {
    /// `use-system-font`, inverted.
    CustomFont,
    HighlightSyntax,
    HighlightCurrentLine,
    ShowLineNumbers,
    SpaceDrawer,
    ShowOverviewMap,
    /// `use-system-editor`, inverted.
    CustomEditor,
    ShallowComparison,
    IgnoreSymlinks,
    FilterText,
    ShowCommitMargin,
    BreakCommitMessage,
    IgnoreBlankLines,
    InferEnabled,
}

impl BoolKey {
    fn get(self, s: &Settings) -> bool {
        match self {
            BoolKey::CustomFont => !s.use_system_font,
            BoolKey::HighlightSyntax => s.highlight_syntax,
            BoolKey::HighlightCurrentLine => s.highlight_current_line,
            BoolKey::ShowLineNumbers => s.show_line_numbers,
            BoolKey::SpaceDrawer => s.enable_space_drawer,
            BoolKey::ShowOverviewMap => s.show_overview_map,
            BoolKey::CustomEditor => !s.use_system_editor,
            BoolKey::ShallowComparison => s.folder_shallow_comparison,
            BoolKey::IgnoreSymlinks => s.folder_ignore_symlinks,
            BoolKey::FilterText => s.folder_filter_text,
            BoolKey::ShowCommitMargin => s.vc_show_commit_margin,
            BoolKey::BreakCommitMessage => s.vc_break_commit_message,
            BoolKey::IgnoreBlankLines => s.ignore_blank_lines,
            BoolKey::InferEnabled => s.ailens_infer_enabled,
        }
    }

    fn set(self, s: &mut Settings, value: bool) {
        let field = match self {
            BoolKey::CustomFont => {
                s.use_system_font = !value;
                return;
            }
            BoolKey::CustomEditor => {
                s.use_system_editor = !value;
                return;
            }
            BoolKey::HighlightSyntax => &mut s.highlight_syntax,
            BoolKey::HighlightCurrentLine => &mut s.highlight_current_line,
            BoolKey::ShowLineNumbers => &mut s.show_line_numbers,
            BoolKey::SpaceDrawer => &mut s.enable_space_drawer,
            BoolKey::ShowOverviewMap => &mut s.show_overview_map,
            BoolKey::ShallowComparison => &mut s.folder_shallow_comparison,
            BoolKey::IgnoreSymlinks => &mut s.folder_ignore_symlinks,
            BoolKey::FilterText => &mut s.folder_filter_text,
            BoolKey::ShowCommitMargin => &mut s.vc_show_commit_margin,
            BoolKey::BreakCommitMessage => &mut s.vc_break_commit_message,
            BoolKey::IgnoreBlankLines => &mut s.ignore_blank_lines,
            BoolKey::InferEnabled => &mut s.ailens_infer_enabled,
        };
        *field = value;
    }
}

/// String settings shown as entries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TextKey {
    CustomEditorCommand,
    DefaultFolder,
    ClaudePath,
    InferModel,
    BuildModel,
    BuildFolder,
    NotesRef,
    AuthorPattern,
}

impl TextKey {
    fn field(self, s: &mut Settings) -> &mut String {
        match self {
            TextKey::CustomEditorCommand => &mut s.custom_editor_command,
            TextKey::DefaultFolder => &mut s.default_folder,
            TextKey::ClaudePath => &mut s.ailens_claude_path,
            TextKey::InferModel => &mut s.ailens_infer_model,
            TextKey::BuildModel => &mut s.ailens_build_model,
            TextKey::BuildFolder => &mut s.ailens_build_folder,
            TextKey::NotesRef => &mut s.ailens_notes_ref,
            TextKey::AuthorPattern => &mut s.ailens_author_pattern,
        }
    }

    fn get(self, s: &Settings) -> &str {
        match self {
            TextKey::CustomEditorCommand => &s.custom_editor_command,
            TextKey::DefaultFolder => &s.default_folder,
            TextKey::ClaudePath => &s.ailens_claude_path,
            TextKey::InferModel => &s.ailens_infer_model,
            TextKey::BuildModel => &s.ailens_build_model,
            TextKey::BuildFolder => &s.ailens_build_folder,
            TextKey::NotesRef => &s.ailens_notes_ref,
            TextKey::AuthorPattern => &s.ailens_author_pattern,
        }
    }
}

/// Integer settings shown as spin buttons.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SpinKey {
    IndentWidth,
    CommitMargin,
    FileRowSpacing,
    FileListWidth,
}

impl SpinKey {
    /// `(lower, upper, step)` of the Python dialog's adjustment.
    fn range(self) -> (i32, i32, i32) {
        match self {
            SpinKey::IndentWidth => (1, 8, 1),
            SpinKey::CommitMargin => (70, 120, 1),
            SpinKey::FileRowSpacing => {
                let (lo, hi) = meld_config::settings::AILENS_FILE_ROW_SPACING_RANGE;
                (lo, hi, 1)
            }
            SpinKey::FileListWidth => {
                let (lo, hi) = meld_config::settings::AILENS_FILE_LIST_WIDTH_RANGE;
                (lo, hi, 10)
            }
        }
    }

    fn field(self, s: &mut Settings) -> &mut i32 {
        match self {
            SpinKey::IndentWidth => &mut s.indent_width,
            SpinKey::CommitMargin => &mut s.vc_commit_margin,
            SpinKey::FileRowSpacing => &mut s.ailens_file_row_spacing,
            SpinKey::FileListWidth => &mut s.ailens_file_list_width,
        }
    }

    fn get(self, s: &Settings) -> i32 {
        match self {
            SpinKey::IndentWidth => s.indent_width,
            SpinKey::CommitMargin => s.vc_commit_margin,
            SpinKey::FileRowSpacing => s.ailens_file_row_spacing,
            SpinKey::FileListWidth => s.ailens_file_list_width,
        }
    }
}

/// A choice from one of the drop-downs.
#[derive(Debug, Clone)]
pub enum Choice {
    StyleVariant(StyleVariant),
    DiffColors(DiffColorMode),
    WrapMode(WrapMode),
    InsertSpaces(bool),
    StyleScheme(String),
    OverviewMapStyle(OverviewMapStyle),
    TimeResolution(i32),
    LeftIsLocal(bool),
    MergeFileOrder(MergeFileOrder),
    FontFamily(String),
}

/// Which filter list a [`FilterAction`] applies to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilterKind {
    /// `filename-filters`: shell wildcards (`FilterEntry.SHELL`).
    Filename,
    /// `text-filters`: regular expressions (`FilterEntry.REGEX`).
    Text,
}

impl FilterKind {
    fn index(self) -> usize {
        match self {
            FilterKind::Filename => 0,
            FilterKind::Text => 1,
        }
    }

    fn filters(self, s: &Settings) -> &[FilterSetting] {
        match self {
            FilterKind::Filename => &s.filename_filters,
            FilterKind::Text => &s.text_filters,
        }
    }

    fn filters_mut(self, s: &mut Settings) -> &mut Vec<FilterSetting> {
        match self {
            FilterKind::Filename => &mut s.filename_filters,
            FilterKind::Text => &mut s.text_filters,
        }
    }

    fn name_input_id(self) -> Id {
        match self {
            FilterKind::Filename => Id::new("prefs-filename-filter-name"),
            FilterKind::Text => Id::new("prefs-text-filter-name"),
        }
    }
}

#[derive(Debug, Clone)]
pub enum FilterAction {
    Select(usize),
    SetActive(usize, bool),
    SetName(usize, String),
    SetPattern(usize, String),
    Add,
    Remove,
    MoveUp,
    MoveDown,
}

#[derive(Debug, Clone)]
pub enum Message {
    Close,
    Page(Page),
    Bool(BoolKey, bool),
    Text(TextKey, String),
    /// Choose the default folder with a folder chooser.
    BrowseDefaultFolder,
    DefaultFolderChosen(Option<std::path::PathBuf>),
    SpinStep(SpinKey, i32),
    SpinInput(SpinKey, String),
    FontSizeStep(i32),
    FontSizeInput(String),
    FontFamilyInput(String),
    /// `diff-color-opacity`, in percent.
    DiffColorOpacity(i32),
    Choose(Choice),
    Filter(FilterKind, FilterAction),
    ColumnVisible(usize, bool),
    ColumnMove(usize, isize),
}

/// What a preferences update did.
pub struct Outcome {
    pub task: Task<Message>,
    /// Settings changed: save them and tell the tabs.
    pub changed: bool,
    pub close: bool,
}

impl Outcome {
    fn changed(changed: bool) -> Self {
        Outcome {
            task: Task::none(),
            changed,
            close: false,
        }
    }
}

/// A scheme in the colour scheme drop-down.
#[derive(Debug, Clone, PartialEq)]
struct SchemeChoice {
    id: String,
    name: String,
    description: String,
}

impl std::fmt::Display for SchemeChoice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.name)
    }
}

/// A drop-down entry: a value and its label, compared by value.
#[derive(Debug, Clone)]
struct Item<T> {
    value: T,
    label: String,
}

impl<T: PartialEq> PartialEq for Item<T> {
    fn eq(&self, other: &Self) -> bool {
        self.value == other.value
    }
}

impl<T> std::fmt::Display for Item<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.label)
    }
}

pub struct Preferences {
    page: Page,
    schemes: Vec<SchemeChoice>,
    /// The selected row of the filename and text filter lists.
    selected_filter: [Option<usize>; 2],
    /// Spin button text being typed, until it is a valid value.
    spin_drafts: HashMap<SpinKey, String>,
    font_size_draft: Option<String>,
    /// The font family being typed; the font string normalises spaces,
    /// so the entry shows this rather than the parsed family.
    font_family_draft: Option<String>,
}

impl Preferences {
    pub fn new(shared: &Shared) -> Self {
        let schemes = shared
            .schemes
            .list()
            .into_iter()
            .map(|info| SchemeChoice {
                id: info.id,
                name: info.name,
                description: info.description,
            })
            .collect();
        Preferences {
            page: Page::Editor,
            schemes,
            selected_filter: [None, None],
            spin_drafts: HashMap::new(),
            font_size_draft: None,
            font_family_draft: None,
        }
    }

    pub fn update(&mut self, message: Message, shared: &mut Shared) -> Outcome {
        let settings = &mut shared.settings;
        match message {
            Message::Close => Outcome {
                task: Task::none(),
                changed: false,
                close: true,
            },
            Message::Page(page) => {
                self.page = page;
                Outcome::changed(false)
            }
            Message::Bool(key, value) => {
                key.set(settings, value);
                Outcome::changed(true)
            }
            Message::Text(key, value) => {
                *key.field(settings) = value;
                Outcome::changed(true)
            }
            Message::DefaultFolderChosen(path) => match path {
                Some(path) => {
                    settings.default_folder = path.display().to_string();
                    Outcome::changed(true)
                }
                None => Outcome::changed(false),
            },
            Message::BrowseDefaultFolder => {
                let start = crate::ui::chooser::default_folder();
                Outcome {
                    task: crate::ui::chooser::pick_folder(tr!("Default Folder"), start)
                        .map(Message::DefaultFolderChosen),
                    changed: false,
                    close: false,
                }
            }
            Message::SpinStep(key, direction) => {
                self.spin_drafts.remove(&key);
                let (lo, hi, step) = key.range();
                let field = key.field(settings);
                let value = (*field + direction * step).clamp(lo, hi);
                let changed = value != *field;
                *field = value;
                Outcome::changed(changed)
            }
            Message::SpinInput(key, input) => {
                let (lo, hi, _) = key.range();
                let parsed = input
                    .trim()
                    .parse::<i32>()
                    .ok()
                    .filter(|v| (lo..=hi).contains(v));
                self.spin_drafts.insert(key, input);
                match parsed {
                    Some(value) => {
                        *key.field(settings) = value;
                        Outcome::changed(true)
                    }
                    None => Outcome::changed(false),
                }
            }
            Message::FontSizeStep(direction) => {
                self.font_size_draft = None;
                let (lo, hi) = FONT_SIZE_RANGE;
                edit_font(settings, |spec| {
                    spec.size_pt = (spec.size_pt + direction as f32).clamp(lo, hi);
                });
                Outcome::changed(true)
            }
            Message::FontSizeInput(input) => {
                let (lo, hi) = FONT_SIZE_RANGE;
                let parsed = input
                    .trim()
                    .parse::<f32>()
                    .ok()
                    .filter(|v| (lo..=hi).contains(v));
                self.font_size_draft = Some(input);
                match parsed {
                    Some(size) => {
                        edit_font(settings, |spec| spec.size_pt = size);
                        Outcome::changed(true)
                    }
                    None => Outcome::changed(false),
                }
            }
            Message::FontFamilyInput(input) => {
                let family = input.trim().to_string();
                self.font_family_draft = Some(input);
                edit_font(settings, |spec| spec.family = family);
                Outcome::changed(true)
            }
            Message::DiffColorOpacity(percent) => {
                let (lo, hi) = DIFF_COLOR_OPACITY_RANGE;
                let percent = percent.clamp(lo, hi);
                let changed = percent != settings.diff_color_opacity;
                settings.diff_color_opacity = percent;
                Outcome::changed(changed)
            }
            Message::Choose(choice) => {
                match choice {
                    Choice::StyleVariant(v) => settings.style_variant = v,
                    Choice::DiffColors(v) => settings.diff_colors = v,
                    Choice::WrapMode(v) => settings.wrap_mode = v,
                    Choice::InsertSpaces(v) => settings.insert_spaces_instead_of_tabs = v,
                    Choice::StyleScheme(id) => settings.style_scheme = id,
                    Choice::OverviewMapStyle(v) => settings.overview_map_style = v,
                    Choice::TimeResolution(v) => settings.folder_time_resolution = v,
                    Choice::LeftIsLocal(v) => settings.vc_left_is_local = v,
                    Choice::MergeFileOrder(v) => settings.vc_merge_file_order = v,
                    Choice::FontFamily(family) => {
                        self.font_family_draft = None;
                        edit_font(settings, |spec| spec.family = family);
                    }
                }
                Outcome::changed(true)
            }
            Message::Filter(kind, action) => self.update_filter(kind, action, settings),
            Message::ColumnVisible(index, visible) => {
                let mut columns = ordered_columns(&settings.folder_columns);
                let Some(column) = columns.get_mut(index) else {
                    return Outcome::changed(false);
                };
                column.visible = visible;
                settings.folder_columns = columns;
                Outcome::changed(true)
            }
            Message::ColumnMove(index, offset) => {
                let mut columns = ordered_columns(&settings.folder_columns);
                let target = index.saturating_add_signed(offset);
                let moved = move_item(&mut columns, index, target);
                settings.folder_columns = columns;
                Outcome::changed(moved)
            }
        }
    }

    fn update_filter(
        &mut self,
        kind: FilterKind,
        action: FilterAction,
        settings: &mut Settings,
    ) -> Outcome {
        let filters = kind.filters_mut(settings);
        let selected = &mut self.selected_filter[kind.index()];
        match action {
            FilterAction::Select(index) => {
                if *selected == Some(index) {
                    return Outcome::changed(false);
                }
                *selected = (index < filters.len()).then_some(index);
                Outcome::changed(false)
            }
            FilterAction::SetActive(index, active) => match filters.get_mut(index) {
                Some(filter) => {
                    filter.active = active;
                    Outcome::changed(true)
                }
                None => Outcome::changed(false),
            },
            FilterAction::SetName(index, name) => match filters.get_mut(index) {
                Some(filter) => {
                    filter.name = name;
                    Outcome::changed(true)
                }
                None => Outcome::changed(false),
            },
            FilterAction::SetPattern(index, pattern) => match filters.get_mut(index) {
                Some(filter) => {
                    filter.pattern = pattern;
                    Outcome::changed(true)
                }
                None => Outcome::changed(false),
            },
            FilterAction::Add => {
                // `FilterList.default_entry`
                filters.push(FilterSetting::new(tr!("label"), false, tr!("pattern")));
                *selected = Some(filters.len() - 1);
                let id = kind.name_input_id();
                Outcome {
                    task: Task::batch([
                        iced::widget::operation::focus(id.clone()),
                        iced::widget::operation::select_all(id),
                    ]),
                    changed: true,
                    close: false,
                }
            }
            FilterAction::Remove => {
                let Some(index) = selected.take().filter(|i| *i < filters.len()) else {
                    return Outcome::changed(false);
                };
                filters.remove(index);
                Outcome::changed(true)
            }
            FilterAction::MoveUp | FilterAction::MoveDown => {
                let Some(index) = *selected else {
                    return Outcome::changed(false);
                };
                let target = match action {
                    FilterAction::MoveUp => index.checked_sub(1),
                    _ => Some(index + 1).filter(|t| *t < filters.len()),
                };
                if let Some(target) = target
                    && move_item(filters, index, target)
                {
                    *selected = Some(target);
                    Outcome::changed(true)
                } else {
                    Outcome::changed(false)
                }
            }
        }
    }

    pub fn view<'a>(&'a self, shared: &'a Shared) -> Element<'a, Message> {
        let mut pages = column![].spacing(2).padding([12, 8]);
        for page in Page::ALL {
            let selected = page == self.page;
            pages = pages.push(
                button(text(page.title()).size(14))
                    .width(Length::Fill)
                    .padding([7, 12])
                    .style(sidebar_button(selected))
                    .on_press(Message::Page(page)),
            );
        }
        let sidebar = container(column![
            container(text(tr!("Preferences")).size(16).font(ui::bold())).padding([14, 20]),
            pages
        ])
        .width(200)
        .height(Length::Fill)
        .style(ui::header_bar);

        let header = row![
            text(self.page.title()).size(18).font(ui::bold()),
            space::horizontal(),
            ui::tool_button(icon::CLOSE, tr!("Close"), Some(Message::Close)),
        ]
        .align_y(iced::Center)
        .padding([8, 12]);

        let body = match self.page {
            Page::Editor => self.editor_page(shared),
            Page::Folders => self.folders_page(&shared.settings),
            Page::VersionControl => self.vc_page(&shared.settings),
            Page::GitCompare => self.git_compare_page(&shared.settings),
            Page::Filters => self.filters_page(&shared.settings),
        };
        let content = column![
            container(header).padding([0, 12]),
            scrollable(container(body).padding(iced::Padding {
                top: 4.0,
                right: 32.0,
                bottom: 24.0,
                left: 24.0,
            }))
            .height(Length::Fill),
        ]
        .width(Length::Fill);

        container(row![sidebar, ui::separator(true), content])
            .width(Length::Fill)
            .height(Length::Fill)
            .max_width(DIALOG_WIDTH)
            .max_height(DIALOG_HEIGHT)
            .clip(true)
            .style(ui::popover)
            .into()
    }

    fn editor_page<'a>(&'a self, shared: &'a Shared) -> Element<'a, Message> {
        let s = &shared.settings;

        let style_variant = choice_row(
            tr!("Interface Style"),
            None,
            StyleVariant::ALL
                .map(|v| (v, crate::i18n::gettext(v.label())))
                .to_vec(),
            &s.style_variant,
            Choice::StyleVariant,
        );

        let font = FontSpec::parse(&s.custom_font);
        let family_value = self
            .font_family_draft
            .clone()
            .unwrap_or_else(|| font.family.clone());
        let known_family = MONOSPACE_FAMILIES
            .iter()
            .find(|f| f.eq_ignore_ascii_case(font.family.trim()))
            .map(|f| f.to_string());
        let family_pick = pick_list(
            MONOSPACE_FAMILIES
                .iter()
                .map(|f| f.to_string())
                .collect::<Vec<_>>(),
            known_family,
            |family| Message::Choose(Choice::FontFamily(family)),
        )
        .placeholder(tr!("Other"))
        .text_size(14)
        .padding([4, 8])
        .width(190);
        let size_value = self
            .font_size_draft
            .clone()
            .unwrap_or_else(|| format_points(font.size_pt));
        let custom_font = expander(
            tr!("Use Custom Font"),
            Some(tr!(
                "Choose a font different to the system default monospace font"
            )),
            BoolKey::CustomFont.get(s),
            |v| Message::Bool(BoolKey::CustomFont, v),
            vec![
                setting_row(
                    tr!("Font"),
                    Some(tr!("The font used within the editor")),
                    row![
                        family_pick,
                        spin_box(
                            size_value,
                            Message::FontSizeStep(-1),
                            Message::FontSizeStep(1),
                            Message::FontSizeInput,
                            true
                        ),
                    ]
                    .spacing(8)
                    .align_y(iced::Center),
                    true,
                ),
                setting_row(
                    tr!("Font Family"),
                    Some(tr!("Any installed font, by name")),
                    text_input(tr!("Font family"), &family_value)
                        .on_input(Message::FontFamilyInput)
                        .size(14)
                        .padding([5, 8])
                        .width(260),
                    true,
                ),
                container(
                    text("The quick brown fox jumps over the lazy dog. 0123456789 {}[]()")
                        .font(shared.mono_font())
                        .size(shared.mono_size())
                        .wrapping(text::Wrapping::None),
                )
                .padding([10, 14])
                .width(Length::Fill)
                .clip(true)
                .into(),
            ],
        );

        let current_scheme = self.schemes.iter().find(|c| c.id == s.style_scheme);
        let scheme_pick = pick_list(
            self.schemes.as_slice(),
            current_scheme,
            |c: SchemeChoice| Message::Choose(Choice::StyleScheme(c.id)),
        )
        .placeholder(s.style_scheme.as_str())
        .text_size(14)
        .padding([4, 8])
        .width(240);
        let syntax = expander(
            tr!("Use syntax highlighting"),
            None,
            s.highlight_syntax,
            |v| Message::Bool(BoolKey::HighlightSyntax, v),
            vec![setting_row(
                tr!("Color Scheme"),
                current_scheme
                    .map(|c| c.description.as_str())
                    .filter(|d| !d.is_empty()),
                scheme_pick,
                true,
            )],
        );

        let diff_colors = diff_colors_section(shared);

        let wrap = choice_row(
            tr!("Text wrapping"),
            Some(tr!("How text should be wrapped when wider than the pane")),
            WrapMode::PREFERENCES
                .map(|v| (v, crate::i18n::gettext(v.label())))
                .to_vec(),
            &s.wrap_mode,
            Choice::WrapMode,
        );

        let overview = expander(
            tr!("Show overview map"),
            None,
            s.show_overview_map,
            |v| Message::Bool(BoolKey::ShowOverviewMap, v),
            vec![choice_row(
                tr!("Overview map style"),
                Some(tr!("How the overview map is displayed")),
                vec![
                    (OverviewMapStyle::Chunkmap, tr!("Simple change map")),
                    (
                        OverviewMapStyle::CompactSourcemap,
                        tr!("Compact source map"),
                    ),
                    (OverviewMapStyle::FullSourcemap, tr!("Full source map")),
                ],
                &s.overview_map_style,
                Choice::OverviewMapStyle,
            )],
        );

        let editor = expander(
            tr!("Use custom text editor"),
            Some(tr!(
                "Choose an external text editor different to the system default"
            )),
            BoolKey::CustomEditor.get(s),
            |v| Message::Bool(BoolKey::CustomEditor, v),
            vec![entry_row(
                tr!("Custom Editor Command"),
                tr!(
                    "The command used to launch a custom editor. Some limited templating is \
                     supported here; at the moment '{{file}}' and '{{line}}' are recognised tokens."
                ),
                TextKey::CustomEditorCommand,
                s,
                None,
            )],
        );

        let default_folder = row![
            container(entry_row(
                tr!("Default folder"),
                tr!(
                    "Where file and folder choosers open when there is no better place to start. \
                     Empty means your home folder."
                ),
                TextKey::DefaultFolder,
                s,
                (!s.default_folder.trim().is_empty()
                    && !std::path::Path::new(&expand_home(s.default_folder.trim())).is_dir())
                .then(
                    || tr!("This folder doesn't exist; your home folder is used instead.")
                        .to_string()
                ),
            ))
            .width(Length::Fill),
            // Level with the entry, which sits above the row's bottom padding.
            container(
                button(text(tr!("Browse…")).size(13))
                    .padding([5, 10])
                    .style(button::secondary)
                    .on_press(Message::BrowseDefaultFolder),
            )
            .padding(iced::Padding::ZERO.bottom(8)),
        ]
        .padding(iced::Padding::ZERO.right(14))
        .align_y(iced::Bottom);

        column![
            group(Some(tr!("Files")), None, vec![default_folder.into()]),
            group(
                Some(tr!("Appearance")),
                None,
                vec![style_variant, custom_font, syntax, diff_colors]
            ),
            group(None, None, vec![wrap]),
            group(
                None,
                None,
                vec![
                    switch_row(
                        tr!("Highlight current line"),
                        Some(tr!("Make the current line stand out with highlights")),
                        BoolKey::HighlightCurrentLine,
                        s,
                        true,
                    ),
                    switch_row(
                        tr!("Show line numbers"),
                        Some(tr!("Display line numbers next to each line of text")),
                        BoolKey::ShowLineNumbers,
                        s,
                        true,
                    ),
                ],
            ),
            group(
                None,
                None,
                vec![switch_row(
                    tr!("Show whitespace"),
                    None,
                    BoolKey::SpaceDrawer,
                    s,
                    true
                )]
            ),
            group(
                Some(tr!("Indentation")),
                None,
                vec![
                    choice_row(
                        tr!("Tab character"),
                        Some(tr!("The character to be inserted for Tab")),
                        vec![(false, tr!("Tab")), (true, tr!("Spaces"))],
                        &s.insert_spaces_instead_of_tabs,
                        Choice::InsertSpaces,
                    ),
                    self.spin_row(
                        tr!("Indentation Size"),
                        Some(tr!("The number of characters to indent")),
                        SpinKey::IndentWidth,
                        s,
                        true,
                    ),
                ],
            ),
            group(Some(tr!("Code Overview")), None, vec![overview]),
            group(Some(tr!("External Editor")), None, vec![editor]),
        ]
        .spacing(24)
        .into()
    }

    fn folders_page<'a>(&'a self, s: &'a Settings) -> Element<'a, Message> {
        let resolutions: Vec<(i32, &str)> = TIMESTAMP_RESOLUTIONS
            .iter()
            .map(|(v, label)| (*v, crate::i18n::gettext(label)))
            .collect();
        let shallow = expander(
            tr!("Compare files based only on size and timestamp"),
            None,
            s.folder_shallow_comparison,
            |v| Message::Bool(BoolKey::ShallowComparison, v),
            vec![choice_row(
                tr!("Timestamp resolution"),
                None,
                resolutions,
                &s.folder_time_resolution,
                Choice::TimeResolution,
            )],
        );

        let mut columns: Vec<Element<'a, Message>> = Vec::new();
        for (index, column) in ordered_columns(&s.folder_columns).into_iter().enumerate() {
            let last = index + 1 == AVAILABLE_COLUMNS.len();
            let label = column_label(&column.name).unwrap_or_default();
            let controls = row![
                ui::tool_button(
                    icon::UP,
                    tr!("Move Up"),
                    (index > 0).then_some(Message::ColumnMove(index, -1))
                ),
                ui::tool_button(
                    icon::DOWN,
                    tr!("Move Down"),
                    (!last).then_some(Message::ColumnMove(index, 1))
                ),
                toggler(column.visible)
                    .on_toggle(move |v| Message::ColumnVisible(index, v))
                    .size(20),
            ]
            .spacing(4)
            .align_y(iced::Center);
            columns.push(setting_row(label, None, controls, true));
        }

        column![
            group(Some(tr!("Comparison Options")), None, vec![shallow]),
            group(
                None,
                None,
                vec![switch_row(
                    tr!("Ignore symbolic links"),
                    None,
                    BoolKey::IgnoreSymlinks,
                    s,
                    true
                )],
            ),
            group(
                None,
                None,
                vec![switch_row(
                    tr!("Apply text filters during folder comparisons"),
                    Some(tr!(
                        "Enabling text filters will make comparing large files much slower"
                    )),
                    BoolKey::FilterText,
                    s,
                    !s.folder_shallow_comparison,
                )],
            ),
            group(Some(tr!("Visible Columns")), None, columns),
        ]
        .spacing(24)
        .into()
    }

    fn vc_page<'a>(&'a self, s: &'a Settings) -> Element<'a, Message> {
        let margin = s.vc_show_commit_margin;
        column![
            group(
                Some(tr!("Version Comparisons")),
                None,
                vec![
                    choice_row(
                        tr!("Order when Comparing File Revisions"),
                        None,
                        vec![
                            (false, tr!("Left is remote, right is local")),
                            (true, tr!("Left is local, right is remote")),
                        ],
                        &s.vc_left_is_local,
                        Choice::LeftIsLocal,
                    ),
                    choice_row(
                        tr!("Order when Merging Files"),
                        None,
                        MergeFileOrder::ALL
                            .map(|v| (v, crate::i18n::gettext(v.label())))
                            .to_vec(),
                        &s.vc_merge_file_order,
                        Choice::MergeFileOrder,
                    ),
                ],
            ),
            group(
                Some(tr!("Commit Messages")),
                None,
                vec![
                    switch_row(
                        tr!("Show right margin"),
                        None,
                        BoolKey::ShowCommitMargin,
                        s,
                        true
                    ),
                    self.spin_row(
                        tr!("Margin position"),
                        None,
                        SpinKey::CommitMargin,
                        s,
                        margin
                    ),
                    switch_row(
                        tr!("Automatically break lines at right margin on commit"),
                        None,
                        BoolKey::BreakCommitMessage,
                        s,
                        margin,
                    ),
                ],
            ),
        ]
        .spacing(24)
        .into()
    }

    fn git_compare_page<'a>(&'a self, s: &'a Settings) -> Element<'a, Message> {
        let author_error = (!s.ailens_author_pattern.is_empty())
            .then(|| check_regex(&s.ailens_author_pattern).err())
            .flatten();
        column![
            group(
                Some(tr!("File List")),
                None,
                vec![
                    self.spin_row(
                        tr!("Row Spacing"),
                        Some(tr!("Extra space above and below each file, in pixels")),
                        SpinKey::FileRowSpacing,
                        s,
                        true,
                    ),
                    self.spin_row(
                        tr!("Width"),
                        Some(tr!("In pixels; dragging the divider changes it too")),
                        SpinKey::FileListWidth,
                        s,
                        true,
                    ),
                ],
            ),
            group(
                Some(tr!("Claude Code")),
                Some(
                    tr!("Explain and Explain & Build run “claude -p” with no tools. Changes apply \
                         to comparisons opened or reloaded afterwards.")
                ),
                vec![
                    switch_row(
                        tr!("Explain with Claude"),
                        Some(tr!("Offer to explain changes whose reason isn’t recorded")),
                        BoolKey::InferEnabled,
                        s,
                        true,
                    ),
                    entry_row(
                        tr!("Claude Code Path"),
                        tr!("The claude executable that explains changes and writes build summaries."),
                        TextKey::ClaudePath,
                        s,
                        None,
                    ),
                    entry_row(
                        tr!("Model for Explanations"),
                        tr!("The model passed to claude -p when inferring why a commit or change was made."),
                        TextKey::InferModel,
                        s,
                        None,
                    ),
                    entry_row(
                        tr!("Model for Build Summaries"),
                        tr!("The model passed to claude -p for a build file's title, summary, decisions and risks."),
                        TextKey::BuildModel,
                        s,
                        None,
                    ),
                ],
            ),
            group(
                Some(tr!("Repository")),
                None,
                vec![
                    entry_row(
                        tr!("Build Folder"),
                        tr!("Where build files are written, relative to the repository root."),
                        TextKey::BuildFolder,
                        s,
                        None,
                    ),
                    entry_row(
                        tr!("Notes Ref"),
                        tr!("The notes ref that explanations are read from and saved to."),
                        TextKey::NotesRef,
                        s,
                        None,
                    ),
                    entry_row(
                        tr!("Agent Author Pattern (regular expression)"),
                        tr!("A regular expression matched against commit authors to recognise agent \
                             commits, besides the Co-Authored-By: Claude trailer."),
                        TextKey::AuthorPattern,
                        s,
                        author_error,
                    ),
                ],
            ),
        ]
        .spacing(24)
        .into()
    }

    fn filters_page<'a>(&'a self, s: &'a Settings) -> Element<'a, Message> {
        column![
            group(
                Some(tr!("Filename filters")),
                Some(tr!(
                    "When performing directory comparisons, you may filter out files and \
                         directories by name. Each pattern is a list of shell style wildcards \
                         separated by spaces."
                )),
                vec![self.filter_list(FilterKind::Filename, s)],
            ),
            group(
                Some(tr!("Change trimming")),
                None,
                vec![switch_row(
                    tr!("Trim blank line differences from the start and end of changes"),
                    Some(tr!(
                        "Removing blank lines can simplify comparisons, and can combine with \
                             text filters to fully remove changes."
                    )),
                    BoolKey::IgnoreBlankLines,
                    s,
                    true,
                )],
            ),
            group(
                Some(tr!("Text filters")),
                Some(tr!(
                    "When performing file comparisons, you may ignore certain types of changes. \
                         Each pattern here is a python regular expression which replaces matching \
                         text with the empty string before comparison is performed. If the \
                         expression contains groups, only the groups are replaced. See the user \
                         manual for more details."
                )),
                vec![self.filter_list(FilterKind::Text, s)],
            ),
        ]
        .spacing(24)
        .into()
    }

    /// The editable filter list (`FilterList`): one row per filter with an
    /// Active check, the name, the pattern and a warning if the pattern is
    /// invalid. The selected row's name and pattern are editable.
    fn filter_list<'a>(&'a self, kind: FilterKind, s: &'a Settings) -> Element<'a, Message> {
        let filters = kind.filters(s);
        let selected = self.selected_filter[kind.index()].filter(|i| *i < filters.len());
        let msg = move |action| Message::Filter(kind, action);

        let heading = |label: &'static str| text(label).size(12).style(ui::dim_text);
        let mut list = column![
            row![
                container(heading(tr!("Active"))).width(56),
                container(heading(tr!("Name"))).width(190),
                heading(tr!("Pattern")).width(Length::Fill),
            ]
            .spacing(8)
            .padding([6, 14]),
        ];

        for (index, filter) in filters.iter().enumerate() {
            let error = filter_error(&filter.pattern, kind);
            let is_selected = selected == Some(index);
            let active = container(
                checkbox(filter.active).on_toggle(move |v| msg(FilterAction::SetActive(index, v))),
            )
            .width(56)
            .center_x(56);
            let (name, pattern): (Element<'a, Message>, Element<'a, Message>) = if is_selected {
                (
                    text_input(tr!("Name"), &filter.name)
                        .id(kind.name_input_id())
                        .on_input(move |v| msg(FilterAction::SetName(index, v)))
                        .size(14)
                        .padding([4, 6])
                        .width(190)
                        .into(),
                    text_input(tr!("Pattern"), &filter.pattern)
                        .on_input(move |v| msg(FilterAction::SetPattern(index, v)))
                        .font(iced::Font::MONOSPACE)
                        .size(13)
                        .padding([5, 6])
                        .style(invalid_input(error.is_some()))
                        .width(Length::Fill)
                        .into(),
                )
            } else {
                (
                    text(filter.name.as_str())
                        .size(14)
                        .width(190)
                        .wrapping(text::Wrapping::None)
                        .into(),
                    text(filter.pattern.as_str())
                        .font(iced::Font::MONOSPACE)
                        .size(13)
                        .width(Length::Fill)
                        .wrapping(text::Wrapping::None)
                        .into(),
                )
            };
            let warning: Element<'a, Message> = match error {
                Some(error) => tooltip(
                    text("⚠").size(15).style(text::danger),
                    container(text(error).size(13))
                        .padding([4, 8])
                        .style(ui::tooltip_box),
                    tooltip::Position::Left,
                )
                .into(),
                None => space::horizontal().width(0).into(),
            };
            let line = row![
                active,
                name,
                container(pattern).width(Length::Fill).clip(true),
                container(warning).width(20)
            ]
            .spacing(8)
            .align_y(iced::Center)
            .height(34);
            let line = container(line)
                .padding([2, 14])
                .width(Length::Fill)
                .style(move |theme: &Theme| list_row_style(theme, is_selected));
            list = list.push(ui::separator(false));
            list = list.push(mouse_area(line).on_press(msg(FilterAction::Select(index))));
        }

        let toolbar = row![
            toolbar_button(
                icon::ADD,
                tr!("Add"),
                tr!("Add new filter"),
                Some(msg(FilterAction::Add))
            ),
            toolbar_button(
                "−",
                tr!("Remove"),
                tr!("Remove selected filter"),
                selected.map(|_| msg(FilterAction::Remove))
            ),
            toolbar_button(
                icon::UP,
                tr!("Move Up"),
                tr!("Move item up"),
                selected
                    .filter(|i| *i > 0)
                    .map(|_| msg(FilterAction::MoveUp)),
            ),
            toolbar_button(
                icon::DOWN,
                tr!("Move Down"),
                tr!("Move item down"),
                selected
                    .filter(|i| i + 1 < filters.len())
                    .map(|_| msg(FilterAction::MoveDown)),
            ),
        ]
        .spacing(4)
        .padding([4, 8]);

        column![list, ui::separator(false), toolbar].into()
    }

    fn spin_row<'a>(
        &self,
        title: &'a str,
        subtitle: Option<&'a str>,
        key: SpinKey,
        s: &Settings,
        enabled: bool,
    ) -> Element<'a, Message> {
        let value = self
            .spin_drafts
            .get(&key)
            .cloned()
            .unwrap_or_else(|| key.get(s).to_string());
        setting_row(
            title,
            subtitle,
            spin_box(
                value,
                Message::SpinStep(key, -1),
                Message::SpinStep(key, 1),
                move |v| Message::SpinInput(key, v),
                enabled,
            ),
            enabled,
        )
    }
}

/// Rewrites `custom-font` after changing its parsed description.
fn edit_font(settings: &mut Settings, edit: impl FnOnce(&mut FontSpec)) {
    let mut spec = FontSpec::parse(&settings.custom_font);
    edit(&mut spec);
    settings.custom_font = font_string(&spec);
}

/// A Pango font description string for `spec`, such as "Fira Code 11".
///
/// A family whose last word would be read as a style or size (e.g.
/// "Iosevka 2") is followed by a comma, as Pango does.
pub fn font_string(spec: &FontSpec) -> String {
    let family = spec.family.split_whitespace().collect::<Vec<_>>().join(" ");
    let family = if family.is_empty() {
        "Monospace".to_string()
    } else {
        family
    };
    let mut styles = String::new();
    if spec.bold {
        styles.push_str(" Bold");
    }
    if spec.italic {
        styles.push_str(" Italic");
    }
    let size = format_points(spec.size_pt);
    let plain = format!("{family}{styles} {size}");
    if FontSpec::parse(&plain).family == family {
        plain
    } else {
        format!("{family},{styles} {size}")
    }
}

/// A point size without a needless fraction: 11 or 10.5.
fn format_points(size: f32) -> String {
    let rounded = (size * 100.0).round() / 100.0;
    format!("{rounded}")
}

/// `ColumnList`'s rows: every available column, in the saved order (with
/// unsaved columns last), visible as saved and hidden if unsaved.
pub fn ordered_columns(saved: &[FolderColumn]) -> Vec<FolderColumn> {
    let mut columns: Vec<(usize, FolderColumn)> = AVAILABLE_COLUMNS
        .iter()
        .map(|(name, _)| {
            let position = saved.iter().position(|c| c.name == *name);
            let visible = position.is_some_and(|p| saved[p].visible);
            (
                position.unwrap_or(AVAILABLE_COLUMNS.len()),
                FolderColumn::new(name, visible),
            )
        })
        .collect();
    columns.sort_by_key(|(order, _)| *order);
    columns.into_iter().map(|(_, c)| c).collect()
}

fn column_label(name: &str) -> Option<&'static str> {
    AVAILABLE_COLUMNS
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, label)| crate::i18n::gettext(label))
}

/// Moves `items[from]` to `to`, clamped to the list. Returns whether
/// anything moved.
fn move_item<T>(items: &mut Vec<T>, from: usize, to: usize) -> bool {
    if from >= items.len() || items.is_empty() {
        return false;
    }
    let to = to.min(items.len() - 1);
    if to == from {
        return false;
    }
    let item = items.remove(from);
    items.insert(to, item);
    true
}

// ---------------------------------------------------------------------
// Building blocks, after libadwaita's preferences rows.

/// A preferences group: an optional title and description over a boxed
/// list of rows.
fn group<'a>(
    title: Option<&'a str>,
    description: Option<&'a str>,
    rows: Vec<Element<'a, Message>>,
) -> Element<'a, Message> {
    let mut col = column![].spacing(6);
    if let Some(title) = title {
        col = col.push(text(title).size(15).font(ui::bold()));
    }
    if let Some(description) = description {
        col = col.push(text(description).size(13).style(ui::dim_text));
    }
    let mut list = column![];
    for (i, row) in rows.into_iter().enumerate() {
        if i > 0 {
            list = list.push(ui::separator(false));
        }
        list = list.push(row);
    }
    col.push(container(list).width(Length::Fill).clip(true).style(card))
        .into()
}

/// A row with a title, an optional subtitle and a control on the right.
fn setting_row<'a>(
    title: &'a str,
    subtitle: Option<&'a str>,
    control: impl Into<Element<'a, Message>>,
    enabled: bool,
) -> Element<'a, Message> {
    let title_style = move |theme: &Theme| {
        let color = theme.extended_palette().background.base.text;
        text::Style {
            color: Some(if enabled {
                color
            } else {
                color.scale_alpha(0.45)
            }),
        }
    };
    let mut label = column![text(title).size(14).style(title_style)].spacing(2);
    if let Some(subtitle) = subtitle {
        label = label.push(text(subtitle).size(12).style(ui::dim_text));
    }
    row![label.width(Length::Fill), control.into()]
        .spacing(16)
        .align_y(iced::Center)
        .padding([10, 14])
        .into()
}

fn switch_row<'a>(
    title: &'a str,
    subtitle: Option<&'a str>,
    key: BoolKey,
    s: &Settings,
    enabled: bool,
) -> Element<'a, Message> {
    let switch = toggler(key.get(s))
        .on_toggle_maybe(enabled.then_some(move |v| Message::Bool(key, v)))
        .size(20);
    setting_row(title, subtitle, switch, enabled)
}

/// The difference colour palettes, each with a preview of how its
/// chunks look, and the opacity slider. The previews use the current
/// scheme and opacity, so moving the slider updates them along with any
/// open comparison.
fn diff_colors_section(shared: &Shared) -> Element<'_, Message> {
    let s = &shared.settings;
    let header = setting_row(
        tr!("Difference Colors"),
        Some(tr!("How removed, added and changed lines are highlighted")),
        diff_preview_legend(),
        true,
    );
    let mut col = column![header];
    for mode in DiffColorMode::ALL {
        let selected = mode == s.diff_colors;
        // A radio button's circle
        let dot = container(space::horizontal())
            .width(6)
            .height(6)
            .style(move |theme: &Theme| container::Style {
                background: selected
                    .then(|| Background::Color(theme.extended_palette().primary.base.color)),
                border: Border {
                    radius: 3.0.into(),
                    ..Border::default()
                },
                ..container::Style::default()
            });
        let mark = container(dot).center(14).style(move |theme: &Theme| {
            let palette = theme.extended_palette();
            container::Style {
                border: Border {
                    color: if selected {
                        palette.primary.base.color
                    } else {
                        palette.background.base.text.scale_alpha(0.5)
                    },
                    width: 1.5,
                    radius: 7.0.into(),
                },
                ..container::Style::default()
            }
        });
        let label = column![
            text(crate::i18n::gettext(mode.label())).size(14),
            text(mode.description()).size(12).style(ui::dim_text),
        ]
        .spacing(2)
        .width(Length::Fill);
        let content = row![mark, label, diff_preview(shared, mode)]
            .spacing(10)
            .align_y(iced::Center);
        col = col.push(
            button(content)
                .width(Length::Fill)
                .padding([6, 14])
                .style(move |theme: &Theme, status| {
                    let mut style = ui::flat_button(theme, status);
                    if selected {
                        style.background = Some(Background::Color(
                            theme
                                .extended_palette()
                                .primary
                                .weak
                                .color
                                .scale_alpha(0.25),
                        ));
                    }
                    style.border = Border::default();
                    style
                })
                .on_press(Message::Choose(Choice::DiffColors(mode))),
        );
    }
    let (lo, hi) = DIFF_COLOR_OPACITY_RANGE;
    let opacity = s.diff_color_opacity.clamp(lo, hi);
    col = col.push(ui::separator(false));
    col = col.push(setting_row(
        tr!("Difference Color Opacity"),
        Some(tr!(
            "Fainter colors keep the text easier to read; outlines stay visible"
        )),
        row![
            iced::widget::slider(lo..=hi, opacity, Message::DiffColorOpacity)
                .step(5)
                .width(180),
            text(format!("{opacity}%")).size(13).width(40),
        ]
        .spacing(10)
        .align_y(iced::Center),
        true,
    ));
    col.into()
}

/// Column captions for [`diff_preview`].
fn diff_preview_legend<'a>() -> Element<'a, Message> {
    let caption = |label: &'a str| {
        text(label)
            .size(11)
            .width(PREVIEW_PANE_WIDTH)
            .align_x(iced::Center)
            .style(ui::dim_text)
    };
    row![caption(tr!("Left pane")), caption(tr!("Right pane"))]
        .spacing(PREVIEW_GAP)
        .into()
}

const PREVIEW_CHIP: (f32, f32) = (24.0, 14.0);
const PREVIEW_GAP: f32 = 6.0;
const PREVIEW_PANE_WIDTH: f32 = 3.0 * PREVIEW_CHIP.0 + 2.0 * 2.0 + 2.0 * 3.0;

/// How a palette colours lines removed from the left file, changed lines
/// and lines added in the right file, in the left and the right pane of
/// a two-way comparison, drawn on the scheme's text background.
fn diff_preview(shared: &Shared, mode: DiffColorMode) -> Element<'_, Message> {
    let theme = &shared.style;
    let opacity = shared.settings.diff_color_opacity;
    let background = shared
        .scheme_color("text", StyleAttr::Background)
        .unwrap_or(if theme.is_dark() {
            iced::Color::from_rgb8(0x1e, 0x1e, 0x1e)
        } else {
            iced::Color::WHITE
        });
    let chip = |c: meld_config::ChunkColors| {
        let (fill, line) = (crate::shared::color(c.fill), crate::shared::color(c.line));
        container(space::horizontal())
            .width(PREVIEW_CHIP.0)
            .height(PREVIEW_CHIP.1)
            .style(move |_: &Theme| container::Style {
                background: Some(Background::Color(fill)),
                border: Border {
                    color: line,
                    width: 1.0,
                    radius: 2.0.into(),
                },
                ..container::Style::default()
            })
    };
    // In each pane, lines only that pane has are a delete and lines only
    // the other pane has an insert; replaced lines are a replace.
    let pane = |side: DiffSide| {
        let colors = theme.diff_colors(mode, Some(side)).with_opacity(opacity);
        let (removed, added) = match side {
            DiffSide::Old => (colors.delete, colors.insert),
            DiffSide::New => (colors.insert, colors.delete),
        };
        container(row![chip(removed), chip(colors.replace), chip(added)].spacing(2))
            .padding(3)
            .style(move |_: &Theme| container::Style {
                background: Some(Background::Color(background)),
                border: Border {
                    radius: 3.0.into(),
                    ..Border::default()
                },
                ..container::Style::default()
            })
    };
    tooltip(
        row![pane(DiffSide::Old), pane(DiffSide::New)].spacing(PREVIEW_GAP),
        container(text(tr!("Removed, changed and added lines")).size(12))
            .padding([4, 8])
            .style(ui::tooltip_box),
        tooltip::Position::Top,
    )
    .into()
}

/// A drop-down row (`PreferenceComboRow`).
fn choice_row<'a, T>(
    title: &'a str,
    subtitle: Option<&'a str>,
    options: Vec<(T, &'static str)>,
    current: &T,
    on_choose: impl Fn(T) -> Choice + 'a,
) -> Element<'a, Message>
where
    T: Clone + PartialEq + 'a,
{
    let items: Vec<Item<T>> = options
        .into_iter()
        .map(|(value, label)| Item {
            value,
            label: label.to_string(),
        })
        .collect();
    let selected = items.iter().find(|i| i.value == *current).cloned();
    let pick = pick_list(items, selected, move |item: Item<T>| {
        Message::Choose(on_choose(item.value))
    })
    .text_size(14)
    .padding([4, 8])
    .width(260);
    setting_row(title, subtitle, pick, true)
}

/// A switch row whose children show while it is on (`AdwExpanderRow` with
/// an enable switch).
fn expander<'a>(
    title: &'a str,
    subtitle: Option<&'a str>,
    enabled: bool,
    on_toggle: impl Fn(bool) -> Message + 'a,
    children: Vec<Element<'a, Message>>,
) -> Element<'a, Message> {
    let header = setting_row(
        title,
        subtitle,
        toggler(enabled).on_toggle(on_toggle).size(20),
        true,
    );
    let mut col = column![header];
    if enabled {
        for child in children {
            col = col.push(ui::separator(false));
            col = col.push(
                container(child)
                    .padding(iced::Padding {
                        left: 20.0,
                        ..iced::Padding::ZERO
                    })
                    .width(Length::Fill)
                    .style(nested_row),
            );
        }
    }
    col.into()
}

/// An entry row (`AdwEntryRow`): the title over a text field. `tip` is
/// the setting's description; `error` marks the value invalid.
fn entry_row<'a>(
    title: &'a str,
    tip: &'a str,
    key: TextKey,
    s: &'a Settings,
    error: Option<String>,
) -> Element<'a, Message> {
    let label = tooltip(
        text(title).size(12).style(ui::dim_text),
        container(text(tip).size(13))
            .padding([4, 8])
            .max_width(360)
            .style(ui::tooltip_box),
        tooltip::Position::Top,
    );
    let input = text_input("", key.get(s))
        .on_input(move |v| Message::Text(key, v))
        .size(14)
        .padding([5, 8])
        .style(invalid_input(error.is_some()));
    let mut col = column![label, input].spacing(4);
    if let Some(error) = error {
        col = col.push(text(format!("⚠ {error}")).size(12).style(text::danger));
    }
    container(col).padding([8, 14]).width(Length::Fill).into()
}

/// A spin button: − and + around a number field.
fn spin_box<'a>(
    value: String,
    down: Message,
    up: Message,
    on_input: impl Fn(String) -> Message + 'a,
    enabled: bool,
) -> Element<'a, Message> {
    let step = |label: &'static str, message: Message| {
        button(text(label).size(15).center())
            .width(28)
            .padding([2, 0])
            .style(button::secondary)
            .on_press_maybe(enabled.then_some(message))
    };
    let mut input = text_input("", &value)
        .size(14)
        .padding([4, 6])
        .width(64)
        .align_x(iced::Center);
    if enabled {
        input = input.on_input(on_input);
    }
    row![step("−", down), input, step("+", up)]
        .spacing(4)
        .align_y(iced::Center)
        .into()
}

/// A small icon-and-label button for the filter list toolbar.
fn toolbar_button<'a>(
    glyph: &'a str,
    label: &'a str,
    tip: &'a str,
    on_press: Option<Message>,
) -> Element<'a, Message> {
    let enabled = on_press.is_some();
    let content = row![text(glyph).size(15), text(label).size(13)]
        .spacing(6)
        .align_y(iced::Center);
    let b = button(content)
        .padding([4, 10])
        .style(ui::flat_button)
        .on_press_maybe(on_press);
    let b: Element<'a, Message> = if enabled {
        tooltip(
            b,
            container(text(tip).size(13))
                .padding([4, 8])
                .style(ui::tooltip_box),
            tooltip::Position::Bottom,
        )
        .into()
    } else {
        b.into()
    };
    b
}

fn card(theme: &Theme) -> container::Style {
    let palette = theme.extended_palette();
    container::Style {
        background: Some(Background::Color(palette.background.weakest.color)),
        border: Border::default()
            .rounded(8)
            .width(1)
            .color(palette.background.strong.color),
        ..container::Style::default()
    }
}

fn nested_row(theme: &Theme) -> container::Style {
    let palette = theme.extended_palette();
    container::Style {
        background: Some(Background::Color(
            palette.background.weak.color.scale_alpha(0.5),
        )),
        ..container::Style::default()
    }
}

fn list_row_style(theme: &Theme, selected: bool) -> container::Style {
    let palette = theme.extended_palette();
    container::Style {
        background: selected
            .then(|| Background::Color(palette.primary.weak.color.scale_alpha(0.35))),
        ..container::Style::default()
    }
}

fn sidebar_button(selected: bool) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |theme, status| {
        let palette = theme.extended_palette();
        let mut style = ui::flat_button(theme, status);
        if selected {
            style.background = Some(Background::Color(palette.background.strong.color));
            style.text_color = palette.background.strong.text;
        }
        style
    }
}

fn invalid_input(invalid: bool) -> impl Fn(&Theme, text_input::Status) -> text_input::Style {
    move |theme, status| {
        let mut style = text_input::default(theme, status);
        if invalid {
            style.border = style
                .border
                .color(theme.extended_palette().danger.base.color);
        }
        style
    }
}

// ---------------------------------------------------------------------
// Filter validation (`meld/filters.py`).

/// Why `pattern` can't be used as a filter of `kind`, or `None` if it is
/// valid (`FilterEntry.check_filter`).
pub fn filter_error(pattern: &str, kind: FilterKind) -> Option<String> {
    match kind {
        FilterKind::Text => check_regex(pattern).err(),
        FilterKind::Filename => compile_shell_pattern(pattern)
            .and_then(|regex| check_regex(&regex))
            .err(),
    }
}

/// `FilterEntry.compile_shell_pattern`: space-separated wildcards to one
/// regular expression. An empty pattern would match everything, so it is
/// refused.
fn compile_shell_pattern(pattern: &str) -> Result<String, String> {
    let bits: Vec<&str> = pattern.split_whitespace().collect();
    match bits.as_slice() {
        [] => Err("empty pattern".into()),
        [one] => shell_to_regex(one),
        many => {
            let parts = many
                .iter()
                .map(|bit| shell_to_regex(bit).map(|r| r[..r.len() - 1].to_string()))
                .collect::<Result<Vec<_>, _>>()?;
            Ok(format!("({})$", parts.join("|")))
        }
    }
}

/// `shell_to_regex`: translate a shell pattern to a Python regular
/// expression, like `fnmatch.translate()` plus `{a,b,c}` alternatives.
pub fn shell_to_regex(pattern: &str) -> Result<String, String> {
    let chars: Vec<char> = pattern.chars().collect();
    let n = chars.len();
    let mut i = 0;
    let mut res = String::new();
    let find = |c: char, from: usize| chars[from..].iter().position(|x| *x == c).map(|p| p + from);
    while i < n {
        let c = chars[i];
        i += 1;
        match c {
            '\\' => {
                // A trailing backslash is dropped.
                if i < n {
                    res.push_str(&re_escape(chars[i]));
                    i += 1;
                }
            }
            '*' => res.push_str(".*"),
            '?' => res.push('.'),
            '[' => match find(']', i) {
                None => res.push_str("\\["),
                Some(j) => {
                    let stuff: String = chars[i..j].iter().collect();
                    i = j + 1;
                    // Python fails on `stuff[0]` here.
                    let stuff = if let Some(rest) = stuff.strip_prefix('!') {
                        format!("^{rest}")
                    } else if let Some(rest) = stuff.strip_prefix('^') {
                        format!("\\^{rest}")
                    } else if stuff.is_empty() {
                        return Err("empty character set in wildcard".into());
                    } else {
                        stuff
                    };
                    res.push_str(&format!("[{stuff}]"));
                }
            },
            '{' => match find('}', i) {
                None => res.push_str("\\{"),
                Some(j) => {
                    let stuff: String = chars[i..j].iter().collect();
                    i = j + 1;
                    let alternatives = stuff
                        .split(',')
                        .map(|p| shell_to_regex(p).map(|r| r[..r.len() - 1].to_string()))
                        .collect::<Result<Vec<_>, _>>()?;
                    res.push_str(&format!("({})", alternatives.join("|")));
                }
            },
            c => res.push_str(&re_escape(c)),
        }
    }
    res.push('$');
    Ok(res)
}

/// Python's `re.escape` for one character.
fn re_escape(c: char) -> String {
    if "()[]{}?*+-|^$\\.&~# \t\n\r\x0b\x0c".contains(c) {
        format!("\\{c}")
    } else {
        c.to_string()
    }
}

/// Checks a regular expression against the syntax of Python's `re`
/// module, which Meld's text filters use, returning Python's error
/// message.
///
/// Covers groups and extensions, inline flags, character sets and
/// ranges, escapes, repeats and group references. Not checked: that
/// look-behinds have a fixed width, and character names in `\N{...}`.
pub fn check_regex(pattern: &str) -> Result<(), String> {
    RegexChecker {
        chars: pattern.chars().collect(),
        pos: 0,
        groups: 0,
        open: Vec::new(),
        closed: vec![true],
        names: HashMap::new(),
        verbose: false,
        seen_item: false,
    }
    .run()
}

/// The last item parsed, for repeat checks.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Last {
    Nothing,
    Anchor,
    Atom,
    Repeat,
}

enum Escape {
    Literal(u32),
    Category,
    Anchor,
}

struct OpenGroup {
    pos: usize,
    number: Option<usize>,
    conditional: bool,
    branches: usize,
}

struct RegexChecker {
    chars: Vec<char>,
    pos: usize,
    groups: usize,
    open: Vec<OpenGroup>,
    /// Whether each group number has been closed; group 0 always is.
    closed: Vec<bool>,
    names: HashMap<String, usize>,
    verbose: bool,
    /// Whether anything but global flags has been parsed, which makes
    /// global flags invalid.
    seen_item: bool,
}

const MAXREPEAT: u64 = 4_294_967_295;
const FLAG_CHARS: &str = "aiLmsux";

impl RegexChecker {
    fn error(&self, message: impl std::fmt::Display, pos: usize) -> String {
        format!("{message} at position {pos}")
    }

    fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }

    fn next(&mut self) -> Option<char> {
        let c = self.peek()?;
        self.pos += 1;
        Some(c)
    }

    fn eat(&mut self, c: char) -> bool {
        if self.peek() == Some(c) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn run(mut self) -> Result<(), String> {
        let mut last = Last::Nothing;
        while let Some(c) = self.next() {
            let start = self.pos - 1;
            if self.verbose && " \t\n\r\x0b\x0c".contains(c) {
                continue;
            }
            if self.verbose && c == '#' {
                while let Some(c) = self.next() {
                    if c == '\n' {
                        break;
                    }
                }
                continue;
            }
            if c != '(' {
                self.seen_item = true;
            }
            last = match c {
                '|' => {
                    if let Some(group) = self.open.last_mut() {
                        group.branches += 1;
                        if group.conditional && group.branches > 2 {
                            return Err(self
                                .error("conditional backref with more than two branches", start));
                        }
                    }
                    Last::Nothing
                }
                ')' => {
                    let Some(group) = self.open.pop() else {
                        return Err(self.error("unbalanced parenthesis", start));
                    };
                    if let Some(number) = group.number {
                        self.closed[number] = true;
                    }
                    Last::Atom
                }
                '(' => match self.open_group(start)? {
                    Some(item) => item,
                    None => last,
                },
                '[' => {
                    self.class(start)?;
                    Last::Atom
                }
                '\\' => match self.escape(start)? {
                    Escape::Anchor => Last::Anchor,
                    Escape::Literal(_) | Escape::Category => Last::Atom,
                },
                '^' | '$' => Last::Anchor,
                '*' | '+' | '?' => self.repeat(last, start)?,
                '{' => match self.brace_repeat(start)? {
                    true => self.repeat(last, start)?,
                    false => Last::Atom,
                },
                _ => Last::Atom,
            };
        }
        match self.open.last() {
            Some(group) => Err(self.error("missing ), unterminated subpattern", group.pos)),
            None => Ok(()),
        }
    }

    fn repeat(&mut self, last: Last, start: usize) -> Result<Last, String> {
        match last {
            Last::Atom => {
                // Lazy and possessive forms
                if !self.eat('?') {
                    self.eat('+');
                }
                Ok(Last::Repeat)
            }
            Last::Repeat => Err(self.error("multiple repeat", start)),
            Last::Nothing | Last::Anchor => Err(self.error("nothing to repeat", start)),
        }
    }

    /// After a `{`: whether it starts a `{m,n}` repeat (consumed), or is a
    /// literal brace.
    fn brace_repeat(&mut self, start: usize) -> Result<bool, String> {
        let here = self.pos;
        if self.peek() == Some('}') {
            return Ok(false);
        }
        let digits = |this: &mut Self| {
            let mut s = String::new();
            while let Some(c) = this.peek().filter(char::is_ascii_digit) {
                s.push(c);
                this.pos += 1;
            }
            s
        };
        let lo = digits(self);
        let hi = if self.eat(',') {
            digits(self)
        } else {
            lo.clone()
        };
        if !self.eat('}') {
            self.pos = here;
            return Ok(false);
        }
        let parse = |s: &str| s.parse::<u64>().unwrap_or(u64::MAX);
        let min = if lo.is_empty() { 0 } else { parse(&lo) };
        let max = if hi.is_empty() { MAXREPEAT } else { parse(&hi) };
        if (!lo.is_empty() && min >= MAXREPEAT) || (!hi.is_empty() && max >= MAXREPEAT) {
            return Err(self.error("the repetition number is too large", start + 1));
        }
        if max < min {
            return Err(self.error("min repeat greater than max repeat", start + 1));
        }
        Ok(true)
    }

    /// Reads a name up to `terminator` (`getuntil`); `what` names it in
    /// errors.
    fn name_until(&mut self, terminator: char, what: &str) -> Result<String, String> {
        let start = self.pos;
        let mut name = String::new();
        loop {
            match self.next() {
                None if name.is_empty() => return Err(self.error(format!("missing {what}"), start)),
                None => {
                    return Err(
                        self.error(format!("missing {terminator}, unterminated name"), start)
                    );
                }
                Some(c) if c == terminator => break,
                Some(c) => name.push(c),
            }
        }
        if name.is_empty() {
            return Err(self.error(format!("missing {what}"), start));
        }
        Ok(name)
    }

    fn check_identifier(&self, name: &str, pos: usize) -> Result<(), String> {
        if is_identifier(name) {
            Ok(())
        } else {
            Err(self.error(format!("bad character in group name '{name}'"), pos))
        }
    }

    fn push_group(&mut self, pos: usize, number: Option<usize>, conditional: bool) {
        self.open.push(OpenGroup {
            pos,
            number,
            conditional,
            branches: 1,
        });
    }

    fn new_group(&mut self) -> usize {
        self.groups += 1;
        self.closed.push(false);
        self.groups
    }

    /// After a `(`. Returns the item it makes, or `None` for comments and
    /// global flags, which leave the previous item as it was.
    fn open_group(&mut self, start: usize) -> Result<Option<Last>, String> {
        if !self.eat('?') {
            self.seen_item = true;
            let number = self.new_group();
            self.push_group(start, Some(number), false);
            return Ok(Some(Last::Nothing));
        }
        let Some(c) = self.next() else {
            return Err(self.error("unexpected end of pattern", self.pos));
        };
        if c == '#' {
            loop {
                match self.next() {
                    None => return Err(self.error("missing ), unterminated comment", start)),
                    Some(')') => return Ok(None),
                    Some(_) => {}
                }
            }
        }
        if FLAG_CHARS.contains(c) || c == '-' {
            return self.flags(c, start);
        }
        self.seen_item = true;
        match c {
            'P' => {
                if self.eat('<') {
                    let pos = self.pos;
                    let name = self.name_until('>', "group name")?;
                    self.check_identifier(&name, pos)?;
                    if let Some(previous) = self.names.get(&name) {
                        let message = format!(
                            "redefinition of group name '{name}' as group {}; was group {previous}",
                            self.groups + 1
                        );
                        return Err(self.error(message, pos));
                    }
                    let number = self.new_group();
                    self.names.insert(name, number);
                    self.push_group(start, Some(number), false);
                    Ok(Some(Last::Nothing))
                } else if self.eat('=') {
                    let pos = self.pos;
                    let name = self.name_until(')', "group name")?;
                    self.check_identifier(&name, pos)?;
                    let Some(&number) = self.names.get(&name) else {
                        return Err(self.error(format!("unknown group name '{name}'"), pos));
                    };
                    if !self.closed[number] {
                        return Err(self.error("cannot refer to an open group", pos));
                    }
                    Ok(Some(Last::Atom))
                } else {
                    match self.next() {
                        None => Err(self.error("unexpected end of pattern", self.pos)),
                        Some(c) => Err(self.error(format!("unknown extension ?P{c}"), start + 1)),
                    }
                }
            }
            ':' | '>' | '=' | '!' => {
                self.push_group(start, None, false);
                Ok(Some(Last::Nothing))
            }
            '<' => match self.next() {
                None => Err(self.error("unexpected end of pattern", self.pos)),
                Some('=' | '!') => {
                    self.push_group(start, None, false);
                    Ok(Some(Last::Nothing))
                }
                Some(c) => Err(self.error(format!("unknown extension ?<{c}"), start + 1)),
            },
            '(' => {
                let pos = self.pos;
                let name = self.name_until(')', "group name")?;
                if is_identifier(&name) {
                    if !self.names.contains_key(&name) {
                        return Err(self.error(format!("unknown group name '{name}'"), pos));
                    }
                } else {
                    let Some(number) = name
                        .parse::<usize>()
                        .ok()
                        .filter(|_| name.bytes().all(|b| b.is_ascii_digit()))
                    else {
                        return Err(
                            self.error(format!("bad character in group name '{name}'"), pos)
                        );
                    };
                    if number == 0 {
                        return Err(self.error("bad group number", pos));
                    }
                    if number > self.groups {
                        return Err(self.error(format!("invalid group reference {number}"), pos));
                    }
                }
                self.push_group(start, None, true);
                Ok(Some(Last::Nothing))
            }
            c => Err(self.error(format!("unknown extension ?{c}"), start + 1)),
        }
    }

    /// Inline flags after `(?` (`_parse_flags`).
    fn flags(&mut self, first: char, start: usize) -> Result<Option<Last>, String> {
        let mut c = first;
        let mut add = String::new();
        if c != '-' {
            loop {
                if c == 'L' {
                    return Err(self.error(
                        "bad inline flags: cannot use 'L' flag with a str pattern",
                        self.pos,
                    ));
                }
                add.push(c);
                if add.contains('a') && add.contains('u') {
                    return Err(self.error(
                        "bad inline flags: flags 'a', 'u' and 'L' are incompatible",
                        self.pos,
                    ));
                }
                match self.next() {
                    None => return Err(self.error("missing -, : or )", self.pos)),
                    Some(next @ (')' | '-' | ':')) => {
                        c = next;
                        break;
                    }
                    Some(next) if FLAG_CHARS.contains(next) => c = next,
                    Some(next) => {
                        let message = if next.is_alphabetic() {
                            "unknown flag"
                        } else {
                            "missing -, : or )"
                        };
                        return Err(self.error(message, self.pos - 1));
                    }
                }
            }
        }
        if c == ')' {
            // Global flags
            if self.seen_item {
                return Err(self.error("global flags not at the start of the expression", start));
            }
            if add.contains('x') {
                self.verbose = true;
            }
            return Ok(None);
        }
        self.seen_item = true;
        let mut remove = String::new();
        if c == '-' {
            match self.next() {
                None => return Err(self.error("missing flag", self.pos)),
                Some(next) if FLAG_CHARS.contains(next) => c = next,
                Some(next) => {
                    let message = if next.is_alphabetic() {
                        "unknown flag"
                    } else {
                        "missing flag"
                    };
                    return Err(self.error(message, self.pos - 1));
                }
            }
            loop {
                if "aLu".contains(c) {
                    return Err(self.error(
                        "bad inline flags: cannot turn off flags 'a', 'u' and 'L'",
                        self.pos,
                    ));
                }
                remove.push(c);
                match self.next() {
                    None => return Err(self.error("missing :", self.pos)),
                    Some(':') => break,
                    Some(next) if FLAG_CHARS.contains(next) => c = next,
                    Some(next) => {
                        let message = if next.is_alphabetic() {
                            "unknown flag"
                        } else {
                            "missing :"
                        };
                        return Err(self.error(message, self.pos - 1));
                    }
                }
            }
        }
        if add.chars().any(|f| remove.contains(f)) {
            return Err(self.error("bad inline flags: flag turned on and off", self.pos - 1));
        }
        self.push_group(start, None, false);
        Ok(Some(Last::Nothing))
    }

    /// Reads `count` hex digits for `\x`, `\u` and `\U`.
    fn hex_escape(&mut self, kind: char, count: usize, start: usize) -> Result<Escape, String> {
        let mut digits = String::new();
        while digits.len() < count {
            match self.peek().filter(char::is_ascii_hexdigit) {
                Some(c) => {
                    digits.push(c);
                    self.pos += 1;
                }
                None => {
                    return Err(self.error(format!("incomplete escape \\{kind}{digits}"), start));
                }
            }
        }
        let value = u32::from_str_radix(&digits, 16).unwrap_or(u32::MAX);
        if value > 0x10FFFF {
            return Err(self.error(format!("bad escape \\{kind}{digits}"), start));
        }
        Ok(Escape::Literal(value))
    }

    fn named_escape(&mut self) -> Result<Escape, String> {
        if !self.eat('{') {
            return Err(self.error("missing {", self.pos));
        }
        self.name_until('}', "character name")?;
        // The character itself is unknown here; use a placeholder that
        // keeps ranges permissive.
        Ok(Escape::Literal(0))
    }

    /// Reads up to `max` more octal digits after `first`.
    fn octal(&mut self, first: char, max: usize, start: usize) -> Result<Escape, String> {
        let mut digits = first.to_string();
        while digits.len() <= max {
            match self.peek().filter(|c| ('0'..='7').contains(c)) {
                Some(c) => {
                    digits.push(c);
                    self.pos += 1;
                }
                None => break,
            }
        }
        let value = u32::from_str_radix(&digits, 8).unwrap_or(u32::MAX);
        if value > 0o377 {
            return Err(self.error(
                format!("octal escape value \\{digits} outside of range 0-0o377"),
                start,
            ));
        }
        Ok(Escape::Literal(value))
    }

    /// An escape outside a character set (`_escape`); `start` is the
    /// backslash.
    fn escape(&mut self, start: usize) -> Result<Escape, String> {
        let Some(c) = self.next() else {
            return Err(self.error("bad escape (end of pattern)", start));
        };
        match c {
            'A' | 'Z' | 'b' | 'B' => Ok(Escape::Anchor),
            'd' | 'D' | 's' | 'S' | 'w' | 'W' => Ok(Escape::Category),
            'x' => self.hex_escape('x', 2, start),
            'u' => self.hex_escape('u', 4, start),
            'U' => self.hex_escape('U', 8, start),
            'N' => self.named_escape(),
            '0' => self.octal('0', 2, start),
            '1'..='9' => {
                let mut digits = c.to_string();
                if let Some(d) = self.peek().filter(char::is_ascii_digit) {
                    self.pos += 1;
                    digits.push(d);
                    let octal = |x: char| ('0'..='7').contains(&x);
                    if octal(c) && octal(d) && self.peek().is_some_and(octal) {
                        let third = self.next().unwrap_or('0');
                        digits.push(third);
                        let value = u32::from_str_radix(&digits, 8).unwrap_or(u32::MAX);
                        if value > 0o377 {
                            return Err(self.error(
                                format!("octal escape value \\{digits} outside of range 0-0o377"),
                                start,
                            ));
                        }
                        return Ok(Escape::Literal(value));
                    }
                }
                let group: usize = digits.parse().unwrap_or(usize::MAX);
                if group <= self.groups {
                    if !self.closed[group] {
                        return Err(self.error("cannot refer to an open group", start));
                    }
                    return Ok(Escape::Category);
                }
                Err(self.error(format!("invalid group reference {group}"), start + 1))
            }
            c if ESCAPES.contains(&c) => Ok(Escape::Literal(escape_value(c))),
            c if c.is_ascii_alphabetic() => Err(self.error(format!("bad escape \\{c}"), start)),
            c => Ok(Escape::Literal(c as u32)),
        }
    }

    /// An escape inside a character set (`_class_escape`).
    fn class_escape(&mut self, start: usize) -> Result<Escape, String> {
        let Some(c) = self.next() else {
            return Err(self.error("bad escape (end of pattern)", start));
        };
        match c {
            'b' => Ok(Escape::Literal(8)),
            'd' | 'D' | 's' | 'S' | 'w' | 'W' => Ok(Escape::Category),
            'x' => self.hex_escape('x', 2, start),
            'u' => self.hex_escape('u', 4, start),
            'U' => self.hex_escape('U', 8, start),
            'N' => self.named_escape(),
            '0'..='7' => self.octal(c, 1, start),
            '8' | '9' => Err(self.error(format!("bad escape \\{c}"), start)),
            c if ESCAPES.contains(&c) => Ok(Escape::Literal(escape_value(c))),
            c if c.is_ascii_alphabetic() => Err(self.error(format!("bad escape \\{c}"), start)),
            c => Ok(Escape::Literal(c as u32)),
        }
    }

    /// A character set after its `[`.
    fn class(&mut self, start: usize) -> Result<(), String> {
        self.eat('^');
        let mut first = true;
        loop {
            let item_start = self.pos;
            let Some(c) = self.next() else {
                return Err(self.error("unterminated character set", start));
            };
            if c == ']' && !first {
                return Ok(());
            }
            first = false;
            let lo = if c == '\\' {
                self.class_escape(item_start)?
            } else {
                Escape::Literal(c as u32)
            };
            if !self.eat('-') {
                continue;
            }
            let that_start = self.pos;
            let Some(that) = self.next() else {
                return Err(self.error("unterminated character set", start));
            };
            if that == ']' {
                return Ok(());
            }
            let hi = if that == '\\' {
                self.class_escape(that_start)?
            } else {
                Escape::Literal(that as u32)
            };
            let range: String = self.chars[item_start..self.pos].iter().collect();
            match (lo, hi) {
                (Escape::Literal(lo), Escape::Literal(hi)) if hi >= lo => {}
                _ => return Err(self.error(format!("bad character range {range}"), item_start)),
            }
        }
    }
}

/// Single-character escapes for control characters, and the backslash.
const ESCAPES: [char; 7] = ['a', 'f', 'n', 'r', 't', 'v', '\\'];

fn escape_value(c: char) -> u32 {
    match c {
        'a' => 7,
        'f' => 12,
        'n' => 10,
        'r' => 13,
        't' => 9,
        'v' => 11,
        c => c as u32,
    }
}

/// Python's `str.isidentifier`, near enough.
fn is_identifier(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(|c| c == '_' || c.is_alphabetic())
        && chars.all(|c| c == '_' || c.is_alphanumeric())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(family: &str, size: f32, bold: bool, italic: bool) -> FontSpec {
        FontSpec {
            family: family.into(),
            size_pt: size,
            bold,
            italic,
        }
    }

    #[test]
    fn font_strings() {
        assert_eq!(
            font_string(&spec("Fira Code", 11.0, false, false)),
            "Fira Code 11"
        );
        assert_eq!(
            font_string(&spec("Hack", 10.5, true, true)),
            "Hack Bold Italic 10.5"
        );
        assert_eq!(
            font_string(&spec("  Source   Code Pro ", 12.0, false, false)),
            "Source Code Pro 12"
        );
        assert_eq!(font_string(&spec("", 9.0, false, false)), "Monospace 9");
        // Family names that end like a size or style keep their last word
        assert_eq!(
            font_string(&spec("Iosevka 2", 11.0, false, false)),
            "Iosevka 2 11"
        );
        assert_eq!(
            font_string(&spec("Iosevka Bold", 11.0, false, false)),
            "Iosevka Bold, 11"
        );
        assert_eq!(
            font_string(&spec("Foo Light", 11.0, true, false)),
            "Foo Light, Bold 11"
        );
    }

    #[test]
    fn font_strings_round_trip() {
        for s in [
            spec("Fira Code", 11.0, false, false),
            spec("Hack", 10.5, true, true),
            spec("Iosevka 2", 14.0, false, true),
            spec("Foo Light", 8.0, true, false),
        ] {
            assert_eq!(FontSpec::parse(&font_string(&s)), s);
        }
    }

    #[test]
    fn edit_font_keeps_other_parts() {
        let mut settings = Settings {
            custom_font: "monospace, 14".into(),
            ..Settings::default()
        };
        edit_font(&mut settings, |s| s.size_pt = 9.0);
        assert_eq!(settings.custom_font, "monospace 9");
        settings.custom_font = "DejaVu Sans Mono Bold 10".into();
        edit_font(&mut settings, |s| s.family = "Hack".into());
        assert_eq!(settings.custom_font, "Hack Bold 10");
    }

    #[test]
    fn point_formatting() {
        assert_eq!(format_points(11.0), "11");
        assert_eq!(format_points(10.5), "10.5");
        assert_eq!(format_points(12.000_001), "12");
    }

    #[test]
    fn columns_follow_saved_order() {
        let columns = ordered_columns(&meld_config::settings::default_folder_columns());
        let names: Vec<_> = columns
            .iter()
            .map(|c| (c.name.as_str(), c.visible))
            .collect();
        assert_eq!(
            names,
            [
                ("size", true),
                ("modification time", true),
                ("permissions", false),
                ("iso-time", false),
            ]
        );

        let saved = vec![
            FolderColumn::new("permissions", true),
            FolderColumn::new("bogus", true),
            FolderColumn::new("size", false),
        ];
        let names: Vec<_> = ordered_columns(&saved)
            .into_iter()
            .map(|c| (c.name, c.visible))
            .collect();
        assert_eq!(
            names,
            [
                ("permissions".to_string(), true),
                ("size".to_string(), false),
                ("modification time".to_string(), false),
                ("iso-time".to_string(), false),
            ]
        );
        assert!(ordered_columns(&[]).iter().all(|c| !c.visible));
    }

    #[test]
    fn moving_items() {
        let mut items = vec![1, 2, 3];
        assert!(move_item(&mut items, 0, 1));
        assert_eq!(items, [2, 1, 3]);
        assert!(move_item(&mut items, 2, 0));
        assert_eq!(items, [3, 2, 1]);
        // Targets past the end are clamped
        assert!(move_item(&mut items, 0, 7));
        assert_eq!(items, [2, 1, 3]);
        assert!(!move_item(&mut items, 2, 7));
        assert!(!move_item(&mut items, 5, 0));
        let mut empty: Vec<i32> = Vec::new();
        assert!(!move_item(&mut empty, 0, 0));
    }

    #[test]
    fn filter_list_editing() {
        let mut prefs = Preferences {
            page: Page::Filters,
            schemes: Vec::new(),
            selected_filter: [None, None],
            spin_drafts: HashMap::new(),
            font_size_draft: None,
            font_family_draft: None,
        };
        let mut settings = Settings::default();
        let defaults = settings.text_filters.clone();
        let n = defaults.len();
        let mut run = |action| {
            prefs
                .update_filter(FilterKind::Text, action, &mut settings)
                .changed
        };

        // Nothing selected
        assert!(!run(FilterAction::Remove));
        assert!(!run(FilterAction::MoveUp));
        assert!(!run(FilterAction::MoveDown));
        // A new filter is added at the end and selected
        assert!(run(FilterAction::Add));
        assert!(!run(FilterAction::MoveDown));
        assert!(run(FilterAction::MoveUp));
        assert!(run(FilterAction::SetPattern(n - 1, "(".into())));
        assert!(run(FilterAction::SetActive(n - 1, true)));
        assert!(run(FilterAction::SetName(n - 1, "Mine".into())));
        // Select the last default and remove it
        assert!(!run(FilterAction::Select(n + 3)));
        assert!(!run(FilterAction::Select(n)));
        assert!(run(FilterAction::Remove));
        assert!(!run(FilterAction::Remove));
        assert!(!run(FilterAction::Select(0)));
        assert!(!run(FilterAction::MoveUp));
        assert!(run(FilterAction::MoveDown));

        assert_eq!(settings.text_filters.len(), n);
        assert_eq!(settings.text_filters[1], defaults[0]);
        assert_eq!(
            settings.text_filters[n - 1],
            FilterSetting::new("Mine", true, "(")
        );
        assert_eq!(settings.text_filters[2..n - 1], defaults[2..n - 1]);
        assert_eq!(prefs.selected_filter, [None, Some(1)]);
    }

    #[test]
    fn default_filters_are_valid() {
        for f in meld_config::settings::default_filename_filters() {
            assert_eq!(
                filter_error(&f.pattern, FilterKind::Filename),
                None,
                "{}",
                f.pattern
            );
        }
        for f in meld_config::settings::default_text_filters() {
            assert_eq!(
                filter_error(&f.pattern, FilterKind::Text),
                None,
                "{}",
                f.pattern
            );
        }
    }

    #[test]
    fn shell_patterns() {
        assert_eq!(shell_to_regex("*.txt").unwrap(), ".*\\.txt$");
        assert_eq!(shell_to_regex("a?[!x]").unwrap(), "a.[^x]$");
        assert_eq!(shell_to_regex("[^x]").unwrap(), "[\\^x]$");
        assert_eq!(shell_to_regex("*.{py,pyc}").unwrap(), ".*\\.(py|pyc)$");
        assert_eq!(shell_to_regex("[abc").unwrap(), "\\[abc$");
        assert_eq!(shell_to_regex("{abc").unwrap(), "\\{abc$");
        assert_eq!(shell_to_regex("\\*x\\").unwrap(), "\\*x$");
        assert!(shell_to_regex("[]").is_err());
        assert_eq!(
            compile_shell_pattern("*.o  *.a").unwrap(),
            "(.*\\.o|.*\\.a)$"
        );

        assert!(filter_error("", FilterKind::Filename).is_some());
        assert!(filter_error("   ", FilterKind::Filename).is_some());
        assert!(filter_error("[z-a]", FilterKind::Filename).is_some());
        assert!(filter_error("[\\]", FilterKind::Filename).is_some());
        assert!(filter_error("*.{a,[]}", FilterKind::Filename).is_some());
        assert_eq!(filter_error("#*# .#* ~* *~", FilterKind::Filename), None);
        assert_eq!(filter_error("[a-z]*.c", FilterKind::Filename), None);
    }

    #[test]
    fn valid_regexes() {
        for re in [
            "",
            "abc",
            "a|b|",
            "(a)(b)\\2",
            "(?P<x>a)(?P=x)",
            "(?:a|b)*?",
            "a++",
            "a{2,3}",
            "a{,3}",
            "a{,}",
            "a{x}",
            "{",
            "x{}",
            "a{3",
            "[]a]",
            "[^]a]",
            "[a-]",
            "[\\]]",
            "[\\w-]",
            "[\\d.]",
            "[-a]",
            "\\$\\w+(:[^\\n$]+)?\\$",
            "(?i)abc",
            "(?im)(?s)abc",
            "(?i:a)(?-i:b)(?s-i:c)",
            "(?=a)(?!b)(?<=c)(?<!d)",
            "(?>a+)",
            "(?#comment)a",
            "(a)?(?(1)b|c)",
            "(?P<n>a)(?(n)b)",
            "\\x41\\u0041\\U00000041\\N{DASH}",
            "\\0\\012\\101",
            "\\.\\-\\ \\#",
            "(?x) a b # comment (\n c",
            "(?i)(?x)a",
            "(?#x)(?i)a",
            "^$\\A\\Z\\b\\B",
            "[\\x00-\\x7f]",
            "é+",
        ] {
            assert_eq!(check_regex(re), Ok(()), "{re}");
        }
    }

    #[test]
    fn invalid_regexes() {
        for (re, message) in [
            ("(", "missing ), unterminated subpattern at position 0"),
            ("a)", "unbalanced parenthesis at position 1"),
            ("[a", "unterminated character set at position 0"),
            ("[]", "unterminated character set at position 0"),
            ("*a", "nothing to repeat at position 0"),
            ("a|*", "nothing to repeat at position 2"),
            ("(*)", "nothing to repeat at position 1"),
            ("^*", "nothing to repeat at position 1"),
            ("\\b+", "nothing to repeat at position 2"),
            ("a**", "multiple repeat at position 2"),
            ("a{2}{3}", "multiple repeat at position 4"),
            ("{2}", "nothing to repeat at position 0"),
            ("a{3,2}", "min repeat greater than max repeat at position 2"),
            ("[z-a]", "bad character range z-a at position 1"),
            ("[\\d-z]", "bad character range \\d-z at position 1"),
            ("\\", "bad escape (end of pattern) at position 0"),
            ("\\q", "bad escape \\q at position 0"),
            ("[\\A]", "bad escape \\A at position 1"),
            ("\\x4", "incomplete escape \\x4 at position 0"),
            ("\\1", "invalid group reference 1 at position 1"),
            ("(a\\1)", "cannot refer to an open group at position 2"),
            (
                "\\777",
                "octal escape value \\777 outside of range 0-0o377 at position 0",
            ),
            ("(?", "unexpected end of pattern at position 2"),
            ("(?<x>a)", "unknown extension ?<x at position 1"),
            ("(?Px)", "unknown extension ?Px at position 1"),
            ("(?q)", "unknown extension ?q at position 1"),
            (
                "a(?i)",
                "global flags not at the start of the expression at position 1",
            ),
            (
                "(?i-i:a)",
                "bad inline flags: flag turned on and off at position 5",
            ),
            ("(?P<1>a)", "bad character in group name '1' at position 4"),
            (
                "(?P<x>a)(?P<x>b)",
                "redefinition of group name 'x' as group 2; was group 1 at position 12",
            ),
            ("(?P=y)", "unknown group name 'y' at position 4"),
            ("(?#abc", "missing ), unterminated comment at position 0"),
            ("(?(2)a)", "invalid group reference 2 at position 3"),
            (
                "(a)(?(1)b|c|d)",
                "conditional backref with more than two branches at position 11",
            ),
            ("[\\8]", "bad escape \\8 at position 1"),
            (
                "(?L)",
                "bad inline flags: cannot use 'L' flag with a str pattern at position 3",
            ),
            (
                "(?au)",
                "bad inline flags: flags 'a', 'u' and 'L' are incompatible at position 4",
            ),
            (
                "(?-a:x)",
                "bad inline flags: cannot turn off flags 'a', 'u' and 'L' at position 4",
            ),
            ("(?i", "missing -, : or ) at position 3"),
            ("(?i-", "missing flag at position 4"),
            ("(?i-x", "missing : at position 5"),
            ("\\N", "missing { at position 2"),
            ("\\N{", "missing character name at position 3"),
            (
                "x|(?i)",
                "global flags not at the start of the expression at position 2",
            ),
            (
                "a{99999999999}",
                "the repetition number is too large at position 2",
            ),
            ("(?P<x", "missing >, unterminated name at position 4"),
            ("(?P<>a)", "missing group name at position 4"),
            ("(?(0)a)", "bad group number at position 3"),
            ("(?(x)a)", "unknown group name 'x' at position 3"),
        ] {
            assert_eq!(check_regex(re), Err(message.to_string()), "{re}");
        }
    }
}

/// Expands a leading `~` to the home folder.
fn expand_home(path: &str) -> String {
    match (path.strip_prefix('~'), std::env::var("HOME")) {
        (Some(rest), Ok(home)) if rest.is_empty() || rest.starts_with('/') => {
            format!("{home}{rest}")
        }
        _ => path.to_string(),
    }
}
