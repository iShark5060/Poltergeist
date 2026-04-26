//! Selective import / export picker — Rust counterpart of Python's
//! `ui/node_picker_dialog.py`.
//!
//! The picker owns a deep clone of the source tree so the UI can
//! freely toggle check states without ever mutating the live
//! personal/team trees. On accept the live tree is rebuilt from the
//! still-checked subset; on cancel the whole session is discarded.
//!
//! Tristate semantics mirror Qt's `ItemIsAutoTristate`:
//!
//! - Folder checked: all descendants checked
//! - Folder unchecked: all descendants unchecked
//! - Folder partial: at least one descendant differs from the rest
//!   of its siblings
//! - Toggling a partial folder snaps it to fully checked first
//!   (matches the Python branch where `state != PartiallyChecked`
//!   is required before propagating downwards).
//!
//! Folder roll-up (children -> parent) is recomputed on every
//! mutation rather than being driven by an event-loop callback so
//! the data model stays self-consistent regardless of which path
//! triggered the change.

use poltergeist_core::models::{Folder, Node};

/// Tristate check value for a single picker row.
///
/// Numeric values (0/1/2) are kept stable so they can be passed to
/// the Slint UI directly without going through a mapping table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PickerCheck {
    Unchecked = 0,
    Checked = 1,
    Partial = 2,
}

impl PickerCheck {
    pub fn as_int(self) -> i32 {
        self as i32
    }
}

/// Why we opened the picker. The accept handler in `main.rs`
/// dispatches on this to decide whether to write a JSON file or
/// merge/replace into one of the live trees.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PickerPurpose {
    ExportPersonal,
    ExportTeam,
    ImportPersonal,
    ImportTeam,
}

impl PickerPurpose {
    /// Reserved for future toolbar gating; kept to mirror the
    /// import/export branch shape used in `main.rs`.
    #[allow(dead_code)]
    pub fn is_export(self) -> bool {
        matches!(self, Self::ExportPersonal | Self::ExportTeam)
    }
}

/// One node in the picker's working tree. Wraps the original `Node`
/// so the accept step can reach back for fields (id, color, match
/// rule, …) that the picker itself doesn't surface.
#[derive(Clone, Debug)]
pub struct PickerNode {
    pub source: Node,
    pub name: String,
    pub color: Option<String>,
    pub is_folder: bool,
    pub inject_kbd: bool,
    pub children: Vec<PickerNode>,
    pub checked: PickerCheck,
    pub expanded: bool,
}

impl PickerNode {
    fn from_node(node: &Node) -> Self {
        match node {
            Node::Folder(f) => Self {
                source: node.clone(),
                name: f.name.clone(),
                color: f.color.clone(),
                is_folder: true,
                inject_kbd: false,
                children: f.children.iter().map(Self::from_node).collect(),
                checked: PickerCheck::Checked,
                expanded: true,
            },
            Node::Snippet(s) => Self {
                source: node.clone(),
                name: s.name.clone(),
                color: s.color.clone(),
                is_folder: false,
                inject_kbd: matches!(
                    s.injection,
                    Some(poltergeist_core::models::InjectionMode::Typing)
                        | Some(poltergeist_core::models::InjectionMode::TypingCompat)
                ),
                children: Vec::new(),
                checked: PickerCheck::Checked,
                expanded: false,
            },
        }
    }
}

/// Active picker session attached to `AppState`. None when the
/// modal isn't on screen.
pub struct PickerSession {
    pub purpose: PickerPurpose,
    pub roots: Vec<PickerNode>,
    /// Output path (export) or input path (import) — used purely
    /// for the status text after accept.
    pub file_path: std::path::PathBuf,
    /// Path-from-roots of the row under each visible index. Refreshed
    /// every time we re-flatten so callbacks can resolve `i32`
    /// indices back to a tree path without scanning.
    pub visible_paths: Vec<Vec<usize>>,
    /// Filtered tree captured at accept time, kept around so the
    /// follow-up merge/replace prompt can still apply it after the
    /// picker modal closes.
    pub pending_filtered: Option<Vec<Node>>,
}

