//! Sync point model (the GTK-free part of `meld/syncpoints.py`).
//!
//! Each pane holds an ordered list of sync point marks (line numbers). When
//! every pane has the same number of marks, they pair up in order and become
//! the valid sync points handed to [`crate::Differ::set_syncpoints`].

/// What the sync point action for the cursor line of a pane would do.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SyncpointAction {
    /// A dangling sync point can be moved to this line.
    Move,
    /// A dangling sync point on this line can be removed.
    Delete,
    /// A sync point can be added to this line to match existing ones.
    Match,
    /// A new, dangling sync point can be added to this line.
    Add,
    /// No sync point action is possible on this line.
    Disabled,
}

/// Sync point state of a pane.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PaneState {
    /// All of the pane's sync points are matched.
    Matched,
    /// The pane is waiting to be matched to sync points in other panes.
    Short,
    /// The pane has a dangling sync point not yet matched in all panes.
    Dangling,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Mark {
    id: u64,
    line: usize,
}

/// Sync points of a two- or three-pane comparison.
#[derive(Clone, Debug)]
pub struct Syncpoints {
    panes: Vec<Vec<Mark>>,
    next_id: u64,
    /// Most recent pairing (mark ids per pane), recalculated only when all
    /// panes have the same number of sync points.
    cached_points: Vec<Vec<u64>>,
    states: Vec<PaneState>,
}

impl Syncpoints {
    pub fn new(num_panes: usize) -> Self {
        let mut s = Syncpoints {
            panes: vec![Vec::new(); num_panes],
            next_id: 0,
            cached_points: Vec::new(),
            states: vec![PaneState::Matched; num_panes],
        };
        s.update_state();
        s
    }

    fn update_state(&mut self) {
        // Walk marks in all panes in lockstep, stopping whenever any pane
        // runs out of marks.
        let shortest = self.panes.iter().map(Vec::len).min().unwrap_or(0);
        let longest = self.panes.iter().map(Vec::len).max().unwrap_or(0);
        if shortest == longest {
            // Every pane ran out on the same step: matched
            self.states = vec![PaneState::Matched; self.panes.len()];
            self.cached_points = (0..shortest)
                .map(|i| self.panes.iter().map(|p| p[i].id).collect())
                .collect();
        } else {
            self.states = self
                .panes
                .iter()
                .map(|p| {
                    if p.len() == shortest {
                        PaneState::Short
                    } else {
                        PaneState::Dangling
                    }
                })
                .collect();
        }
    }

    fn insert_sorted(marks: &mut Vec<Mark>, mark: Mark) {
        let pos = marks.partition_point(|m| m.line <= mark.line);
        marks.insert(pos, mark);
    }

    fn mark_at(&self, pane: usize, line: usize) -> Option<Mark> {
        self.panes
            .get(pane)?
            .iter()
            .copied()
            .find(|m| m.line == line)
    }

    /// Add a sync point at `line` of `pane`.
    pub fn add(&mut self, pane: usize, line: usize) {
        let Some(marks) = self.panes.get_mut(pane) else {
            return;
        };
        let mark = Mark {
            id: self.next_id,
            line,
        };
        self.next_id += 1;
        Self::insert_sorted(marks, mark);
        self.update_state();
    }

    /// Move the first unmatched sync point of `pane` to `line`. Returns
    /// false if there was none.
    pub fn move_unmatched(&mut self, pane: usize, line: usize) -> bool {
        let matched: Vec<u64> = self
            .cached_points
            .iter()
            .filter_map(|p| p.get(pane).copied())
            .collect();
        let Some(marks) = self.panes.get_mut(pane) else {
            return false;
        };
        let Some(pos) = marks.iter().position(|m| !matched.contains(&m.id)) else {
            return false;
        };
        let mut mark = marks.remove(pos);
        mark.line = line;
        Self::insert_sorted(marks, mark);
        self.update_state();
        true
    }

    /// Remove the sync point at `line` of `pane`. Returns false if there was none.
    pub fn remove(&mut self, pane: usize, line: usize) -> bool {
        let Some(mark) = self.mark_at(pane, line) else {
            return false;
        };
        self.panes[pane].retain(|m| m.id != mark.id);
        self.update_state();
        true
    }

    /// Remove all sync points.
    pub fn clear(&mut self) {
        for marks in &mut self.panes {
            marks.clear();
        }
        self.update_state();
    }

