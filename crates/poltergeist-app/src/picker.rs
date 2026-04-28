use poltergeist_core::models::{Folder, Node};

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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PickerPurpose {
    ExportPersonal,
    ExportTeam,
    ImportPersonal,
    ImportTeam,
}

impl PickerPurpose {
    #[allow(dead_code)]
    pub fn is_export(self) -> bool {
        matches!(self, Self::ExportPersonal | Self::ExportTeam)
    }
}

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

pub struct PickerSession {
    pub purpose: PickerPurpose,
    pub roots: Vec<PickerNode>,
    pub file_path: std::path::PathBuf,
    pub visible_paths: Vec<Vec<usize>>,
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

fn apply_state_recursive(node: &mut PickerNode, state: PickerCheck) {
    node.checked = state;
    for child in &mut node.children {
        apply_state_recursive(child, state);
    }
}

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

pub fn toggle_expand(roots: &mut [PickerNode], path: &[usize]) {
    if let Some(node) = node_at_mut(roots, path) {
        if node.is_folder {
            node.expanded = !node.expanded;
        }
    }
}

pub fn set_all(roots: &mut [PickerNode], state: PickerCheck) {
    for root in roots.iter_mut() {
        apply_state_recursive(root, state);
    }
}

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

pub fn can_accept(roots: &[PickerNode]) -> bool {
    let (folders, snippets) = count_checked(roots);
    folders + snippets > 0
}

pub fn build_filtered(roots: &[PickerNode]) -> Vec<Node> {
    fn walk(node: &PickerNode) -> Option<Node> {
        if !node.is_folder {
            if node.checked != PickerCheck::Checked {
                return None;
            }
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
        let (rows, _paths) = flatten(&session.roots);
        assert_eq!(rows.len(), 4);
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
        assert!(session.roots[0]
            .children
            .iter()
            .all(|c| c.checked == PickerCheck::Checked));
    }

    #[test]
    fn build_filtered_drops_unchecked_subtrees() {
        let nodes = vec![
            folder(
                "F1",
                vec![
                    snippet("a"),
                    snippet("b"),
                    folder("Sub", vec![snippet("c")]),
                ],
            ),
            snippet("d"),
        ];
        let mut session = PickerSession::new(
            PickerPurpose::ExportPersonal,
            &nodes,
            std::path::PathBuf::from("ignored.json"),
        );
        toggle_check(&mut session.roots, &[0, 1]);
        let filtered = build_filtered(&session.roots);
        assert_eq!(filtered.len(), 2);
        match &filtered[0] {
            Node::Folder(f) => {
                assert_eq!(f.name, "F1");
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