impl PickerSession {
    pub fn new(purpose: PickerPurpose, source: &[Node], file_path: std::path::PathBuf) -> Self {
        Self {
            purpose,
            roots: source.iter().map(PickerNode::from_node).collect(),
            file_path,
            visible_paths: Vec::new(),
            pending_filtered: None,
        }
    }
}

/// Display row mirrored to the Slint `NodePickerRow` model. Kept
/// `Copy`-friendly (small) so flattening can rebuild the whole list
/// on every mutation without performance worry.
#[derive(Clone, Debug, PartialEq)]
pub struct PickerVisibleRow {
    pub text: String,
    pub depth: i32,
    pub is_folder: bool,
    pub has_children: bool,
    pub expanded: bool,
    pub color_hex: String,
    pub has_color: bool,
    pub check_state: i32,
    pub inject_kbd: bool,
}

/// Walk the (deep-cloned) picker tree top-to-bottom, skipping the
/// children of collapsed folders, and produce one display row per
/// visible node along with its tree path. Path lengths grow with
/// depth so they double as a stable identifier the click callbacks
/// can use to find the corresponding `PickerNode` again.
pub fn flatten(roots: &[PickerNode]) -> (Vec<PickerVisibleRow>, Vec<Vec<usize>>) {
    let mut rows = Vec::new();
    let mut paths = Vec::new();
    let mut path_buf: Vec<usize> = Vec::new();
    fn walk(
        nodes: &[PickerNode],
        depth: i32,
        path_buf: &mut Vec<usize>,
        rows: &mut Vec<PickerVisibleRow>,
        paths: &mut Vec<Vec<usize>>,
    ) {
        for (i, node) in nodes.iter().enumerate() {
            path_buf.push(i);
            rows.push(PickerVisibleRow {
                text: node.name.clone(),
                depth,
                is_folder: node.is_folder,
                has_children: !node.children.is_empty(),
                expanded: node.expanded,
                color_hex: node.color.clone().unwrap_or_default(),
                has_color: node.color.is_some(),
                check_state: node.checked.as_int(),
                inject_kbd: node.inject_kbd,
            });
            paths.push(path_buf.clone());
            if node.is_folder && node.expanded {
                walk(&node.children, depth + 1, path_buf, rows, paths);
            }
            path_buf.pop();
        }
    }
    walk(roots, 0, &mut path_buf, &mut rows, &mut paths);
    (rows, paths)
}

fn node_at_mut<'a>(roots: &'a mut [PickerNode], path: &[usize]) -> Option<&'a mut PickerNode> {
    let (first, rest) = path.split_first()?;
    let mut cur = roots.get_mut(*first)?;
    for idx in rest {
        cur = cur.children.get_mut(*idx)?;
    }
    Some(cur)
}

/// Recursively force every descendant to `state`. Skipping the
/// roll-up step here is intentional: callers always invoke
/// `recompute_roll_up` immediately after, which collapses the whole
/// dirty subtree in one pass instead of visiting nodes twice.
fn apply_state_recursive(node: &mut PickerNode, state: PickerCheck) {
    node.checked = state;
    for child in &mut node.children {
        apply_state_recursive(child, state);
    }
}

/// Bubble up the (children -> parent) tristate logic through every
/// folder. Snippets terminate the recursion because they only ever
/// hold Checked or Unchecked. Folders with zero children behave as
/// leaves and keep whatever the user explicitly set.
fn recompute_roll_up(roots: &mut [PickerNode]) {
    fn walk(node: &mut PickerNode) {
        if !node.is_folder {
            return;
        }
        for child in &mut node.children {
            walk(child);
        }
        if node.children.is_empty() {
            return;
        }
        let mut all_checked = true;
        let mut all_unchecked = true;
        for child in &node.children {
            match child.checked {
                PickerCheck::Checked => all_unchecked = false,
                PickerCheck::Unchecked => all_checked = false,
                PickerCheck::Partial => {
                    all_checked = false;
                    all_unchecked = false;
                }
            }
        }
        node.checked = if all_checked {
            PickerCheck::Checked
        } else if all_unchecked {
            PickerCheck::Unchecked
        } else {
            PickerCheck::Partial
        };
    }
    for root in roots {
        walk(root);
    }
}

