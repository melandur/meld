//! The folder comparison's row model (`DirDiffTreeStore` in `dirdiff.py`,
//! `DiffTreeStore` in `tree.py`) and its selection.
//!
//! Rows live in an arena and are shared by every pane: row `n` is the
//! same entry in each folder. Scans arrive as [`DirScan`]s addressed by
//! [`WalkItem::tree_path`](meld_files::WalkItem), which index into each
//! folder's original scan children; pruned rows are only unlinked from
//! their parent's display list, so those indices stay valid.

use std::collections::HashSet;

use meld_files::dirdiff::{DirScan, Row, RowStatus, ScanOutcome};
use meld_files::State;

pub type NodeId = usize;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Kind {
    /// A file or folder.
    Entry(Row),
    /// The "empty folder" placeholder.
    Empty,
    /// An error listing a folder, shown in one pane.
    Error { pane: usize, message: String },
}

#[derive(Debug, Clone)]
pub struct Node {
    pub kind: Kind,
    pub parent: Option<NodeId>,
    /// Children in display order: error rows, entries, placeholder.
    pub children: Vec<NodeId>,
    /// Entry children in scan order, for addressing walk results.
    scan_children: Vec<NodeId>,
    pub expanded: bool,
    /// Pruned or deleted; no longer reachable from the root.
    pub removed: bool,
    /// A scan of this folder is in progress.
    pub scanning: bool,
}

impl Node {
    fn new(kind: Kind, parent: Option<NodeId>) -> Self {
        Node {
            kind,
            parent,
            children: Vec::new(),
            scan_children: Vec::new(),
            expanded: false,
            removed: false,
            scanning: false,
        }
    }

    pub fn row(&self) -> Option<&Row> {
        match &self.kind {
            Kind::Entry(row) => Some(row),
            _ => None,
        }
    }
}

/// A row as displayed: the node and its indentation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Visible {
    pub id: NodeId,
    pub depth: usize,
}

#[derive(Debug, Clone, Default)]
pub struct Tree {
    nodes: Vec<Node>,
    root: Option<NodeId>,
}

impl Tree {
    pub fn new() -> Self {
        Tree::default()
    }

    /// Drops every row and starts again from a root row.
    pub fn set_root(&mut self, row: Row) -> NodeId {
        self.nodes.clear();
        let mut node = Node::new(Kind::Entry(row), None);
        node.expanded = true;
        self.nodes.push(node);
        self.root = Some(0);
        0
    }

    pub fn clear(&mut self) {
        self.nodes.clear();
        self.root = None;
    }

    pub fn root(&self) -> Option<NodeId> {
        self.root
    }

    pub fn node(&self, id: NodeId) -> &Node {
        &self.nodes[id]
    }

    pub fn get(&self, id: NodeId) -> Option<&Node> {
        self.nodes.get(id).filter(|n| !n.removed)
    }

    pub fn node_mut(&mut self, id: NodeId) -> &mut Node {
        &mut self.nodes[id]
    }

    pub fn row(&self, id: NodeId) -> Option<&Row> {
        self.get(id).and_then(Node::row)
    }

    fn push(&mut self, kind: Kind, parent: NodeId) -> NodeId {
        let id = self.nodes.len();
        self.nodes.push(Node::new(kind, Some(parent)));
        self.nodes[parent].children.push(id);
        id
    }

    /// Unlinks a folder's children (before it is scanned again).
    pub fn clear_children(&mut self, id: NodeId) {
        let mut stack = std::mem::take(&mut self.nodes[id].children);
        self.nodes[id].scan_children.clear();
        while let Some(child) = stack.pop() {
            self.nodes[child].removed = true;
            stack.extend(std::mem::take(&mut self.nodes[child].children));
            self.nodes[child].scan_children.clear();
        }
    }

    /// Finds the folder a walk result belongs to: `tree_path` indexes the
    /// scan children of each folder below `base`.
    pub fn resolve(&self, base: NodeId, tree_path: &[usize]) -> Option<NodeId> {
        let mut id = base;
        for &index in tree_path {
            id = *self.nodes.get(id)?.scan_children.get(index)?;
        }
        Some(id)
    }

