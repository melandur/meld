//! The link maps between folder panes (`ScrollLinkMap` in `linkmap.py`).
//!
//! Every pane shows the same rows, so the link between two panes is a
//! straight band across the gap for each row that differs between them,
//! coloured like the file comparison's link map: inserts where only one
//! side has the entry, replaces where both have it and it differs.
//! Neighbouring rows of the same kind make one band, outlined at its top
//! and bottom. Scrolling the mouse wheel over a link map goes to the
//! previous or next change, as in Meld.

use iced::mouse;
use iced::widget::canvas::{self, Frame, Geometry, Path, Stroke};
use iced::{Color, Point, Rectangle, Renderer, Size, Theme};
use meld_config::DiffColors;
use meld_files::State;

use super::model::Kind;

/// Width of a link map (`width_request` in `dirdiff.ui`).
pub const WIDTH: f32 = 50.0;

/// How a row links two neighbouring panes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Link {
    /// Only in the right pane.
    Insert,
    /// Only in the left pane.
    Delete,
    Replace,
    Conflict,
    Error,
}

/// The link a row draws between panes `left` and `right`, if any.
pub fn link(kind: &Kind, left: usize, right: usize) -> Option<Link> {
    let Kind::Entry(row) = kind else {
        return None;
    };
    let l = row.status.panes.get(left)?;
    let r = row.status.panes.get(right)?;
    match (l.exists, r.exists) {
        (false, false) => None,
        (false, true) => Some(Link::Insert),
        (true, false) => Some(Link::Delete),
        (true, true) => match (l.state, r.state) {
            (State::Error, _) | (_, State::Error) => Some(Link::Error),
            (State::Conflict, _) | (_, State::Conflict) => Some(Link::Conflict),
            (State::Modified, _) | (_, State::Modified) => Some(Link::Replace),
            _ => None,
        },
    }
}

/// Fill and outline colours of each link.
#[derive(Debug, Clone, Copy)]
pub struct Colors {
    insert: (Color, Color),
    delete: (Color, Color),
    replace: (Color, Color),
    conflict: (Color, Color),
    error: (Color, Color),
}

impl Colors {
    pub fn new(colors: &DiffColors) -> Colors {
        let c = |chunk: meld_config::ChunkColors| (crate::shared::color(chunk.fill), crate::shared::color(chunk.line));
        Colors {
            insert: c(colors.insert),
            delete: c(colors.delete),
            replace: c(colors.replace),
            conflict: c(colors.conflict),
            error: c(colors.error),
        }
    }

    fn of(&self, link: Link) -> (Color, Color) {
        match link {
            Link::Insert => self.insert,
            Link::Delete => self.delete,
            Link::Replace => self.replace,
            Link::Conflict => self.conflict,
            Link::Error => self.error,
        }
    }
}

/// One row's piece of a link map.
#[derive(Debug, Clone, Copy)]
pub struct Cell<Message> {
    pub link: Option<Link>,
    /// The band starts here (the row above links differently).
    pub top: bool,
    /// The band ends here (the row below links differently).
    pub bottom: bool,
    pub colors: Colors,
    /// Messages for scrolling up and down over the map.
    pub on_scroll: (Message, Message),
}

impl<Message: Clone> canvas::Program<Message> for Cell<Message> {
    type State = ();

    fn update(
        &self,
        _state: &mut (),
        event: &canvas::Event,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> Option<canvas::Action<Message>> {
        let iced::Event::Mouse(mouse::Event::WheelScrolled { delta }) = event else {
            return None;
        };
        cursor.position_in(bounds)?;
        let dy = match delta {
            mouse::ScrollDelta::Lines { y, .. } | mouse::ScrollDelta::Pixels { y, .. } => *y,
        };
        if dy == 0.0 {
            return None;
        }
        let message = if dy > 0.0 { self.on_scroll.0.clone() } else { self.on_scroll.1.clone() };
        Some(canvas::Action::publish(message).and_capture())
    }

    fn draw(
        &self,
        _state: &(),
        renderer: &Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: mouse::Cursor,
    ) -> Vec<Geometry> {
        let Some(link) = self.link else {
            return Vec::new();
        };
        let mut frame = Frame::new(renderer, bounds.size());
        let (fill, line) = self.colors.of(link);
        let width = bounds.width;
        let height = bounds.height;
        frame.fill_rectangle(Point::ORIGIN, Size::new(width, height), fill);
        let stroke = Stroke::default().with_width(1.0).with_color(line);
        if self.top {
            frame.stroke(&Path::line(Point::new(0.0, 0.5), Point::new(width, 0.5)), stroke);
        }
        if self.bottom {
            frame.stroke(
                &Path::line(Point::new(0.0, height - 0.5), Point::new(width, height - 0.5)),
                stroke,
            );
        }
        vec![frame.into_geometry()]
    }
}

/// The links of consecutive rows between two panes, with where each
/// band starts and ends: `(link, top, bottom)` per row.
pub fn bands(links: &[Option<Link>]) -> Vec<(Option<Link>, bool, bool)> {
    links
        .iter()
        .enumerate()
        .map(|(i, &link)| {
            let top = i == 0 || links[i - 1] != link;
            let bottom = i + 1 == links.len() || links[i + 1] != link;
            (link, top, bottom)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use meld_files::dirdiff::{PaneStatus, Row, RowStatus};
    use std::path::PathBuf;

    fn entry(states: &[State]) -> Kind {
        Kind::Entry(Row {
            names: vec!["x".into(); states.len()],
            paths: vec![PathBuf::new(); states.len()],
            is_dir: false,
            status: RowStatus {
                state: states[0],
                panes: states
                    .iter()
                    .map(|&s| PaneStatus {
                        state: s,
                        exists: s != State::NonExist,
                        is_dir: false,
                        size: None,
                        mtime: None,
                        mode: None,
                        symlink_target: None,
                    })
                    .collect(),
                different: true,
            },
        })
    }

    #[test]
    fn links_between_panes() {
        use State::{Conflict, Error, Modified, New, NonExist, Normal};
        assert_eq!(link(&entry(&[Normal, Normal]), 0, 1), None);
        assert_eq!(link(&entry(&[Modified, Modified]), 0, 1), Some(Link::Replace));
        assert_eq!(link(&entry(&[New, NonExist]), 0, 1), Some(Link::Delete));
        assert_eq!(link(&entry(&[NonExist, New]), 0, 1), Some(Link::Insert));
        assert_eq!(link(&entry(&[NonExist, NonExist, New]), 0, 1), None);
        // Three panes: the first two are the same, the third is missing
        assert_eq!(link(&entry(&[New, New, NonExist]), 0, 1), None);
        assert_eq!(link(&entry(&[New, New, NonExist]), 1, 2), Some(Link::Delete));
        assert_eq!(link(&entry(&[Error, Error]), 0, 1), Some(Link::Error));
        assert_eq!(link(&entry(&[Conflict, Conflict]), 0, 1), Some(Link::Conflict));
        assert_eq!(link(&Kind::Empty, 0, 1), None);
        assert_eq!(link(&entry(&[Normal, Normal]), 0, 5), None);
    }

    #[test]
    fn bands_join_neighbouring_rows() {
        let r = Some(Link::Replace);
        let i = Some(Link::Insert);
        assert_eq!(
            bands(&[None, r, r, i, None]),
            [
                (None, true, true),
                (r, true, false),
                (r, false, true),
                (i, true, true),
                (None, true, true)
            ]
        );
        assert!(bands(&[]).is_empty());
    }
}