/// Click on a row's checkbox. For folders, snap-partial-to-checked
/// matches the Python branch where partial parents toggle to fully
/// checked instead of fully unchecked (less surprising than
/// nuking the whole subtree).
pub fn toggle_check(roots: &mut [PickerNode], path: &[usize]) {
    let Some(node) = node_at_mut(roots, path) else {
        return;
    };
    let next = match node.checked {
        PickerCheck::Checked => PickerCheck::Unchecked,
        PickerCheck::Unchecked => PickerCheck::Checked,
        PickerCheck::Partial => PickerCheck::Checked,
    };
    if node.is_folder {
        apply_state_recursive(node, next);
    } else {
        node.checked = next;
    }
    recompute_roll_up(roots);
}

/// Toggle the expand chevron on a folder. Snippets never hold the
/// expanded state so calling this on a snippet path is a no-op.
pub fn toggle_expand(roots: &mut [PickerNode], path: &[usize]) {
    if let Some(node) = node_at_mut(roots, path) {
        if node.is_folder {
            node.expanded = !node.expanded;
        }
    }
}

/// "Select all" / "Deselect all" toolbar buttons.
pub fn set_all(roots: &mut [PickerNode], state: PickerCheck) {
    for root in roots.iter_mut() {
        apply_state_recursive(root, state);
    }
}

/// Count `(folders, snippets)` whose subtree contributes at least
/// one node to the final selection. Mirrors Python's
/// `_count_checked` so the on-screen summary stays in sync.
pub fn count_checked(roots: &[PickerNode]) -> (usize, usize) {
    fn walk(node: &PickerNode) -> (usize, usize) {
        let mut folders = 0usize;
        let mut snippets = 0usize;
        if node.is_folder {
            let mut child_folders = 0usize;
            let mut child_snippets = 0usize;
            for child in &node.children {
                let (f, s) = walk(child);
                child_folders += f;
                child_snippets += s;
            }
            let has_any_checked_child = child_folders + child_snippets > 0;
            let counts_self = match node.checked {
                PickerCheck::Checked => true,
                PickerCheck::Partial => has_any_checked_child,
                PickerCheck::Unchecked => false,
            };
            if counts_self {
                folders += 1;
            }
            folders += child_folders;
            snippets += child_snippets;
        } else if node.checked == PickerCheck::Checked {
            snippets += 1;
        }
        (folders, snippets)
    }
    let mut totals = (0usize, 0usize);
    for root in roots {
        let (f, s) = walk(root);
        totals.0 += f;
        totals.1 += s;
    }
    totals
}

/// Whether the OK button should accept the picker. Empty selection
/// disables it (no point exporting / importing zero items).
pub fn can_accept(roots: &[PickerNode]) -> bool {
    let (folders, snippets) = count_checked(roots);
    folders + snippets > 0
}

