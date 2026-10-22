//! The version control tree as the view shows it: the scanned
//! [`VcRow`] tree flattened into an arena (in display pre-order), which
//! folders are expanded, the visible rows, and the multi-row selection
//! with its cursor and anchor (the `Gtk.TreeView` + `Gtk.TreeSelection`
//! behaviour of `vcview.py`).
//!
//! Everything here is toolkit independent and unit-tested.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};

use meld_vc::{Entry, State, VcRow};

pub type NodeId = usize;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NodeKind {
    Entry(Entry),
    /// The "(Empty)" placeholder of a folder without visible entries.
    Empty(String),
    /// A folder that could not be read.
    Error(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Node {
    pub kind: NodeKind,
    pub depth: usize,
    pub parent: Option<NodeId>,
    pub children: Vec<NodeId>,
}

impl Node {
    pub fn entry(&self) -> Option<&Entry> {
        match &self.kind {
            NodeKind::Entry(e) => Some(e),
            _ => None,
        }
    }

    /// The row's state; placeholders are `Empty` and `Error`.
    pub fn state(&self) -> State {
        match &self.kind {
            NodeKind::Entry(e) => e.state,
            NodeKind::Empty(_) => State::Empty,
            NodeKind::Error(_) => State::Error,
        }
    }

    /// Whether the row is a folder (`DiffTreeStore.is_folder`): a folder
    /// entry, or a path that is a folder on disk.
    pub fn is_folder(&self) -> bool {
        self.entry().is_some_and(|e| e.isdir || e.path.is_dir())
    }
}

/// Modifier keys of a click or key press.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Mods {
    pub ctrl: bool,
    pub shift: bool,
}

impl Mods {
    #[cfg(test)]
    pub const NONE: Mods = Mods {
        ctrl: false,
        shift: false,
    };
}

/// What activating a row (double-click, Return) does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Activation {
    /// Expand or collapse the row.
    Toggle(NodeId),
    /// Open a comparison of this file.
    Diff(PathBuf),
    Nothing,
}

#[derive(Debug, Clone, Default)]
pub struct TreeModel {
    nodes: Vec<Node>,
    expanded: HashSet<NodeId>,
    visible: Vec<NodeId>,
    selected: BTreeSet<NodeId>,
    cursor: Option<NodeId>,
    anchor: Option<NodeId>,
}

