//! Messages that the library crates (which don't depend on the app's
//! translations) return as labels, marked here for extraction. The app
//! translates them where it shows them, with
//! `i18n::gettext(state.label())`, or `i18n::pgettext(context, …)` for
//! those marked with a context.

#[allow(dead_code)]
pub const LABELS: &[&str] = &[
    // meld_vc::State::label
    n_!("Ignored"),
    n_!("Unversioned"),
    n_!("Error"),
    n_!("Newly added"),
    n_!("Modified"),
    n_!("Renamed"),
    n_!("Conflict"),
    n_!("Removed"),
    n_!("Missing"),
    n_!("Not present"),
    n_!("Scanning…"),
    // meld_vc::ConflictSide::label
    n_!("Merged"),
    n_!("Base"),
    n_!("Local"),
    n_!("Remote"),
    // meld_files::NewlineType::label
    n_!("UNIX (LF)"),
    n_!("DOS/Windows (CR-LF)"),
    n_!("Mac OS (CR)"),
    // meld_config::DiffColorMode::label
    n_!("From Color Scheme"),
    n_!("Color Scheme, Removed and Added"),
    n_!("Red and Green by Side"),
    n_!("Blue and Orange by Side (Colorblind Safe)"),
    n_!("Red, Green and Blue"),
    n_!("Orange, Blue and Teal (Colorblind Safe)"),
    n_!("Purple, Green and Slate (Red-Green Safe)"),
    n_!("High Contrast"),
    // meld_config::StyleVariant, WrapMode, MergeFileOrder labels
    n_!("Follow System"),
    n_!("Light"),
    n_!("Dark"),
    n_!("Never"),
    n_!("At Spaces"),
    n_!("Anywhere"),
    n_!("At Spaces, Then Anywhere"),
    n_!("Remote, merge, local"),
    n_!("Local, merge, remote"),
    // meld_forge: state filters and pull request states, shown with
    // `pgettext("pull request state", …)`
    nc_!("pull request state", "Open"),
    nc_!("pull request state", "Closed"),
    nc_!("pull request state", "Merged"),
    nc_!("pull request state", "All"),
    nc_!("pull request state", "Draft"),
    // meld_forge: reviews
    n_!("approved"),
    n_!("requested changes"),
    n_!("reviewed"),
    n_!("review dismissed"),
    n_!("Comment"),
    n_!("Approve"),
    n_!("Request changes"),
];
