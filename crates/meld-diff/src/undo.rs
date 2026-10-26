//! Undo/redo bookkeeping (port of `meld/undo.py`).
//!
//! [`UndoSequence`] is generic over the application's action type. It does
//! not perform actions itself: [`UndoSequence::undo`] and
//! [`UndoSequence::redo`] return the actions to revert / re-apply, in order,
//! and the caller applies them *without* recording them again (Python's
//! `busy` flag). Signals become [`UndoEvent`]s, collected until
//! [`UndoSequence::take_events`].

use std::collections::HashMap;

/// An undoable action, tied to the buffer (pane) it modifies.
pub trait UndoAction {
    fn buffer(&self) -> usize;
}

/// Notifications replacing the `can-undo`, `can-redo` and `checkpointed`
/// GObject signals.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UndoEvent {
    CanUndo(bool),
    CanRedo(bool),
    /// The buffer is (`true`) or is no longer (`false`) at its checkpoint,
    /// i.e. unmodified since it was last saved.
    Checkpointed {
        buffer: usize,
        checkpointed: bool,
    },
}

/// One undo step: a single action or a group (`GroupAction`), flattened in
/// the order the actions happened.
#[derive(Clone, Debug)]
enum Step<A> {
    Single(A),
    Group(Vec<A>),
}

impl<A: UndoAction> Step<A> {
    fn buffer(&self) -> Option<usize> {
        match self {
            Step::Single(a) => Some(a.buffer()),
            Step::Group(actions) => actions.first().map(UndoAction::buffer),
        }
    }

    fn flatten_into(self, out: &mut Vec<A>) {
        match self {
            Step::Single(a) => out.push(a),
            Step::Group(actions) => out.extend(actions),
        }
    }
}

/// A checkpoint range `(start, end)`; `end == None` means "to the end".
/// `(None, None)` means the checkpoint was lost.
type Checkpoint = (Option<usize>, Option<usize>);

/// A manager for operations which can be undone/redone (`UndoSequence`).
#[derive(Clone, Debug)]
pub struct UndoSequence<A> {
    buffers: Vec<usize>,
    actions: Vec<Step<A>>,
    next_redo: usize,
    checkpoints: HashMap<usize, Checkpoint>,
    group: Option<Box<UndoSequence<A>>>,
    events: Vec<UndoEvent>,
}

impl<A: UndoAction + Clone> UndoSequence<A> {
    /// An empty sequence for the given buffers, each checkpointed at the start.
    pub fn new(buffers: impl IntoIterator<Item = usize>) -> Self {
        let buffers: Vec<usize> = buffers.into_iter().collect();
        let checkpoints = buffers.iter().map(|&b| (b, (Some(0), None))).collect();
        UndoSequence {
            buffers,
            actions: Vec::new(),
            next_redo: 0,
            checkpoints,
            group: None,
            events: Vec::new(),
        }
    }

    /// Events emitted since the last call.
    pub fn take_events(&mut self) -> Vec<UndoEvent> {
        std::mem::take(&mut self.events)
    }

    /// Remove all undo and redo actions.
    pub fn clear(&mut self) {
        if self.can_undo() {
            self.events.push(UndoEvent::CanUndo(false));
        }
        if self.can_redo() {
            self.events.push(UndoEvent::CanRedo(false));
        }
        self.actions.clear();
        self.next_redo = 0;
        self.checkpoints = self.buffers.iter().map(|&b| (b, (Some(0), None))).collect();
        self.group = None;
    }

    pub fn can_undo(&self) -> bool {
        self.next_redo > 0
    }

    pub fn can_redo(&self) -> bool {
        self.next_redo < self.actions.len()
    }

    /// Add an action to the undo list (or to the current group).
    pub fn add_action(&mut self, action: A) {
        self.add_step(Step::Single(action));
    }

    fn add_step(&mut self, step: Step<A>) {
        if let Some(group) = self.group.as_mut() {
            group.add_step(step);
            return;
        }
        let Some(buffer) = step.buffer() else {
            return;
        };
        if self.checkpointed(buffer) {
            if let Some(cp) = self.checkpoints.get_mut(&buffer) {
                cp.1 = Some(self.next_redo);
            }
            self.events.push(UndoEvent::Checkpointed {
                buffer,
                checkpointed: false,
            });
        } else {
            // If we go back in the undo stack before the checkpoint starts,
            // and then modify the buffer, we lose the checkpoint altogether
            if let Some((Some(start), _)) = self.checkpoints.get(&buffer)
                && *start > self.next_redo
            {
                self.checkpoints.insert(buffer, (None, None));
            }
        }
        let could_undo = self.can_undo();
        let could_redo = self.can_redo();
        self.actions.truncate(self.next_redo);
        self.actions.push(step);
        self.next_redo += 1;
        if !could_undo {
            self.events.push(UndoEvent::CanUndo(true));
        }
        if could_redo {
            self.events.push(UndoEvent::CanRedo(false));
        }
    }