impl TreeModel {
    /// A model holding just the root folder row, shown before the first
    /// scan has finished.
    pub fn root_only(location: &Path) -> TreeModel {
        let name = location
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| location.display().to_string());
        let row = VcRow::Entry {
            entry: Entry::new(location, name, State::Normal, true, ""),
            children: Vec::new(),
            expanded: true,
        };
        TreeModel::new(&row)
    }

    /// Builds the model, expanding the rows the scan asked for (and their
    /// ancestors, like `expand_to_path`) and selecting the root.
    pub fn new(root: &VcRow) -> TreeModel {
        let mut model = TreeModel::default();
        let mut hints = Vec::new();
        model.add(root, 0, None, &mut hints);
        for id in hints {
            model.expand_to(id);
            model.expanded.insert(id);
        }
        model.rebuild_visible();
        if !model.nodes.is_empty() {
            model.select_only(0);
        }
        model
    }

    fn add(&mut self, row: &VcRow, depth: usize, parent: Option<NodeId>, hints: &mut Vec<NodeId>) {
        let id = self.nodes.len();
        let kind = match row {
            VcRow::Entry { entry, .. } => NodeKind::Entry(entry.clone()),
            VcRow::Empty(text) => NodeKind::Empty(text.clone()),
            VcRow::Error(text) => NodeKind::Error(text.clone()),
        };
        self.nodes.push(Node {
            kind,
            depth,
            parent,
            children: Vec::new(),
        });
        if let Some(p) = parent {
            self.nodes[p].children.push(id);
        }
        if let VcRow::Entry {
            children, expanded, ..
        } = row
        {
            if *expanded && !children.is_empty() {
                hints.push(id);
            }
            for child in children {
                self.add(child, depth + 1, Some(id), hints);
            }
        }
    }

    /// Replaces the tree after a rescan, keeping expanded folders, the
    /// selection and the cursor where their paths still exist.
    pub fn replace(&mut self, root: &VcRow) {
        let expanded: HashSet<PathBuf> = self
            .expanded
            .iter()
            .filter_map(|&id| self.path(id).map(Path::to_path_buf))
            .collect();
        let selected: Vec<PathBuf> = self
            .selected
            .iter()
            .filter_map(|&id| self.path(id).map(Path::to_path_buf))
            .collect();
        let cursor = self
            .cursor
            .and_then(|id| self.path(id).map(Path::to_path_buf));
        let anchor = self
            .anchor
            .and_then(|id| self.path(id).map(Path::to_path_buf));

        let mut model = TreeModel::new(root);
        let index = model.path_index();
        for path in &expanded {
            if let Some(&id) = index.get(path)
                && !model.nodes[id].children.is_empty()
            {
                model.expanded.insert(id);
            }
        }
        let selected: BTreeSet<NodeId> = selected
            .iter()
            .filter_map(|p| index.get(p).copied())
            .collect();
        if !selected.is_empty() {
            for &id in &selected {
                model.expand_to(id);
            }
            model.selected = selected;
            model.cursor = cursor
                .and_then(|p| index.get(&p).copied())
                .or_else(|| model.selected.first().copied());
            model.anchor = anchor.and_then(|p| index.get(&p).copied()).or(model.cursor);
        }
        model.rebuild_visible();
        *self = model;
    }

    fn path_index(&self) -> HashMap<PathBuf, NodeId> {
        self.nodes
            .iter()
            .enumerate()
            .filter_map(|(id, n)| n.entry().map(|e| (e.path.clone(), id)))
            .collect()
    }

    fn rebuild_visible(&mut self) {
        let mut visible = Vec::with_capacity(self.nodes.len());
        let mut stack: Vec<NodeId> = if self.nodes.is_empty() {
            Vec::new()
        } else {
            vec![0]
        };
        while let Some(id) = stack.pop() {
            visible.push(id);
            if self.expanded.contains(&id) {
                stack.extend(self.nodes[id].children.iter().rev());
            }
        }
        self.visible = visible;
    }

    pub fn nodes(&self) -> &[Node] {
        &self.nodes
    }

    pub fn node(&self, id: NodeId) -> &Node {
        &self.nodes[id]
    }

    pub fn root(&self) -> Option<&Node> {
        self.nodes.first()
    }

    /// Node ids of the rows on screen, top to bottom.
    pub fn visible(&self) -> &[NodeId] {
        &self.visible
    }

    pub fn is_expanded(&self, id: NodeId) -> bool {
        self.expanded.contains(&id)
    }

    pub fn has_children(&self, id: NodeId) -> bool {
        !self.nodes[id].children.is_empty()
    }

    pub fn is_selected(&self, id: NodeId) -> bool {
        self.selected.contains(&id)
    }

    pub fn cursor(&self) -> Option<NodeId> {
        self.cursor
    }

    /// Position of the cursor row among the visible rows.
    pub fn cursor_row(&self) -> Option<usize> {
        let cursor = self.cursor?;
        self.visible.iter().position(|&id| id == cursor)
    }

    fn path(&self, id: NodeId) -> Option<&Path> {
        self.nodes.get(id)?.entry().map(|e| e.path.as_path())
    }

    /// Selected entries in tree order (placeholders carry no path).
    pub fn selected_entries(&self) -> Vec<&Entry> {
        self.selected
            .iter()
            .filter_map(|&id| self.nodes[id].entry())
            .collect()
    }

    /// Selected paths without trailing slashes (`_get_selected_files`).
    pub fn selected_paths(&self) -> Vec<PathBuf> {
        meld_vc::view::clean_selection(self.selected_entries().into_iter().map(|e| e.path.clone()))
    }

    /// The selection as `(path, state)` pairs for action sensitivity.
    pub fn path_states(&self) -> Vec<(PathBuf, State)> {
        self.selected_entries()
            .into_iter()
            .map(|e| (e.path.clone(), e.state))
            .collect()
    }

    // ----- Expansion -----

    pub fn set_expanded(&mut self, id: NodeId, expand: bool) {
        if id >= self.nodes.len() || self.nodes[id].children.is_empty() {
            return;
        }
        if expand {
            self.expanded.insert(id);
        } else if self.expanded.remove(&id) {
            self.collapse_selection(id);
        }
        self.rebuild_visible();
    }

    pub fn toggle_expanded(&mut self, id: NodeId) {
        let expand = !self.is_expanded(id);
        self.set_expanded(id, expand);
    }

    /// Expands every ancestor of `id` so that it is visible.
    pub fn expand_to(&mut self, id: NodeId) {
        let mut parent = self.nodes.get(id).and_then(|n| n.parent);
        while let Some(p) = parent {
            self.expanded.insert(p);
            parent = self.nodes[p].parent;
        }
    }

    fn is_descendant(&self, id: NodeId, ancestor: NodeId) -> bool {
        let mut parent = self.nodes[id].parent;
        while let Some(p) = parent {
            if p == ancestor {
                return true;
            }
            parent = self.nodes[p].parent;
        }
        false
    }

    /// Collapsing a row deselects its hidden descendants and moves a
    /// hidden cursor to the collapsed row, like GTK.
    fn collapse_selection(&mut self, collapsed: NodeId) {
        let before = self.selected.len();
        let hidden: Vec<NodeId> = self
            .selected
            .iter()
            .copied()
            .filter(|&id| self.is_descendant(id, collapsed))
            .collect();
        for id in hidden {
            self.selected.remove(&id);
        }
        let cursor_hidden = self
            .cursor
            .is_some_and(|c| self.is_descendant(c, collapsed));
        if cursor_hidden {
            self.cursor = Some(collapsed);
        }
        if self
            .anchor
            .is_some_and(|a| self.is_descendant(a, collapsed))
        {
            self.anchor = self.cursor;
        }
        if (cursor_hidden || before != self.selected.len()) && self.selected.is_empty() {
            self.selected.insert(collapsed);
        }
    }

    // ----- Selection -----

    pub fn select_only(&mut self, id: NodeId) {
        self.selected.clear();
        self.selected.insert(id);
        self.cursor = Some(id);
        self.anchor = Some(id);
    }

    pub fn select_all(&mut self) {
        self.selected = self.visible.iter().copied().collect();
    }

    fn select_range(&mut self, from: usize, to: usize, add: bool) {
        if !add {
            self.selected.clear();
        }
        let (a, b) = if from <= to { (from, to) } else { (to, from) };
        self.selected.extend(self.visible[a..=b].iter().copied());
    }

    /// A click on the visible row `row`: plain clicks select the row,
    /// Ctrl toggles it and Shift selects the range from the anchor.
    pub fn click(&mut self, row: usize, mods: Mods) {
        let Some(&id) = self.visible.get(row) else {
            return;
        };
        let anchor_row = self
            .anchor
            .and_then(|a| self.visible.iter().position(|&v| v == a));
        match (mods.shift, anchor_row) {
            (true, Some(anchor_row)) => {
                self.select_range(anchor_row, row, mods.ctrl);
                self.cursor = Some(id);
            }
            _ if mods.ctrl => {
                if !self.selected.remove(&id) {
                    self.selected.insert(id);
                }
                self.cursor = Some(id);
                self.anchor = Some(id);
            }
            _ => self.select_only(id),
        }
    }

    /// Moves the cursor to the visible row `row` (clamped), selecting it,
    /// or extending the selection from the anchor with `extend`.
    pub fn move_to_row(&mut self, row: usize, extend: bool) {
        if self.visible.is_empty() {
            return;
        }
        let row = row.min(self.visible.len() - 1);
        if extend {
            self.click(
                row,
                Mods {
                    ctrl: false,
                    shift: true,
                },
            );
        } else {
            self.select_only(self.visible[row]);
        }
    }

    /// Moves the cursor by `delta` visible rows.
    pub fn move_cursor(&mut self, delta: isize, extend: bool) {
        if self.visible.is_empty() {
            return;
        }
        let row = match self.cursor_row() {
            Some(r) => r.saturating_add_signed(delta),
            None => 0,
        };
        self.move_to_row(row, extend);
    }

    /// Left arrow: collapse an expanded row, otherwise go to the parent.
    pub fn key_left(&mut self) {
        let Some(id) = self.cursor else { return };
        if self.is_expanded(id) {
            self.set_expanded(id, false);
        } else if let Some(parent) = self.nodes[id].parent {
            self.select_only(parent);
        }
    }

    /// Right arrow: expand a collapsed row, otherwise go to its first child.
    pub fn key_right(&mut self) {
        let Some(id) = self.cursor else { return };
        if !self.has_children(id) {
            return;
        }
        if !self.is_expanded(id) {
            self.set_expanded(id, true);
        } else {
            let first = self.nodes[id].children[0];
            self.select_only(first);
        }
    }

    /// Selects `id` alone, expanding its ancestors.
    pub fn go_to(&mut self, id: NodeId) {
        self.expand_to(id);
        self.rebuild_visible();
        self.select_only(id);
    }

    // ----- Changes -----

    /// Whether a row counts as a change for previous/next change
    /// (`_find_next_prev_diff`).
    fn is_change(&self, id: NodeId) -> bool {
        !matches!(
            self.nodes[id].state(),
            State::Normal | State::NoChange | State::Empty
        )
    }

    /// The nearest changed row before (`forward == false`) or after the
    /// cursor, in tree order and including collapsed rows.
    pub fn find_change(&self, forward: bool) -> Option<NodeId> {
        let start = self.cursor.unwrap_or(0);
        if forward {
            (start + 1..self.nodes.len()).find(|&id| self.is_change(id))
        } else {
            (0..start).rev().find(|&id| self.is_change(id))
        }
    }

    /// What activating `id` does (`on_row_activated`).
    pub fn activation(&self, id: NodeId) -> Activation {
        let node = &self.nodes[id];
        if !node.children.is_empty() {
            Activation::Toggle(id)
        } else if let Some(entry) = node.entry()
            && !node.is_folder()
        {
            Activation::Diff(entry.path.clone())
        } else {
            Activation::Nothing
        }
    }

    /// Number of entry rows below the root (for the status line).
    pub fn entry_count(&self) -> usize {
        self.nodes
            .iter()
            .skip(1)
            .filter(|n| n.entry().is_some())
            .count()
    }

    /// The name shown for each visible row, top to bottom (for
    /// type-to-search).
    pub fn visible_names(&self) -> Vec<String> {
        self.visible
            .iter()
            .map(|&id| match &self.nodes[id].kind {
                NodeKind::Entry(e) => e
                    .path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| e.name.clone()),
                NodeKind::Empty(_) | NodeKind::Error(_) => String::new(),
            })
            .collect()
    }

    // ----- Partial refreshes -----

    /// The row of the entry at `path` (`find_iter_by_name`).
    pub fn find(&self, path: &Path) -> Option<NodeId> {
        self.nodes
            .iter()
            .position(|n| n.entry().is_some_and(|e| e.path == path))
    }

    fn root_path(&self) -> Option<&Path> {
        self.path(0)
    }

    /// The folder row to rescan when something in folder `dir` changed:
    /// the nearest folder row at or above it, or the root for folders
    /// outside the tree (such as a repository root above the location).
    pub fn folder_for(&self, dir: &Path) -> Option<PathBuf> {
        let root = self.root_path()?;
        let mut current = dir;
        loop {
            if !current.starts_with(root) || current == root {
                return Some(root.to_path_buf());
            }
            if let Some(id) = self.find(current)
                && self.nodes[id].entry().is_some_and(|e| e.isdir)
            {
                return Some(current.to_path_buf());
            }
            current = current.parent()?;
        }
    }

    /// The tree as scan rows, with `folder`'s row replaced by `new`.
    fn to_row(&self, id: NodeId, splice: Option<(NodeId, &VcRow)>) -> VcRow {
        let node = &self.nodes[id];
        if let Some((target, new)) = splice
            && target == id
        {
            return match (new, node.entry()) {
                (
                    VcRow::Entry {
                        entry,
                        children,
                        expanded,
                    },
                    Some(old),
                ) => {
                    // The root keeps its own entry; other folders keep
                    // their name (which differs in flattened trees).
                    let entry = if id == 0 {
                        old.clone()
                    } else {
                        Entry {
                            name: old.name.clone(),
                            ..entry.clone()
                        }
                    };
                    VcRow::Entry {
                        entry,
                        children: children.clone(),
                        expanded: *expanded || id == 0,
                    }
                }
                _ => new.clone(),
            };
        }
        match &node.kind {
            NodeKind::Entry(entry) => VcRow::Entry {
                entry: entry.clone(),
                children: node
                    .children
                    .iter()
                    .map(|&c| self.to_row(c, splice))
                    .collect(),
                expanded: id == 0,
            },
            NodeKind::Empty(text) => VcRow::Empty(text.clone()),
            NodeKind::Error(text) => VcRow::Error(text.clone()),
        }
    }

    /// Replaces the rows of the folder at `folder` with a rescan of it
    /// (`_search_recursively_iter(path, replace=True)`), keeping the
    /// expanded folders, selection and cursor elsewhere. Returns false if
    /// the folder isn't in the tree.
    pub fn replace_subtree(&mut self, folder: &Path, row: &VcRow) -> bool {
        let Some(id) = self.find(folder) else {
            return false;
        };
        let combined = self.to_row(0, Some((id, row)));
        self.replace(&combined);
        true
    }

    /// Updates the state of the row for `entry.path`
    /// (`_update_item_state()`). Returns false if there is no such row.
    pub fn update_entry(&mut self, entry: &Entry) -> bool {
        let Some(id) = self.find(&entry.path) else {
            return false;
        };
        if let NodeKind::Entry(e) = &mut self.nodes[id].kind {
            e.state = entry.state;
            e.options = entry.options.clone();
        }
        true
    }
}