    /// Adds a folder's scan results below it, the way
    /// `_search_recursively_iter()` does for one folder: error rows,
    /// then entries, or a placeholder or pruning when it's empty.
    /// Returns whether the folder had differences.
    pub fn apply_scan(&mut self, id: NodeId, scan: DirScan) -> bool {
        if self.nodes[id].removed {
            return false;
        }
        self.clear_children(id);
        for error in scan.errors {
            self.push(
                Kind::Error {
                    pane: error.pane,
                    message: error.message,
                },
                id,
            );
        }
        for row in scan.children {
            let child = self.push(Kind::Entry(row), id);
            self.nodes[id].scan_children.push(child);
        }
        match scan.outcome {
            ScanOutcome::Entries => {}
            ScanOutcome::Empty => {
                self.push(Kind::Empty, id);
                if self.nodes[id].parent.is_none() {
                    self.nodes[id].expanded = true;
                }
            }
            ScanOutcome::Prune => self.prune(id),
        }
        if scan.differences && !self.nodes[id].removed {
            self.nodes[id].expanded = true;
            self.expand_to(id);
        }
        scan.differences
    }

    /// Removes an empty folder row and any ancestors left empty; the root
    /// gets a placeholder instead.
    fn prune(&mut self, mut id: NodeId) {
        while self.nodes[id].children.is_empty() {
            let Some(parent) = self.nodes[id].parent else {
                self.push(Kind::Empty, id);
                break;
            };
            self.unlink(id);
            id = parent;
        }
    }

    fn unlink(&mut self, id: NodeId) {
        self.clear_children(id);
        self.nodes[id].removed = true;
        if let Some(parent) = self.nodes[id].parent {
            self.nodes[parent].children.retain(|&c| c != id);
        }
    }

    /// Removes a row (a deleted file or folder).
    pub fn remove(&mut self, id: NodeId) {
        if Some(id) == self.root {
            return;
        }
        self.unlink(id);
    }

    /// Replaces a row's details (the root row once it has been read).
    pub fn set_row(&mut self, id: NodeId, row: Row) {
        if let Kind::Entry(old) = &mut self.nodes[id].kind {
            *old = row;
        }
    }

    pub fn set_status(&mut self, id: NodeId, status: RowStatus) {
        if let Kind::Entry(row) = &mut self.nodes[id].kind {
            row.status = status;
        }
    }

    /// The row and its ancestors, innermost first.
    pub fn with_ancestors(&self, id: NodeId) -> Vec<NodeId> {
        let mut out = vec![id];
        let mut current = id;
        while let Some(parent) = self.nodes[current].parent {
            out.push(parent);
            current = parent;
        }
        out
    }

    /// Expands every ancestor of a row so that it is shown.
    pub fn expand_to(&mut self, id: NodeId) {
        let mut current = id;
        while let Some(parent) = self.nodes[current].parent {
            self.nodes[parent].expanded = true;
            current = parent;
        }
    }

    pub fn has_children(&self, id: NodeId) -> bool {
        !self.nodes[id].children.is_empty()
    }

    pub fn set_expanded(&mut self, id: NodeId, expanded: bool) {
        self.nodes[id].expanded = expanded;
    }

    /// Expands or collapses a row and everything below it.
    pub fn set_expanded_recursive(&mut self, id: NodeId, expanded: bool) {
        let mut stack = vec![id];
        while let Some(current) = stack.pop() {
            if !self.nodes[current].children.is_empty() {
                self.nodes[current].expanded = expanded;
            }
            stack.extend(self.nodes[current].children.iter().copied());
        }
    }

    /// The rows shown, in order, with their depths.
    pub fn flatten(&self) -> Vec<Visible> {
        let mut out = Vec::new();
        let Some(root) = self.root else {
            return out;
        };
        let mut stack = vec![(root, 0)];
        while let Some((id, depth)) = stack.pop() {
            out.push(Visible { id, depth });
            let node = &self.nodes[id];
            if node.expanded {
                stack.extend(node.children.iter().rev().map(|&c| (c, depth + 1)));
            }
        }
        out
    }

    /// Every row, shown or collapsed, in depth-first order.
    pub fn all_rows(&self) -> Vec<NodeId> {
        let mut out = Vec::new();
        let Some(root) = self.root else {
            return out;
        };
        let mut stack = vec![root];
        while let Some(id) = stack.pop() {
            out.push(id);
            stack.extend(self.nodes[id].children.iter().rev());
        }
        out
    }