    /// Undo one step. Returns the actions to revert, in the order they must
    /// be reverted (last action first), or `None` if nothing can be undone.
    pub fn undo(&mut self) -> Option<Vec<A>> {
        if self.next_redo == 0 {
            return None;
        }
        let buffer = self.actions[self.next_redo - 1].buffer();
        if let Some(buf) = buffer.filter(|&b| self.checkpointed(b)) {
            self.events.push(UndoEvent::Checkpointed {
                buffer: buf,
                checkpointed: false,
            });
        }
        let could_redo = self.can_redo();
        self.next_redo -= 1;
        let mut actions = Vec::new();
        self.actions[self.next_redo]
            .clone()
            .flatten_into(&mut actions);
        actions.reverse();
        if !self.can_undo() {
            self.events.push(UndoEvent::CanUndo(false));
        }
        if !could_redo {
            self.events.push(UndoEvent::CanRedo(true));
        }
        if let Some(buf) = buffer.filter(|&b| self.checkpointed(b)) {
            self.events.push(UndoEvent::Checkpointed {
                buffer: buf,
                checkpointed: true,
            });
        }
        Some(actions)
    }

    /// Redo one step. Returns the actions to re-apply, in order, or `None`
    /// if nothing can be redone.
    pub fn redo(&mut self) -> Option<Vec<A>> {
        if self.next_redo >= self.actions.len() {
            return None;
        }
        let buffer = self.actions[self.next_redo].buffer();
        if let Some(buf) = buffer.filter(|&b| self.checkpointed(b)) {
            self.events.push(UndoEvent::Checkpointed {
                buffer: buf,
                checkpointed: false,
            });
        }
        let could_undo = self.can_undo();
        let mut actions = Vec::new();
        self.actions[self.next_redo]
            .clone()
            .flatten_into(&mut actions);
        self.next_redo += 1;
        if !could_undo {
            self.events.push(UndoEvent::CanUndo(true));
        }
        if !self.can_redo() {
            self.events.push(UndoEvent::CanRedo(false));
        }
        if let Some(buf) = buffer.filter(|&b| self.checkpointed(b)) {
            self.events.push(UndoEvent::Checkpointed {
                buffer: buf,
                checkpointed: true,
            });
        }
        Some(actions)
    }

    /// Mark the current state of `buffer` as its unmodified state (on save).
    pub fn checkpoint(&mut self, buffer: usize) {
        let buf_of = |i: usize| self.actions[i].buffer();
        let mut start = self.next_redo;
        while start > 0 && buf_of(start - 1) != Some(buffer) {
            start -= 1;
        }
        let mut end = self.next_redo;
        while end + 1 < self.actions.len() && buf_of(end + 1) != Some(buffer) {
            end += 1;
        }
        let end = (end != self.actions.len()).then_some(end);
        self.checkpoints.insert(buffer, (Some(start), end));
        self.events.push(UndoEvent::Checkpointed {
            buffer,
            checkpointed: true,
        });
    }

    /// Whether `buffer` is at its checkpoint (i.e. unmodified).
    pub fn checkpointed(&self, buffer: usize) -> bool {
        let Some(&(Some(start), end)) = self.checkpoints.get(&buffer) else {
            return false;
        };
        let end = end.unwrap_or(self.actions.len());
        start <= self.next_redo && self.next_redo <= end
    }

    /// Start grouping actions into one undo step. Groups nest.
    pub fn begin_group(&mut self) {
        match self.group.as_mut() {
            Some(group) => group.begin_group(),
            None => self.group = Some(Box::new(UndoSequence::new(self.buffers.clone()))),
        }
    }