/// What to refresh after a command ran or a file was saved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refresh {
    /// Rescan everything (flattened trees, like Meld's `refresh()`).
    Full,
    /// Rescan one folder's rows (`refresh_partial()`).
    Folder(PathBuf),
    /// Update one entry's state (`on_file_changed()`).
    Entry(PathBuf),
}

impl Refresh {
    /// The refresh covering both `self` and `other`, for refreshes asked
    /// for while another one runs.
    pub fn merge(self, other: Refresh) -> Refresh {
        match (self, other) {
            (Refresh::Full, _) | (_, Refresh::Full) => Refresh::Full,
            (Refresh::Entry(a), Refresh::Entry(b)) if a == b => Refresh::Entry(a),
            (a, b) => Refresh::Folder(common_ancestor(&a.folder(), &b.folder())),
        }
    }

    fn folder(&self) -> PathBuf {
        match self {
            Refresh::Full => PathBuf::from("/"),
            Refresh::Folder(f) => f.clone(),
            Refresh::Entry(p) => p.parent().map(Path::to_path_buf).unwrap_or_default(),
        }
    }
}

fn common_ancestor(a: &Path, b: &Path) -> PathBuf {
    a.components()
        .zip(b.components())
        .take_while(|(x, y)| x == y)
        .map(|(x, _)| x)
        .collect()
}