    /// Lines of the sync points of `pane`, in order.
    pub fn lines(&self, pane: usize) -> Vec<usize> {
        self.panes
            .get(pane)
            .map(|p| p.iter().map(|m| m.line).collect())
            .unwrap_or_default()
    }

    /// The valid (fully matched) sync points, one line per pane each, as
    /// expected by [`crate::Differ::set_syncpoints`]. Points whose mark was
    /// removed since they were last matched are skipped.
    pub fn valid_points(&self) -> Vec<Vec<usize>> {
        self.cached_points
            .iter()
            .filter_map(|ids| {
                ids.iter()
                    .enumerate()
                    .map(|(pane, id)| {
                        self.panes[pane]
                            .iter()
                            .find(|m| m.id == *id)
                            .map(|m| m.line)
                    })
                    .collect::<Option<Vec<usize>>>()
            })
            .collect()
    }

    pub fn pane_state(&self, pane: usize) -> Option<PaneState> {
        self.states.get(pane).copied()
    }

    /// The sync point action available on `line` of `pane`.
    pub fn action(&self, pane: usize, line: usize) -> SyncpointAction {
        let Some(state) = self.pane_state(pane) else {
            return SyncpointAction::Disabled;
        };
        let mark = self.mark_at(pane, line);
        match state {
            PaneState::Short => {
                if mark.is_some() {
                    SyncpointAction::Disabled
                } else {
                    SyncpointAction::Match
                }
            }
            PaneState::Matched => {
                if mark.is_some() {
                    SyncpointAction::Delete
                } else {
                    SyncpointAction::Add
                }
            }
            PaneState::Dangling => match mark {
                None => SyncpointAction::Move,
                Some(m)
                    if self
                        .cached_points
                        .iter()
                        .any(|p| p.get(pane) == Some(&m.id)) =>
                {
                    SyncpointAction::Disabled
                }
                Some(_) => SyncpointAction::Delete,
            },
        }
    }

    /// Keep marks attached to their text after an edit of `pane` that
    /// added (`size_change > 0`) or removed lines starting at `start_line`,
    /// as text marks do: later marks shift, marks inside a deleted range
    /// collapse onto its start.
    pub fn adjust_for_edit(&mut self, pane: usize, start_line: usize, size_change: isize) {
        let Some(marks) = self.panes.get_mut(pane) else {
            return;
        };
        for m in marks.iter_mut() {
            if m.line > start_line {
                m.line = if size_change >= 0 {
                    m.line + size_change as usize
                } else {
                    m.line
                        .saturating_sub(size_change.unsigned_abs())
                        .max(start_line)
                };
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matching_flow() {
        let mut s = Syncpoints::new(2);
        assert_eq!(s.action(0, 3), SyncpointAction::Add);
        s.add(0, 3);
        assert_eq!(s.pane_state(0), Some(PaneState::Dangling));
        assert_eq!(s.pane_state(1), Some(PaneState::Short));
        assert_eq!(s.action(0, 3), SyncpointAction::Delete);
        assert_eq!(s.action(0, 4), SyncpointAction::Move);
        assert_eq!(s.action(1, 0), SyncpointAction::Match);
        assert!(s.valid_points().is_empty());

        assert!(s.move_unmatched(0, 5));
        assert_eq!(s.lines(0), vec![5]);
        s.add(1, 7);
        assert_eq!(s.valid_points(), vec![vec![5, 7]]);
        assert_eq!(s.action(1, 7), SyncpointAction::Delete);

        // A new dangling point: matched marks are disabled meanwhile
        s.add(0, 1);
        assert_eq!(s.lines(0), vec![1, 5]);
        assert_eq!(s.action(0, 5), SyncpointAction::Disabled);
        assert_eq!(s.action(0, 1), SyncpointAction::Delete);
        assert_eq!(s.action(1, 7), SyncpointAction::Disabled);
        // the stale pairing is still reported as valid
        assert_eq!(s.valid_points(), vec![vec![5, 7]]);
        s.add(1, 2);
        assert_eq!(s.valid_points(), vec![vec![1, 2], vec![5, 7]]);

        s.adjust_for_edit(0, 2, 3);
        assert_eq!(s.lines(0), vec![1, 8]);
        s.adjust_for_edit(0, 0, -5);
        assert_eq!(s.lines(0), vec![0, 3]);

        assert!(s.remove(0, 3));
        assert!(!s.remove(0, 3));
        s.clear();
        assert!(s.valid_points().is_empty());
        assert_eq!(s.pane_state(1), Some(PaneState::Matched));
    }
}
