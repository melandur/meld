//! Pull and merge requests in the app ([`meld_forge`]): the forge clients
//! of a repository, the remote picked for each repository (remembered for
//! the session), blocking forge calls off the UI thread, and how pull
//! requests are described in lists.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex};

use iced::Color;
use meld_ailens::picker::parse_iso_date;
use meld_ailens::{CancelToken, GitOptions, GitRepository};
use meld_forge::{
    Forge, ForgeKind, PrState, ProcessRunner, PullRequest, Runner, StateFilter, default_remote,
};

/// The remote picked per repository root, for this session.
static CHOSEN: LazyLock<Mutex<HashMap<PathBuf, String>>> = LazyLock::new(Default::default);

/// Runs the real `gh` and `glab`.
pub fn runner() -> Arc<dyn Runner> {
    Arc::new(ProcessRunner::default())
}

pub fn remembered_remote(root: &Path) -> Option<String> {
    CHOSEN.lock().ok()?.get(root).cloned()
}

pub fn remember_remote(root: &Path, name: &str) {
    if let Ok(mut chosen) = CHOSEN.lock() {
        chosen.insert(root.to_path_buf(), name.to_string());
    }
}

/// A repository's remotes on forges, and the one pull requests are listed
/// from.
#[derive(Debug, Clone)]
pub struct Remotes {
    pub root: PathBuf,
    pub forges: Vec<Forge>,
    pub selected: Option<usize>,
}

impl Remotes {
    pub fn current(&self) -> Option<&Forge> {
        self.forges.get(self.selected?)
    }

    /// Picks remote `index` and remembers it for the repository.
    pub fn select(&mut self, index: usize) {
        if let Some(forge) = self.forges.get(index) {
            self.selected = Some(index);
            remember_remote(&self.root, &forge.remote.name);
        }
    }

    pub fn find(&self, name: &str) -> Option<usize> {
        self.forges.iter().position(|f| f.remote.name == name)
    }
}

/// The forges of the repository `repo`: `preferred` (a remote's name)
/// when it is one of them, else the remote picked before, else the first
/// with pull requests (which asks each forge).
pub fn remotes_of(
    repo: &GitRepository,
    preferred: Option<&str>,
    cancel: &CancelToken,
) -> meld_forge::Result<Remotes> {
    let runner = runner();
    let forges: Vec<Forge> = meld_forge::remote::forge_remotes(repo, cancel)?
        .into_iter()
        .map(|remote| Forge::new(remote, runner.clone()))
        .collect();
    let mut remotes = Remotes {
        root: repo.root.clone(),
        forges,
        selected: None,
    };
    let named = |name: Option<String>| name.and_then(|n| remotes.find(&n));
    let selected = named(preferred.map(str::to_string))
        .or_else(|| named(remembered_remote(&repo.root)))
        .or_else(|| default_remote(&remotes.forges, cancel));
    if let Some(index) = selected {
        remotes.select(index);
    }
    Ok(remotes)
}

/// [`remotes_of`] the repository that `location` is in.
pub fn load_remotes(
    location: &Path,
    preferred: Option<&str>,
    cancel: &CancelToken,
) -> meld_forge::Result<Remotes> {
    let repo = GitRepository::discover(location, &GitOptions::default(), cancel)?;
    remotes_of(&repo, preferred, cancel)
}

/// Runs `work` on a thread of its own; None if it panicked.
pub fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> T + Send + 'static,
) -> impl Future<Output = Option<T>> {
    let (sender, receiver) = iced::futures::channel::oneshot::channel();
    let spawned = std::thread::Builder::new()
        .name("meld-forge".into())
        .spawn(move || {
            let _ = sender.send(work());
        });
    if let Err(err) = spawned {
        log::error!("Can't start a thread: {err}");
    }
    async move { receiver.await.ok() }
}

/// e.g. "3 days ago", "in 2 hours", "now" (`meld_ailens::picker::relative_date`,
/// translated); dates are seconds since the epoch.
pub fn relative_date(date: f64, now: f64) -> String {
    const UNITS: [u64; 6] = [
        365 * 24 * 3600,
        30 * 24 * 3600,
        7 * 24 * 3600,
        24 * 3600,
        3600,
        60,
    ];
    let seconds = (date - now).round() as i64;
    let abs = seconds.unsigned_abs();
    let Some(unit) = UNITS.iter().position(|&size| abs >= size) else {
        return tr!("now").into();
    };
    let n = abs / UNITS[unit];
    match (unit, seconds < 0) {
        (0, true) => trn!("{} year ago", "{} years ago", n, n),
        (1, true) => trn!("{} month ago", "{} months ago", n, n),
        (2, true) => trn!("{} week ago", "{} weeks ago", n, n),
        (3, true) => trn!("{} day ago", "{} days ago", n, n),
        (4, true) => trn!("{} hour ago", "{} hours ago", n, n),
        (_, true) => trn!("{} minute ago", "{} minutes ago", n, n),
        (0, false) => trn!("in {} year", "in {} years", n, n),
        (1, false) => trn!("in {} month", "in {} months", n, n),
        (2, false) => trn!("in {} week", "in {} weeks", n, n),
        (3, false) => trn!("in {} day", "in {} days", n, n),
        (4, false) => trn!("in {} hour", "in {} hours", n, n),
        (_, false) => trn!("in {} minute", "in {} minutes", n, n),
    }
}