    /// End the innermost group. Single-action groups collapse to the action;
    /// empty groups are dropped. Unpaired calls are ignored.
    pub fn end_group(&mut self) {
        let Some(group) = self.group.as_mut() else {
            return;
        };
        if group.group.is_some() {
            group.end_group();
            return;
        }
        let Some(group) = self.group.take() else {
            return;
        };
        let mut steps = group.actions;
        match steps.len() {
            0 => {}
            1 => {
                if let Some(step) = steps.pop() {
                    self.add_step(step);
                }
            }
            _ => {
                let mut actions = Vec::new();
                for step in steps {
                    step.flatten_into(&mut actions);
                }
                self.add_step(Step::Group(actions));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone, Debug, PartialEq, Eq)]
    struct Act(usize, &'static str);

    impl UndoAction for Act {
        fn buffer(&self) -> usize {
            self.0
        }
    }

    #[test]
    fn undo_redo_and_events() {
        let mut s = UndoSequence::new([0, 1]);
        assert!(!s.can_undo() && !s.can_redo());
        assert!(s.checkpointed(0));
        s.add_action(Act(0, "a"));
        assert_eq!(
            s.take_events(),
            vec![
                UndoEvent::Checkpointed {
                    buffer: 0,
                    checkpointed: false
                },
                UndoEvent::CanUndo(true)
            ]
        );
        assert!(!s.checkpointed(0));
        assert!(s.checkpointed(1));
        s.add_action(Act(0, "b"));
        assert_eq!(s.undo(), Some(vec![Act(0, "b")]));
        assert!(s.can_redo());
        assert_eq!(s.take_events(), vec![UndoEvent::CanRedo(true)]);
        assert_eq!(s.undo(), Some(vec![Act(0, "a")]));
        assert_eq!(
            s.take_events(),
            vec![
                UndoEvent::CanUndo(false),
                UndoEvent::Checkpointed {
                    buffer: 0,
                    checkpointed: true
                }
            ]
        );
        assert!(s.checkpointed(0));
        assert_eq!(s.undo(), None);
        assert_eq!(s.redo(), Some(vec![Act(0, "a")]));
        assert_eq!(s.redo(), Some(vec![Act(0, "b")]));
        assert_eq!(s.redo(), None);
    }

    #[test]
    fn groups_collapse_and_nest() {
        let mut s = UndoSequence::new([0]);
        s.begin_group();
        s.add_action(Act(0, "a"));
        s.begin_group();
        s.add_action(Act(0, "b"));
        s.add_action(Act(0, "c"));
        s.end_group();
        s.add_action(Act(0, "d"));
        s.end_group();
        assert_eq!(
            s.undo(),
            Some(vec![Act(0, "d"), Act(0, "c"), Act(0, "b"), Act(0, "a")])
        );
        assert!(!s.can_undo());
        assert_eq!(
            s.redo(),
            Some(vec![Act(0, "a"), Act(0, "b"), Act(0, "c"), Act(0, "d")])
        );

        // Single-action groups collapse, empty groups vanish
        let mut s = UndoSequence::new([0]);
        s.begin_group();
        s.end_group();
        assert!(!s.can_undo());
        s.begin_group();
        s.add_action(Act(0, "x"));
        s.end_group();
        assert_eq!(s.undo(), Some(vec![Act(0, "x")]));
        s.end_group(); // unpaired: ignored
    }

    #[test]
    fn checkpoints() {
        let mut s = UndoSequence::new([0, 1]);
        s.add_action(Act(0, "a"));
        s.add_action(Act(1, "b"));
        s.add_action(Act(0, "c"));
        s.checkpoint(0);
        assert!(s.checkpointed(0));
        // Undoing another buffer's action does not modify buffer 0... but
        // undoing its own does.
        s.undo();
        assert!(!s.checkpointed(0));
        s.redo();
        assert!(s.checkpointed(0));

        // Going back before the checkpoint and editing loses it
        s.undo();
        s.undo();
        s.undo();
        assert!(!s.checkpointed(0));
        s.add_action(Act(0, "z"));
        assert!(!s.checkpointed(0));
        s.undo();
        assert!(!s.checkpointed(0));
    }

    #[test]
    fn clear_emits() {
        let mut s = UndoSequence::new([0]);
        s.add_action(Act(0, "a"));
        s.add_action(Act(0, "b"));
        s.undo();
        s.take_events();
        s.clear();
        assert_eq!(
            s.take_events(),
            vec![UndoEvent::CanUndo(false), UndoEvent::CanRedo(false)]
        );
        assert!(s.checkpointed(0));
    }
}