    /// The state of a row in a pane.
    pub fn pane_state(&self, id: NodeId, pane: usize) -> Option<State> {
        match &self.nodes[id].kind {
            Kind::Entry(row) => row.status.panes.get(pane).map(|p| p.state),
            Kind::Empty => Some(State::Empty),
            Kind::Error { pane: p, .. } if *p == pane => Some(State::Error),
            Kind::Error { .. } => None,
        }
    }

    /// Whether a row counts as a change for previous/next change
    /// navigation; like Meld, this looks at the first pane.
    pub fn is_change(&self, id: NodeId) -> bool {
        match &self.nodes[id].kind {
            Kind::Entry(row) => row.status.panes.first().is_some_and(|p| p.state.is_change()),
            Kind::Error { .. } => true,
            Kind::Empty => false,
        }
    }

    /// The previous or next changed row from `from` in tree order,
    /// including collapsed rows (`_find_next_prev_diff`).
    pub fn find_change(&self, from: Option<NodeId>, forward: bool) -> Option<NodeId> {
        let order = self.all_rows();
        let position = from.and_then(|f| order.iter().position(|&id| id == f));
        if forward {
            let start = position.map_or(0, |p| p + 1);
            order[start.min(order.len())..]
                .iter()
                .copied()
                .find(|&id| self.is_change(id))
        } else {
            let end = position.unwrap_or(order.len());
            order[..end].iter().rev().copied().find(|&id| self.is_change(id))
        }
    }

    /// Rows in one of the given states (in the first pane), in tree order.
    pub fn rows_in_states(&self, states: &[State]) -> Vec<NodeId> {
        self.all_rows()
            .into_iter()
            .filter(|&id| matches!(&self.nodes[id].kind, Kind::Entry(row) if row.status.state != State::NonExist && states.contains(&row.status.state)))
            .collect()
    }

    /// A row's identity across rescans: the names from the root down.
    pub fn key(&self, id: NodeId) -> Vec<String> {
        let mut key: Vec<String> = self
            .with_ancestors(id)
            .into_iter()
            .rev()
            .skip(1)
            .map(|n| match &self.nodes[n].kind {
                Kind::Entry(row) => row.names.first().cloned().unwrap_or_default(),
                Kind::Empty => String::new(),
                Kind::Error { message, .. } => message.clone(),
            })
            .collect();
        key.shrink_to_fit();
        key
    }

    /// The folder rows that are expanded, by key.
    pub fn expanded_keys(&self) -> HashSet<Vec<String>> {
        self.all_rows()
            .into_iter()
            .filter(|&id| self.nodes[id].expanded && self.nodes[id].parent.is_some())
            .map(|id| self.key(id))
            .collect()
    }
}

/// The selected rows of the focused pane, GtkTreeSelection-style.
#[derive(Debug, Clone, Default)]
pub struct Selection {
    pub pane: usize,
    pub cursor: Option<NodeId>,
    anchor: Option<NodeId>,
    selected: HashSet<NodeId>,
}

impl Selection {
    pub fn is_selected(&self, id: NodeId) -> bool {
        self.selected.contains(&id)
    }

    pub fn is_empty(&self) -> bool {
        self.selected.is_empty()
    }

    pub fn len(&self) -> usize {
        self.selected.len()
    }

    pub fn clear(&mut self) {
        self.selected.clear();
        self.cursor = None;
        self.anchor = None;
    }

    /// Selects one row and puts the cursor on it.
    pub fn set(&mut self, id: NodeId) {
        self.selected.clear();
        self.selected.insert(id);
        self.cursor = Some(id);
        self.anchor = Some(id);
    }

    /// A click on the row at `index` of `rows`, with Ctrl or Shift held.
    pub fn click(&mut self, rows: &[NodeId], index: usize, ctrl: bool, shift: bool) {
        let Some(&id) = rows.get(index) else {
            return;
        };
        if shift {
            let anchor = self.anchor.and_then(|a| rows.iter().position(|&r| r == a)).unwrap_or(index);
            let (lo, hi) = (anchor.min(index), anchor.max(index));
            if !ctrl {
                self.selected.clear();
            }
            self.selected.extend(rows[lo..=hi].iter().copied());
            self.cursor = Some(id);
        } else if ctrl {
            if !self.selected.remove(&id) {
                self.selected.insert(id);
            }
            self.cursor = Some(id);
            self.anchor = Some(id);
        } else {
            self.set(id);
        }
    }