/// Project the picker tree back into a real `Node` tree, keeping
/// only checked snippets and folders that contain at least one
/// selected descendant. Folder metadata (id, color, shortcut,
/// match) is preserved on the projected copy.
///
/// Mirrors Python's `_filtered_node`: `Unchecked` -> drop,
/// `Partial` with empty children -> drop, otherwise keep with the
/// filtered children list.
pub fn build_filtered(roots: &[PickerNode]) -> Vec<Node> {
    fn walk(node: &PickerNode) -> Option<Node> {
        if !node.is_folder {
            if node.checked != PickerCheck::Checked {
                return None;
            }
            // Snippet metadata is round-tripped untouched — the
            // source clone in `node.source` still holds every
            // field (id, color, match rule, prompt-untranslated,
            // injection mode), so we just hand it back.
            return Some(node.source.clone());
        }
        if node.checked == PickerCheck::Unchecked {
            return None;
        }
        let mut kept_children = Vec::new();
        for child in &node.children {
            if let Some(c) = walk(child) {
                kept_children.push(c);
            }
        }
        if node.checked == PickerCheck::Partial && kept_children.is_empty() {
            return None;
        }
        // Re-project the folder so we replace its children with the
        // filtered list while preserving id/color/shortcut/match —
        // pulled from the (immutable) source clone.
        let Node::Folder(src) = &node.source else {
            return None;
        };
        Some(Node::Folder(Folder {
            id: src.id.clone(),
            name: src.name.clone(),
            children: kept_children,
            color: src.color.clone(),
            shortcut: src.shortcut.clone(),
            r#match: src.r#match.clone(),
        }))
    }

    let mut out = Vec::new();
    for root in roots {
        if let Some(n) = walk(root) {
            out.push(n);
        }
    }
    out
}

/// Format the `"N folder(s), M snippet(s) selected"` summary the
/// picker shows in its toolbar.
pub fn format_summary(roots: &[PickerNode]) -> String {
    let (f, s) = count_checked(roots);
    format!("{f} folder(s), {s} snippet(s) selected")
}

#[cfg(test)]
mod tests {
    use super::*;
    use poltergeist_core::models::{InjectionMode, Snippet};

    fn snippet(name: &str) -> Node {
        Node::Snippet(Snippet {
            id: format!("id-{name}"),
            name: name.to_string(),
            text: format!("body-{name}"),
            injection: Some(InjectionMode::Clipboard),
            prompt_untranslated_before_paste: true,
            color: None,
            r#match: None,
        })
    }

    fn folder(name: &str, children: Vec<Node>) -> Node {
        Node::Folder(Folder {
            id: format!("fid-{name}"),
            name: name.to_string(),
            children,
            color: None,
            shortcut: None,
            r#match: None,
        })
    }