/// The refresh after commands that ran in the folders `dirs`.
pub fn plan_command_refresh(tree: &TreeModel, flatten: bool, dirs: &[PathBuf]) -> Option<Refresh> {
    if dirs.is_empty() {
        return None;
    }
    if flatten {
        return Some(Refresh::Full);
    }
    dirs.iter()
        .filter_map(|d| tree.folder_for(d))
        .map(Refresh::Folder)
        .reduce(Refresh::merge)
        .or(Some(Refresh::Full))
}

/// The refresh after `path` was saved: its row's state, or the folder
/// it appeared in. Paths outside the tree need nothing.
pub fn plan_file_refresh(tree: &TreeModel, flatten: bool, path: &Path) -> Option<Refresh> {
    let root = tree.root_path()?;
    if !path.starts_with(root) {
        return None;
    }
    match tree.find(path) {
        Some(id) if !tree.node(id).is_folder() => Some(Refresh::Entry(path.to_path_buf())),
        _ if flatten => Some(Refresh::Full),
        _ => tree.folder_for(path.parent()?).map(Refresh::Folder),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(path: &str, state: State) -> VcRow {
        let name = Path::new(path)
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        VcRow::Entry {
            entry: Entry::new(path, name, state, false, ""),
            children: Vec::new(),
            expanded: false,
        }
    }

    fn dir(path: &str, state: State, expanded: bool, children: Vec<VcRow>) -> VcRow {
        let name = Path::new(path)
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        VcRow::Entry {
            entry: Entry::new(path, name, state, true, ""),
            children,
            expanded,
        }
    }

    /// /r
    ///   a/          (collapsed)
    ///     a1        modified
    ///   b/          (expanded)
    ///     b1        normal
    ///     c/        (expanded hint, so b is expanded too)
    ///       c1      new
    ///   x           conflict
    ///   (Empty)
    fn sample() -> VcRow {
        dir(
            "/r",
            State::Normal,
            true,
            vec![
                dir(
                    "/r/a",
                    State::Normal,
                    false,
                    vec![file("/r/a/a1", State::Modified)],
                ),
                dir(
                    "/r/b",
                    State::Normal,
                    false,
                    vec![
                        file("/r/b/b1", State::Normal),
                        dir(
                            "/r/b/c",
                            State::Normal,
                            true,
                            vec![file("/r/b/c/c1", State::New)],
                        ),
                    ],
                ),
                file("/r/x", State::Conflict),
                VcRow::Empty("(Empty)".into()),
            ],
        )
    }

    fn names(model: &TreeModel) -> Vec<String> {
        model
            .visible()
            .iter()
            .map(|&id| match &model.node(id).kind {
                NodeKind::Entry(e) => e.name.clone(),
                NodeKind::Empty(t) | NodeKind::Error(t) => t.clone(),
            })
            .collect()
    }

    fn selected_names(model: &TreeModel) -> Vec<String> {
        model
            .selected_entries()
            .iter()
            .map(|e| e.name.clone())
            .collect()
    }

    #[test]
    fn flattens_in_preorder_with_expansion_hints() {
        let model = TreeModel::new(&sample());
        assert_eq!(model.nodes().len(), 9);
        assert_eq!(model.node(3).depth, 1);
        assert_eq!(model.node(6).depth, 3);
        // c is expanded by its hint, which also expands its ancestor b;
        // a stays collapsed.
        assert_eq!(
            names(&model),
            ["r", "a", "b", "b1", "c", "c1", "x", "(Empty)"]
        );
        assert_eq!(selected_names(&model), ["r"]);
        assert_eq!(model.entry_count(), 7);
    }

    #[test]
    fn expand_and_collapse() {
        let mut model = TreeModel::new(&sample());
        model.toggle_expanded(1);
        assert_eq!(names(&model)[..3], ["r", "a", "a1"]);
        // Collapsing a row moves a hidden cursor and selection to it.
        model.click(2, Mods::NONE);
        assert_eq!(selected_names(&model), ["a1"]);
        model.toggle_expanded(1);
        assert_eq!(selected_names(&model), ["a"]);
        assert_eq!(model.cursor(), Some(1));
        // Leaves can't be expanded.
        model.set_expanded(8, true);
        assert!(!model.is_expanded(8));
    }

    #[test]
    fn click_selection_modes() {
        let mut model = TreeModel::new(&sample());
        model.click(1, Mods::NONE);
        model.click(
            3,
            Mods {
                ctrl: true,
                shift: false,
            },
        );
        assert_eq!(selected_names(&model), ["a", "b1"]);
        model.click(
            1,
            Mods {
                ctrl: true,
                shift: false,
            },
        );
        assert_eq!(selected_names(&model), ["b1"]);
        // Shift selects from the anchor (the last ctrl-clicked row).
        model.click(
            6,
            Mods {
                ctrl: false,
                shift: true,
            },
        );
        assert_eq!(selected_names(&model), ["a", "b", "b1", "c", "c1", "x"]);
        model.click(4, Mods::NONE);
        model.click(
            6,
            Mods {
                ctrl: false,
                shift: true,
            },
        );
        assert_eq!(selected_names(&model), ["c", "c1", "x"]);
        // Ctrl+Shift adds the range to the selection.
        model.click(
            0,
            Mods {
                ctrl: true,
                shift: true,
            },
        );
        assert_eq!(
            selected_names(&model),
            ["r", "a", "b", "b1", "c", "c1", "x"]
        );
        // Placeholders can be selected but carry no path.
        model.click(7, Mods::NONE);
        assert!(model.selected_paths().is_empty());
        model.click(42, Mods::NONE);
        assert!(model.is_selected(model.visible()[7]));
    }

    #[test]
    fn keyboard_navigation() {
        let mut model = TreeModel::new(&sample());
        model.move_cursor(1, false);
        assert_eq!(selected_names(&model), ["a"]);
        model.move_cursor(2, true);
        assert_eq!(selected_names(&model), ["a", "b", "b1"]);
        model.move_cursor(-10, false);
        assert_eq!(selected_names(&model), ["r"]);
        model.move_cursor(100, false);
        assert_eq!(model.cursor_row(), Some(7));
        model.move_to_row(4, false);
        model.key_left();
        assert!(!model.is_expanded(5));
        model.key_left();
        assert_eq!(selected_names(&model), ["b"]);
        model.key_right();
        assert_eq!(selected_names(&model), ["b1"]);
        model.move_to_row(4, false);
        model.key_right();
        assert!(model.is_expanded(5));
        model.key_right();
        assert_eq!(selected_names(&model), ["c1"]);
        model.select_all();
        assert_eq!(model.selected_paths().len(), 7);
    }

    #[test]
    fn next_and_previous_change() {
        let mut model = TreeModel::new(&sample());
        let next = model.find_change(true).unwrap();
        // a1 is inside the collapsed folder a; going there expands it.
        assert_eq!(model.node(next).entry().unwrap().name, "a1");
        model.go_to(next);
        assert!(model.is_expanded(1));
        assert_eq!(selected_names(&model), ["a1"]);
        let next = model.find_change(true).unwrap();
        assert_eq!(model.node(next).entry().unwrap().name, "c1");
        model.go_to(next);
        let next = model.find_change(true).unwrap();
        assert_eq!(model.node(next).entry().unwrap().name, "x");
        model.go_to(next);
        assert_eq!(model.find_change(true), None);
        let prev = model.find_change(false).unwrap();
        assert_eq!(model.node(prev).entry().unwrap().name, "c1");
    }

    #[test]
    fn activation_rules() {
        let model = TreeModel::new(&sample());
        assert_eq!(model.activation(1), Activation::Toggle(1));
        assert_eq!(model.activation(7), Activation::Diff(PathBuf::from("/r/x")));
        assert_eq!(model.activation(8), Activation::Nothing);
        // A folder row without children (flattened changed folder).
        let row = dir(
            "/r",
            State::Normal,
            true,
            vec![dir("/r/d", State::New, false, vec![])],
        );
        assert_eq!(TreeModel::new(&row).activation(1), Activation::Nothing);
    }

    #[test]
    fn replace_keeps_expansion_and_selection() {
        let mut model = TreeModel::new(&sample());
        model.toggle_expanded(1);
        model.click(2, Mods::NONE);
        model.click(
            4,
            Mods {
                ctrl: true,
                shift: false,
            },
        );
        assert_eq!(selected_names(&model), ["a1", "b1"]);

        // The rescan drops b1 and collapses everything by default.
        let rescanned = dir(
            "/r",
            State::Normal,
            true,
            vec![
                dir(
                    "/r/a",
                    State::Normal,
                    false,
                    vec![file("/r/a/a1", State::Modified)],
                ),
                dir(
                    "/r/b",
                    State::Normal,
                    false,
                    vec![file("/r/b/b2", State::New)],
                ),
            ],
        );
        model.replace(&rescanned);
        assert_eq!(selected_names(&model), ["a1"]);
        assert_eq!(names(&model), ["r", "a", "a1", "b", "b2"]);
        assert_eq!(
            model.node(model.cursor().unwrap()).entry().unwrap().name,
            "a1"
        );

        // Nothing selected survives: the root is selected.
        let mut model = TreeModel::new(&sample());
        model.click(3, Mods::NONE);
        model.replace(&dir("/r", State::Normal, true, vec![]));
        assert_eq!(selected_names(&model), ["r"]);
    }

    #[test]
    fn partial_refresh_replaces_one_folder() {
        let mut model = TreeModel::new(&sample());
        model.toggle_expanded(1); // expand a
        model.click(2, Mods::NONE); // a1
        assert_eq!(selected_names(&model), ["a1"]);

        // b was rescanned: b1 is now modified, c is gone and d is new
        let rescanned = dir(
            "/r/b",
            State::Normal,
            true,
            vec![file("/r/b/b1", State::Modified), file("/r/b/d", State::New)],
        );
        assert!(model.replace_subtree(Path::new("/r/b"), &rescanned));
        assert_eq!(
            names(&model),
            ["r", "a", "a1", "b", "b1", "d", "x", "(Empty)"]
        );
        assert_eq!(
            selected_names(&model),
            ["a1"],
            "the selection elsewhere stays"
        );
        let b1 = model.find(Path::new("/r/b/b1")).unwrap();
        assert_eq!(model.node(b1).state(), State::Modified);
        assert_eq!(model.node(b1).depth, 2);
        assert!(model.find(Path::new("/r/b/c")).is_none());
        assert!(!model.replace_subtree(Path::new("/r/zzz"), &rescanned));

        // The root keeps its own row
        let root = dir(
            "/r",
            State::Modified,
            true,
            vec![file("/r/only", State::New)],
        );
        assert!(model.replace_subtree(Path::new("/r"), &root));
        assert_eq!(names(&model), ["r", "only"]);
        assert_eq!(model.node(0).state(), State::Normal);
    }

    #[test]
    fn entry_updates() {
        let mut model = TreeModel::new(&sample());
        let mut entry = Entry::new("/r/x", "x", State::Normal, false, "Staged");
        assert!(model.update_entry(&entry));
        let x = model.find(Path::new("/r/x")).unwrap();
        assert_eq!(model.node(x).state(), State::Normal);
        assert_eq!(model.node(x).entry().unwrap().options, "Staged");
        entry.path = PathBuf::from("/r/nope");
        assert!(!model.update_entry(&entry));
    }

    #[test]
    fn refresh_planning() {
        let model = TreeModel::new(&sample());
        assert_eq!(
            model.folder_for(Path::new("/r/b/c")),
            Some(PathBuf::from("/r/b/c"))
        );
        assert_eq!(
            model.folder_for(Path::new("/r/b/c/new/deeper")),
            Some(PathBuf::from("/r/b/c"))
        );
        assert_eq!(model.folder_for(Path::new("/r")), Some(PathBuf::from("/r")));
        // A repository root above the location refreshes the location
        assert_eq!(model.folder_for(Path::new("/")), Some(PathBuf::from("/r")));
        assert_eq!(
            model.folder_for(Path::new("/elsewhere")),
            Some(PathBuf::from("/r"))
        );

        let folders = [PathBuf::from("/r/b/c"), PathBuf::from("/r/a")];
        assert_eq!(
            plan_command_refresh(&model, false, &folders),
            Some(Refresh::Folder("/r".into()))
        );
        assert_eq!(
            plan_command_refresh(&model, false, &folders[..1]),
            Some(Refresh::Folder("/r/b/c".into()))
        );
        assert_eq!(
            plan_command_refresh(&model, true, &folders),
            Some(Refresh::Full)
        );
        assert_eq!(plan_command_refresh(&model, false, &[]), None);

        assert_eq!(
            plan_file_refresh(&model, false, Path::new("/r/x")),
            Some(Refresh::Entry("/r/x".into()))
        );
        assert_eq!(
            plan_file_refresh(&model, true, Path::new("/r/x")),
            Some(Refresh::Entry("/r/x".into()))
        );
        assert_eq!(
            plan_file_refresh(&model, false, Path::new("/r/b/c/brand-new")),
            Some(Refresh::Folder("/r/b/c".into()))
        );
        assert_eq!(
            plan_file_refresh(&model, true, Path::new("/r/b/c/brand-new")),
            Some(Refresh::Full)
        );
        assert_eq!(
            plan_file_refresh(&model, false, Path::new("/other/file")),
            None
        );
    }

    #[test]
    fn refreshes_merge() {
        let entry = |p: &str| Refresh::Entry(p.into());
        let folder = |p: &str| Refresh::Folder(p.into());
        assert_eq!(entry("/r/x").merge(entry("/r/x")), entry("/r/x"));
        assert_eq!(entry("/r/a/1").merge(entry("/r/a/2")), folder("/r/a"));
        assert_eq!(entry("/r/a/1").merge(folder("/r/b")), folder("/r"));
        assert_eq!(folder("/r/b/c").merge(folder("/r/b")), folder("/r/b"));
        assert_eq!(folder("/r/b").merge(Refresh::Full), Refresh::Full);
    }

    #[test]
    fn visible_names_for_search() {
        let model = TreeModel::new(&sample());
        assert_eq!(
            model.visible_names(),
            ["r", "a", "b", "b1", "c", "c1", "x", ""]
        );
    }

    #[test]
    fn root_only_placeholder() {
        let model = TreeModel::root_only(Path::new("/home/me/repo"));
        assert_eq!(names(&model), ["repo"]);
        assert!(model.node(0).entry().unwrap().isdir);
    }
}