    /// Moves the cursor to `index`, extending the selection from the
    /// anchor with Shift.
    pub fn move_to(&mut self, rows: &[NodeId], index: usize, extend: bool) {
        if rows.is_empty() {
            return;
        }
        let index = index.min(rows.len() - 1);
        if extend {
            self.click(rows, index, false, true);
        } else {
            self.set(rows[index]);
        }
    }

    /// Moves the cursor by `delta` rows.
    pub fn move_by(&mut self, rows: &[NodeId], delta: isize, extend: bool) {
        if rows.is_empty() {
            return;
        }
        let index = match self.cursor_index(rows) {
            Some(i) => (i as isize + delta).clamp(0, rows.len() as isize - 1) as usize,
            None => 0,
        };
        self.move_to(rows, index, extend);
    }

    pub fn cursor_index(&self, rows: &[NodeId]) -> Option<usize> {
        self.cursor.and_then(|c| rows.iter().position(|&r| r == c))
    }

    pub fn select_all(&mut self, rows: &[NodeId]) {
        self.selected = rows.iter().copied().collect();
        if self.cursor.is_none() {
            self.cursor = rows.first().copied();
        }
    }

    /// The selected rows in display order.
    pub fn selected(&self, rows: &[NodeId]) -> Vec<NodeId> {
        rows.iter().copied().filter(|r| self.selected.contains(r)).collect()
    }