    #[test]
    fn flatten_skips_collapsed_children() {
        let nodes = vec![folder("F1", vec![snippet("a"), snippet("b")]), snippet("c")];
        let mut session = PickerSession::new(
            PickerPurpose::ExportPersonal,
            &nodes,
            std::path::PathBuf::from("ignored.json"),
        );
        // Default expanded -> 4 rows
        let (rows, _paths) = flatten(&session.roots);
        assert_eq!(rows.len(), 4);
        // Collapse F1 -> 2 rows (F1 + c)
        session.roots[0].expanded = false;
        let (rows, _paths) = flatten(&session.roots);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].text, "F1");
        assert_eq!(rows[1].text, "c");
    }

    #[test]
    fn folder_uncheck_propagates_to_descendants() {
        let nodes = vec![folder("F1", vec![snippet("a"), snippet("b")])];
        let mut session = PickerSession::new(
            PickerPurpose::ExportPersonal,
            &nodes,
            std::path::PathBuf::from("ignored.json"),
        );
        toggle_check(&mut session.roots, &[0]);
        assert_eq!(session.roots[0].checked, PickerCheck::Unchecked);
        assert_eq!(session.roots[0].children[0].checked, PickerCheck::Unchecked);
        assert_eq!(session.roots[0].children[1].checked, PickerCheck::Unchecked);
    }

    #[test]
    fn child_uncheck_bubbles_to_partial_parent() {
        let nodes = vec![folder("F1", vec![snippet("a"), snippet("b")])];
        let mut session = PickerSession::new(
            PickerPurpose::ExportPersonal,
            &nodes,
            std::path::PathBuf::from("ignored.json"),
        );
        toggle_check(&mut session.roots, &[0, 0]);
        assert_eq!(session.roots[0].checked, PickerCheck::Partial);
        assert_eq!(session.roots[0].children[0].checked, PickerCheck::Unchecked);
        assert_eq!(session.roots[0].children[1].checked, PickerCheck::Checked);
    }

    #[test]
    fn partial_folder_toggles_to_fully_checked() {
        let nodes = vec![folder("F1", vec![snippet("a"), snippet("b")])];
        let mut session = PickerSession::new(
            PickerPurpose::ExportPersonal,
            &nodes,
            std::path::PathBuf::from("ignored.json"),
        );
        toggle_check(&mut session.roots, &[0, 0]);
        assert_eq!(session.roots[0].checked, PickerCheck::Partial);
        toggle_check(&mut session.roots, &[0]);
        assert_eq!(session.roots[0].checked, PickerCheck::Checked);
        assert!(session
            .roots[0]
            .children
            .iter()
            .all(|c| c.checked == PickerCheck::Checked));
    }

    #[test]
    fn build_filtered_drops_unchecked_subtrees() {
        let nodes = vec![
            folder(
                "F1",
                vec![snippet("a"), snippet("b"), folder("Sub", vec![snippet("c")])],
            ),
            snippet("d"),
        ];
        let mut session = PickerSession::new(
            PickerPurpose::ExportPersonal,
            &nodes,
            std::path::PathBuf::from("ignored.json"),
        );
        // Uncheck only `b`
        toggle_check(&mut session.roots, &[0, 1]);
        let filtered = build_filtered(&session.roots);
        assert_eq!(filtered.len(), 2);
        match &filtered[0] {
            Node::Folder(f) => {
                assert_eq!(f.name, "F1");
                // a, Sub (still has c)
                assert_eq!(f.children.len(), 2);
                assert!(matches!(f.children[0], Node::Snippet(ref s) if s.name == "a"));
                assert!(matches!(f.children[1], Node::Folder(ref s) if s.name == "Sub"));
            }
            _ => panic!("expected folder"),
        }
    }

    #[test]
    fn build_filtered_drops_partial_folder_with_no_kept_children() {
        let nodes = vec![folder("F1", vec![snippet("a"), snippet("b")])];
        let mut session = PickerSession::new(
            PickerPurpose::ExportPersonal,
            &nodes,
            std::path::PathBuf::from("ignored.json"),
        );
        // Uncheck both children -> folder becomes Unchecked, dropped
        toggle_check(&mut session.roots, &[0, 0]);
        toggle_check(&mut session.roots, &[0, 1]);
        assert_eq!(session.roots[0].checked, PickerCheck::Unchecked);
        let filtered = build_filtered(&session.roots);
        assert!(filtered.is_empty());
    }

    #[test]
    fn count_checked_summary() {
        let nodes = vec![
            folder("F1", vec![snippet("a"), snippet("b")]),
            folder("F2", vec![snippet("c")]),
            snippet("d"),
        ];
        let mut session = PickerSession::new(
            PickerPurpose::ExportPersonal,
            &nodes,
            std::path::PathBuf::from("ignored.json"),
        );
        let (folders, snippets) = count_checked(&session.roots);
        assert_eq!((folders, snippets), (2, 4));
        // Uncheck F2 wholesale -> 1 folder, 3 snippets
        toggle_check(&mut session.roots, &[1]);
        let (folders, snippets) = count_checked(&session.roots);
        assert_eq!((folders, snippets), (1, 3));
    }

    #[test]
    fn set_all_unchecked_disables_accept() {
        let nodes = vec![folder("F1", vec![snippet("a")])];
        let mut session = PickerSession::new(
            PickerPurpose::ExportPersonal,
            &nodes,
            std::path::PathBuf::from("ignored.json"),
        );
        set_all(&mut session.roots, PickerCheck::Unchecked);
        recompute_roll_up(&mut session.roots);
        assert!(!can_accept(&session.roots));
        set_all(&mut session.roots, PickerCheck::Checked);
        recompute_roll_up(&mut session.roots);
        assert!(can_accept(&session.roots));
    }
}