/// "pull requests" or "merge requests", for the forge (GitHub when not
/// known), translated; on its own, e.g. as a heading.
pub fn nouns_heading(kind: Option<ForgeKind>) -> &'static str {
    match kind {
        Some(ForgeKind::GitLab) => tr!("Merge requests"),
        _ => tr!("Pull requests"),
    }
}

/// The empty list's text for pull requests (GitHub when not known) in
/// state `filter`.
pub fn no_prs(kind: Option<ForgeKind>, filter: StateFilter) -> &'static str {
    match (kind, filter) {
        (Some(ForgeKind::GitLab), StateFilter::Open) => tr!("No open merge requests"),
        (Some(ForgeKind::GitLab), StateFilter::Closed) => tr!("No closed merge requests"),
        (Some(ForgeKind::GitLab), StateFilter::Merged) => tr!("No merged merge requests"),
        (Some(ForgeKind::GitLab), StateFilter::All) => tr!("No merge requests"),
        (_, StateFilter::Open) => tr!("No open pull requests"),
        (_, StateFilter::Closed) => tr!("No closed pull requests"),
        (_, StateFilter::Merged) => tr!("No merged pull requests"),
        (_, StateFilter::All) => tr!("No pull requests"),
    }
}

/// "Loading pull requests…" or "Loading merge requests…".
pub fn loading_prs(kind: Option<ForgeKind>) -> &'static str {
    match kind {
        Some(ForgeKind::GitLab) => tr!("Loading merge requests…"),
        _ => tr!("Loading pull requests…"),
    }
}

/// e.g. `alice · updated 3 days ago · feature → main`
pub fn pr_description(pr: &PullRequest, now: f64) -> String {
    let mut parts = Vec::new();
    if !pr.author.is_empty() {
        parts.push(pr.author.clone());
    }
    if let Some(date) = parse_iso_date(&pr.updated_at).filter(|_| !pr.updated_at.is_empty()) {
        parts.push(tr!("updated {when}", when = relative_date(date, now)));
    }
    if !pr.head_branch.is_empty() && !pr.base_branch.is_empty() {
        parts.push(format!("{} → {}", pr.head_branch, pr.base_branch));
    }
    if !pr.labels.is_empty() {
        parts.push(pr.labels.join(", "));
    }
    parts.join(" · ")
}

fn hex(rgb: u32) -> Color {
    Color::from_rgb8((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8)
}

/// The color of a state badge: open green, draft grey, merged purple,
/// closed red.
pub fn state_color(pr: &PullRequest) -> Color {
    match pr.state {
        PrState::Open if pr.draft => hex(0x6e7781),
        PrState::Open => hex(0x1f883d),
        PrState::Merged => hex(0x8250df),
        PrState::Closed => hex(0xcf222e),
    }
}

/// A rounded badge with the state's color.
pub fn state_badge<'a, M: 'a>(pr: &PullRequest) -> iced::Element<'a, M> {
    use iced::widget::{container, text};
    let color = state_color(pr);
    container(
        text(crate::i18n::pgettext(
            "pull request state",
            pr.state_label(),
        ))
        .size(13)
        .color(Color::WHITE)
        .wrapping(text::Wrapping::None),
    )
    .padding([1, 7])
    .style(move |_theme: &iced::Theme| container::Style {
        background: Some(iced::Background::Color(color)),
        border: iced::Border::default().rounded(9),
        ..container::Style::default()
    })
    .into()
}

/// A row of toggle chips, one per option; the `selected` one highlighted.
pub fn chips<'a, M: Clone + 'a>(
    options: Vec<(String, M)>,
    selected: Option<usize>,
    size: f32,
) -> iced::Element<'a, M> {
    use iced::widget::{button, row, text};
    let mut chips = row![].spacing(4).align_y(iced::Center);
    for (index, (label, message)) in options.into_iter().enumerate() {
        let on = selected == Some(index);
        chips = chips.push(
            button(text(label).size(size).wrapping(text::Wrapping::None))
                .padding([3, 10])
                .style(move |theme: &iced::Theme, status| {
                    let palette = theme.extended_palette();
                    let mut style = crate::ui::flat_button(theme, status);
                    style.border = iced::Border::default()
                        .rounded(12)
                        .width(1)
                        .color(palette.background.strong.color);
                    if on {
                        style.background = Some(palette.primary.weak.color.scale_alpha(0.6).into());
                        style.text_color = palette.primary.weak.text;
                    }
                    style
                })
                .on_press(message),
        );
    }
    chips.into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn descriptions_list_what_is_known() {
        let pr = PullRequest {
            number: 3,
            author: "alice".into(),
            updated_at: "2026-10-20T00:00:00Z".into(),
            head_branch: "feature".into(),
            base_branch: "main".into(),
            labels: vec!["bug".into()],
            ..Default::default()
        };
        let now = parse_iso_date("2026-10-22T00:00:00Z").unwrap();
        assert_eq!(
            pr_description(&pr, now),
            "alice · updated 2 days ago · feature → main · bug"
        );
        assert_eq!(pr_description(&PullRequest::default(), now), "");
    }

    #[test]
    fn remotes_are_remembered_per_repository() {
        let root = Path::new("/tmp/meld-test-remembered");
        assert_eq!(remembered_remote(root), None);
        remember_remote(root, "github");
        assert_eq!(remembered_remote(root).as_deref(), Some("github"));
    }
}