    /// Drops rows that are no longer shown; the cursor moves to the
    /// nearest shown row (`refocus_deleted_path`).
    pub fn retain_visible(&mut self, rows: &[NodeId], previous: &[NodeId]) {
        let shown: HashSet<NodeId> = rows.iter().copied().collect();
        self.selected.retain(|r| shown.contains(r));
        if let Some(cursor) = self.cursor
            && !shown.contains(&cursor)
        {
            // Pick the next row after the old cursor position that still
            // exists, then the previous one.
            let old = previous.iter().position(|&r| r == cursor);
            let replacement = old.and_then(|i| {
                previous[i + 1..]
                    .iter()
                    .chain(previous[..i].iter().rev())
                    .find(|r| shown.contains(r))
                    .copied()
            });
            self.cursor = replacement;
            if let Some(r) = replacement
                && self.selected.is_empty()
            {
                self.selected.insert(r);
            }
        }
        if let Some(anchor) = self.anchor
            && !shown.contains(&anchor)
        {
            self.anchor = self.cursor;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use meld_files::dirdiff::{PaneStatus, ScanError};
    use std::path::PathBuf;

    fn status(states: &[State]) -> RowStatus {
        let state = states.iter().copied().find(|s| *s != State::NonExist).unwrap_or(State::Normal);
        RowStatus {
            state,
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
            different: !matches!(state, State::Normal | State::NoChange),
        }
    }

    fn row(name: &str, is_dir: bool, states: &[State]) -> Row {
        Row {
            names: vec![name.into(); states.len()],
            paths: (0..states.len()).map(|p| PathBuf::from(format!("/{p}/{name}"))).collect(),
            is_dir,
            status: status(states),
        }
    }

    fn scan(children: Vec<Row>, outcome: ScanOutcome, differences: bool) -> DirScan {
        DirScan {
            roots: Vec::new(),
            children,
            errors: Vec::new(),
            invalid_filenames: Vec::new(),
            shadowed: Vec::new(),
            whitespace: Vec::new(),
            outcome,
            differences,
        }
    }

    const N: State = State::Normal;
    const M: State = State::Modified;
    const X: State = State::NonExist;

    fn names(tree: &Tree, rows: &[Visible]) -> Vec<String> {
        rows.iter()
            .map(|v| match &tree.node(v.id).kind {
                Kind::Entry(r) => format!("{}{}", "  ".repeat(v.depth), r.names[0]),
                Kind::Empty => format!("{}(empty)", "  ".repeat(v.depth)),
                Kind::Error { message, .. } => format!("{}!{message}", "  ".repeat(v.depth)),
            })
            .collect()
    }

    /// root/{sub/{a (modified)}, b, c (new)}
    fn sample() -> Tree {
        let mut tree = Tree::new();
        let root = tree.set_root(row("root", true, &[N, N]));
        tree.apply_scan(
            root,
            scan(
                vec![row("sub", true, &[N, N]), row("b", false, &[N, N]), row("c", false, &[State::New, X])],
                ScanOutcome::Entries,
                true,
            ),
        );
        let sub = tree.resolve(root, &[0]).unwrap();
        tree.apply_scan(sub, scan(vec![row("a", false, &[M, M])], ScanOutcome::Entries, true));
        tree
    }

    #[test]
    fn walk_results_build_the_tree_and_expand_differences() {
        let tree = sample();
        let rows = tree.flatten();
        assert_eq!(names(&tree, &rows), ["root", "  sub", "    a", "  b", "  c"]);
        assert_eq!(rows[2].depth, 2);
        assert_eq!(tree.all_rows().len(), 5);
    }

    #[test]
    fn collapsed_rows_are_hidden() {
        let mut tree = sample();
        let sub = tree.resolve(0, &[0]).unwrap();
        tree.set_expanded(sub, false);
        assert_eq!(names(&tree, &tree.flatten()), ["root", "  sub", "  b", "  c"]);
        tree.set_expanded_recursive(0, false);
        assert_eq!(names(&tree, &tree.flatten()), ["root"]);
        tree.set_expanded_recursive(0, true);
        assert_eq!(tree.flatten().len(), 5);
    }

    #[test]
    fn identical_folders_stay_collapsed() {
        let mut tree = Tree::new();
        let root = tree.set_root(row("root", true, &[N, N]));
        tree.apply_scan(root, scan(vec![row("sub", true, &[N, N])], ScanOutcome::Entries, false));
        let sub = tree.resolve(root, &[0]).unwrap();
        tree.apply_scan(sub, scan(vec![row("a", false, &[N, N])], ScanOutcome::Entries, false));
        assert_eq!(names(&tree, &tree.flatten()), ["root", "  sub"]);
    }

    #[test]
    fn errors_come_first_and_empty_folders_get_a_placeholder() {
        let mut tree = Tree::new();
        let root = tree.set_root(row("root", true, &[N, N]));
        let mut s = scan(vec![row("sub", true, &[N, N])], ScanOutcome::Entries, true);
        s.errors.push(ScanError {
            pane: 1,
            message: "Permission denied".into(),
        });
        tree.apply_scan(root, s);
        let sub = tree.resolve(root, &[0]).unwrap();
        tree.apply_scan(sub, scan(Vec::new(), ScanOutcome::Empty, false));
        tree.set_expanded(sub, true);
        assert_eq!(names(&tree, &tree.flatten()), ["root", "  !Permission denied", "  sub", "    (empty)"]);
        let error = tree.flatten()[1].id;
        assert_eq!(tree.pane_state(error, 1), Some(State::Error));
        assert_eq!(tree.pane_state(error, 0), None);
    }

    #[test]
    fn pruning_removes_empty_ancestors_but_keeps_indices() {
        let mut tree = Tree::new();
        let root = tree.set_root(row("root", true, &[N, N]));
        tree.apply_scan(
            root,
            scan(vec![row("d1", true, &[N, N]), row("d2", true, &[N, N])], ScanOutcome::Entries, false),
        );
        let d1 = tree.resolve(root, &[0]).unwrap();
        tree.apply_scan(d1, scan(vec![row("inner", true, &[N, N])], ScanOutcome::Entries, false));
        let inner = tree.resolve(root, &[0, 0]).unwrap();
        tree.apply_scan(inner, scan(Vec::new(), ScanOutcome::Prune, false));
        // inner and then d1 are pruned; d2 is still addressable as [1]
        assert_eq!(names(&tree, &tree.flatten()), ["root", "  d2"]);
        let d2 = tree.resolve(root, &[1]).unwrap();
        tree.apply_scan(d2, scan(Vec::new(), ScanOutcome::Prune, false));
        // Everything is gone: the root gets a placeholder
        assert_eq!(names(&tree, &tree.flatten()), ["root", "  (empty)"]);
    }

    #[test]
    fn rescanning_replaces_children() {
        let mut tree = sample();
        let sub = tree.resolve(0, &[0]).unwrap();
        tree.apply_scan(
            sub,
            scan(vec![row("x", false, &[N, N]), row("y", false, &[N, N])], ScanOutcome::Entries, false),
        );
        assert_eq!(names(&tree, &tree.flatten()), ["root", "  sub", "    x", "    y", "  b", "  c"]);
    }

    #[test]
    fn change_navigation_includes_collapsed_rows() {
        let mut tree = sample();
        let sub = tree.resolve(0, &[0]).unwrap();
        let a = tree.resolve(0, &[0, 0]).unwrap();
        let b = tree.resolve(0, &[1]).unwrap();
        let c = tree.resolve(0, &[2]).unwrap();
        tree.set_expanded(sub, false);
        assert_eq!(tree.find_change(None, true), Some(a));
        assert_eq!(tree.find_change(Some(a), true), Some(c));
        assert_eq!(tree.find_change(Some(c), true), None);
        assert_eq!(tree.find_change(Some(b), false), Some(a));
        assert_eq!(tree.find_change(Some(a), false), None);
        tree.expand_to(a);
        assert!(tree.node(sub).expanded);
        assert_eq!(tree.rows_in_states(&[State::Modified]), vec![a]);
    }

    #[test]
    fn removing_rows_and_keys() {
        let mut tree = sample();
        let a = tree.resolve(0, &[0, 0]).unwrap();
        assert_eq!(tree.key(a), ["sub", "a"]);
        let sub = tree.resolve(0, &[0]).unwrap();
        assert!(tree.expanded_keys().contains(&vec!["sub".to_string()]));
        tree.remove(sub);
        assert!(tree.get(a).is_none());
        assert_eq!(names(&tree, &tree.flatten()), ["root", "  b", "  c"]);
    }

    #[test]
    fn selection_clicks() {
        let rows = vec![10, 11, 12, 13, 14];
        let mut sel = Selection::default();
        sel.click(&rows, 1, false, false);
        assert_eq!(sel.selected(&rows), [11]);
        sel.click(&rows, 3, false, true);
        assert_eq!(sel.selected(&rows), [11, 12, 13]);
        assert_eq!(sel.cursor, Some(13));
        sel.click(&rows, 0, false, true);
        assert_eq!(sel.selected(&rows), [10, 11]);
        sel.click(&rows, 4, true, false);
        assert_eq!(sel.selected(&rows), [10, 11, 14]);
        sel.click(&rows, 10, true, false);
        sel.click(&rows, 0, true, false);
        assert_eq!(sel.selected(&rows), [11, 14]);
        sel.click(&rows, 2, false, false);
        assert_eq!(sel.selected(&rows), [12]);
        assert_eq!(sel.len(), 1);
    }

    #[test]
    fn selection_keyboard() {
        let rows = vec![1, 2, 3, 4];
        let mut sel = Selection::default();
        sel.move_by(&rows, 1, false);
        assert_eq!(sel.cursor, Some(1), "no cursor starts at the top");
        sel.move_by(&rows, 1, false);
        sel.move_by(&rows, 1, true);
        assert_eq!(sel.selected(&rows), [2, 3]);
        sel.move_by(&rows, 10, false);
        assert_eq!(sel.selected(&rows), [4]);
        sel.move_to(&rows, 0, true);
        assert_eq!(sel.selected(&rows), [1, 2, 3, 4]);
        sel.select_all(&rows);
        assert_eq!(sel.len(), 4);
    }

    #[test]
    fn selection_follows_removed_rows() {
        let before = vec![1, 2, 3, 4];
        let after = vec![1, 2, 4];
        let mut sel = Selection::default();
        sel.set(3);
        sel.retain_visible(&after, &before);
        assert_eq!(sel.cursor, Some(4));
        assert_eq!(sel.selected(&after), [4]);
        sel.set(4);
        sel.retain_visible(&[1], &after);
        assert_eq!(sel.cursor, Some(1));
    }
}
