mod i18n;
mod picker;

use anyhow::Result;
use arboard::Clipboard;
use poltergeist_core::context as context_svc;
use poltergeist_core::models::{
    match_rule_from_expr, match_rule_to_expr, Folder, InjectionMode, Node, PoltergeistConfig,
    Settings, Snippet, ThemeMode,
};
use poltergeist_core::tokens::{self, evaluate_match_rule};
use poltergeist_io::{
    config, database::DatabaseRegistry, team_pack, translation::TranslationService,
};
use poltergeist_platform_win::focus::{current_foreground, WindowHandle};
use poltergeist_platform_win::hotkeys::HotkeyManager;
use poltergeist_platform_win::injector::{
    inject, InjectParams, InjectionMode as PlatformInjectionMode,
};
use rfd::{MessageButtons, MessageDialog, MessageDialogResult, MessageLevel};
use slint::{
    CloseRequestResponse, Color, Global, LogicalSize, ModelRc, SharedString, Timer, TimerMode,
    VecModel,
};
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;
#[cfg(target_os = "windows")]
use tauri_winrt_notification::{IconCrop, Toast};
use tracing_subscriber::EnvFilter;
use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem};
use tray_icon::{
    Icon as TrayIconImage, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent,
    TrayIconId,
};

slint::include_modules!();

fn init_logging() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt().with_env_filter(filter).init();
}

/// Persists the main window's inner size (logical pixels, clamped to the Slint min size).
fn persist_main_window_geometry(win: &slint::Window, state: &RefCell<AppState>, base: &Path) {
    let sz = win.size();
    let scale = f64::from(win.scale_factor().max(1.0));
    let mut lw = f64::from(sz.width as f32) / scale;
    let mut lh = f64::from(sz.height as f32) / scale;
    lw = lw.max(860.0);
    lh = lh.max(560.0);
    let mut st = state.borrow_mut();
    st.cfg.settings.main_window_width = Some(lw);
    st.cfg.settings.main_window_height = Some(lh);
    let _ = config::save(base, &st.cfg);
}

fn base_dir() -> PathBuf {
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|v| v.to_path_buf()))
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));

    // Development convenience: when launched via `cargo run`, resolve base dir
    // to the workspace root (instead of `target/debug`) so assets/config are
    // picked up from the project folder.
    let maybe_target = exe_dir.parent();
    if exe_dir
        .file_name()
        .and_then(|v| v.to_str())
        .is_some_and(|name| matches!(name, "debug" | "release"))
        && maybe_target
            .and_then(|p| p.file_name())
            .and_then(|v| v.to_str())
            .is_some_and(|name| name == "target")
    {
        if let Some(workspace_root) = maybe_target.and_then(Path::parent) {
            return workspace_root.to_path_buf();
        }
    }

    exe_dir
}

fn collect_snippet_names(tree: &[Node], out: &mut Vec<String>) {
    for node in tree {
        match node {
            Node::Snippet(snippet) => out.push(snippet.name.clone()),
            Node::Folder(folder) => collect_snippet_names(&folder.children, out),
        }
    }
}

fn as_model(values: Vec<String>) -> ModelRc<SharedString> {
    ModelRc::new(VecModel::from(
        values
            .into_iter()
            .map(SharedString::from)
            .collect::<Vec<_>>(),
    ))
}

fn default_snippet() -> Snippet {
    Snippet {
        id: uuid::Uuid::new_v4().simple().to_string(),
        name: "New Snippet".to_string(),
        text: String::new(),
        injection: None,
        prompt_untranslated_before_paste: true,
        color: None,
        r#match: None,
    }
}

fn default_folder() -> Folder {
    Folder {
        id: uuid::Uuid::new_v4().simple().to_string(),
        name: "New Folder".to_string(),
        children: Vec::new(),
        color: None,
        shortcut: None,
        r#match: None,
    }
}

fn parse_color_hex(raw: &str) -> Option<Color> {
    let s = raw.trim().trim_start_matches('#');
    if s.len() != 6 && s.len() != 8 {
        return None;
    }
    let r = u8::from_str_radix(&s[0..2], 16).ok()?;
    let g = u8::from_str_radix(&s[2..4], 16).ok()?;
    let b = u8::from_str_radix(&s[4..6], 16).ok()?;
    let a = if s.len() == 8 {
        u8::from_str_radix(&s[6..8], 16).ok()?
    } else {
        255
    };
    Some(Color::from_argb_u8(a, r, g, b))
}

const DEFAULT_ACCENT_HEX: &str = "#5865f2";

fn default_accent_base_color() -> Color {
    parse_color_hex(DEFAULT_ACCENT_HEX).expect("default accent parses")
}

fn accent_hex_for_ui(settings: &Settings) -> String {
    settings
        .accent_color
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty() && parse_color_hex(s).is_some())
        .map(|s| {
            let t = s.trim_start_matches('#');
            format!("#{t}")
        })
        .unwrap_or_else(|| DEFAULT_ACCENT_HEX.to_string())
}

fn accent_base_color(settings: &Settings) -> Color {
    settings
        .accent_color
        .as_deref()
        .and_then(parse_color_hex)
        .unwrap_or_else(default_accent_base_color)
}

fn accent_color_option_from_picker_hex(raw: &str) -> Option<String> {
    let t = raw.trim();
    if t.is_empty() {
        return None;
    }
    let normalized = if t.starts_with('#') {
        t.to_string()
    } else {
        format!("#{t}")
    };
    if parse_color_hex(&normalized).is_none() {
        return None;
    }
    if normalized.eq_ignore_ascii_case(DEFAULT_ACCENT_HEX) {
        None
    } else {
        Some(normalized)
    }
}

fn derive_accent_family(base: Color, is_light: bool) -> (Color, Color, Color) {
    let p = base.to_argb_u8();
    let rf = f64::from(p.red);
    let gf = f64::from(p.green);
    let bf = f64::from(p.blue);
    let factor = if is_light { 0.78 } else { 0.72 };
    let hover = Color::from_argb_u8(
        0xff,
        (rf * factor).round() as u8,
        (gf * factor).round() as u8,
        (bf * factor).round() as u8,
    );
    let (mx, my, mz) = if is_light {
        (255.0_f64, 255.0_f64, 255.0_f64)
    } else {
        (20.0_f64, 22.0_f64, 28.0_f64)
    };
    let t = 0.62_f64;
    let lerp = |c: f64, m: f64| -> u8 { (c * (1.0 - t) + m * t).round().clamp(0.0, 255.0) as u8 };
    let soft = Color::from_argb_u8(0xff, lerp(rf, mx), lerp(gf, my), lerp(bf, mz));
    (base, hover, soft)
}

fn apply_accent_theme(window: &MainWindow, base: Color, is_light: bool) {
    let (accent, hover, soft) = derive_accent_family(base, is_light);
    Theme::get(window).set_accent(accent);
    Theme::get(window).set_accent_hover(hover);
    Theme::get(window).set_accent_soft(soft);
}

fn apply_accent_from_settings(window: &MainWindow, settings: &Settings, is_light: bool) {
    let base = accent_base_color(settings);
    apply_accent_theme(window, base, is_light);
}

fn sync_options_accent_fields(window: &MainWindow, settings: &Settings) {
    let hex = accent_hex_for_ui(settings);
    let preview = parse_color_hex(&hex).unwrap_or_else(default_accent_base_color);
    window.set_options_accent_hex(hex.into());
    window.set_options_accent_preview(preview);
}

/// Build a single Slint TreeRowData payload for either a folder or snippet.
///
/// Selection is applied later (after construction) so that we don't have to
/// thread the selection-index through the recursive walk.
#[allow(clippy::too_many_arguments)]
fn make_row(
    label: String,
    icon_glyph: String,
    icon_color: Color,
    is_brands: bool,
    indent: i32,
    is_folder: bool,
    is_folder_collapsed: bool,
    is_locked: bool,
    shortcut_text: String,
    node_color: Option<Color>,
) -> TreeRowData {
    TreeRowData {
        label: label.into(),
        icon_glyph: icon_glyph.into(),
        icon_color,
        icon_is_brands: is_brands,
        indent,
        is_folder,
        is_folder_collapsed,
        is_selected: false,
        is_locked,
        has_shortcut: !shortcut_text.is_empty(),
        shortcut_text: shortcut_text.into(),
        node_color: node_color.unwrap_or(Color::from_argb_u8(0, 0, 0, 0)),
        has_node_color: node_color.is_some(),
    }
}

/// Default folder/snippet icon tint when the node has no custom colour
/// (matches `Theme.text` in `main.slint`).
fn default_tree_icon_tint(is_light: bool) -> Color {
    if is_light {
        Color::from_argb_u8(0xff, 0x1d, 0x1f, 0x24)
    } else {
        Color::from_argb_u8(0xff, 0xe3, 0xe5, 0xe8)
    }
}

/// Preset node colours — same hex values as Python `COLOR_SWATCHES` in `ui/tree_widget.py`.
pub const COLOR_SWATCHES: &[(&str, &str)] = &[
    ("Red", "#E06C75"),
    ("Orange", "#E5A66C"),
    ("Yellow", "#D5B84A"),
    ("Green", "#98C379"),
    ("Teal", "#56B6C2"),
    ("Blue", "#61AFEF"),
    ("Purple", "#C678DD"),
    ("Pink", "#E58FB4"),
    ("Grey", "#9BA3AF"),
];

/// Preset swatches for the anchored colour popup, split into two rows
/// (clear swatch is Slint-only on the first row).
fn build_color_swatch_row_pair() -> (ModelRc<ColorSwatchRow>, ModelRc<ColorSwatchRow>) {
    let rows: Vec<ColorSwatchRow> = COLOR_SWATCHES
        .iter()
        .filter_map(|(label_key, hex)| {
            parse_color_hex(hex).map(|swatch| ColorSwatchRow {
                label: i18n::tr(label_key).into(),
                hex: (*hex).into(),
                swatch,
            })
        })
        .collect();
    if rows.is_empty() {
        let empty: ModelRc<ColorSwatchRow> = ModelRc::new(VecModel::from(Vec::new()));
        return (empty.clone(), empty);
    }
    // 4 + 5 for nine presets (clear swatch is separate in Slint).
    let mid = rows.len() / 2;
    (
        ModelRc::new(VecModel::from(rows[..mid].to_vec())),
        ModelRc::new(VecModel::from(rows[mid..].to_vec())),
    )
}

#[allow(clippy::too_many_arguments)]
fn flatten_tree(
    nodes: &[Node],
    depth: i32,
    prefix: &[usize],
    icons: &IconAssets,
    default_icon_tint: Color,
    default_injection: InjectionMode,
    collapsed: &HashSet<Vec<usize>>,
    paths: &mut Vec<Vec<usize>>,
    out: &mut Vec<TreeRowData>,
) {
    for (index, node) in nodes.iter().enumerate() {
        let mut path = prefix.to_vec();
        path.push(index);
        match node {
            Node::Folder(folder) => {
                let user_color = folder.color.as_deref().and_then(parse_color_hex);
                let icon_color = user_color.unwrap_or(default_icon_tint);
                let is_collapsed = collapsed.contains(&path);
                out.push(make_row(
                    folder.name.clone(),
                    icons.folder_glyph.clone(),
                    icon_color,
                    false,
                    depth,
                    true,
                    is_collapsed,
                    false,
                    folder.shortcut.clone().unwrap_or_default(),
                    user_color,
                ));
                paths.push(path.clone());
                if !is_collapsed {
                    flatten_tree(
                        &folder.children,
                        depth + 1,
                        &path,
                        icons,
                        default_icon_tint,
                        default_injection,
                        collapsed,
                        paths,
                        out,
                    );
                }
            }
            Node::Snippet(snippet) => {
                let user_color = snippet.color.as_deref().and_then(parse_color_hex);
                let icon_color = user_color.unwrap_or(default_icon_tint);
                // Mirror Python `_snippet_icon`: typing-mode snippets get
                // the keyboard glyph, every other mode (clipboard,
                // shift+ins, typing_compat) gets the clipboard glyph.
                let effective_mode = snippet.injection.unwrap_or(default_injection);
                let glyph = if effective_mode == InjectionMode::Typing {
                    icons.keyboard_glyph.clone()
                } else {
                    icons.snippet_glyph.clone()
                };
                out.push(make_row(
                    snippet.name.clone(),
                    glyph,
                    icon_color,
                    false,
                    depth,
                    false,
                    false,
                    false,
                    String::new(),
                    user_color,
                ));
                paths.push(path);
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn flatten_team_tree(
    nodes: &[Node],
    depth: i32,
    prefix: &[usize],
    team_shortcuts: &HashMap<String, String>,
    icons: &IconAssets,
    default_icon_tint: Color,
    default_injection: InjectionMode,
    collapsed: &HashSet<Vec<usize>>,
    paths: &mut Vec<Vec<usize>>,
    out: &mut Vec<TreeRowData>,
) {
    for (index, node) in nodes.iter().enumerate() {
        let mut path = prefix.to_vec();
        path.push(index);
        match node {
            Node::Folder(folder) => {
                let effective_shortcut = team_shortcuts
                    .get(&folder.id)
                    .cloned()
                    .or_else(|| folder.shortcut.clone());
                let is_locked = effective_shortcut.is_none();
                let user_color = folder.color.as_deref().and_then(parse_color_hex);
                let icon_color = user_color.unwrap_or(if is_locked {
                    icons.team_locked_color
                } else {
                    default_icon_tint
                });
                let is_collapsed = collapsed.contains(&path);
                out.push(make_row(
                    folder.name.clone(),
                    icons.folder_glyph.clone(),
                    icon_color,
                    false,
                    depth,
                    true,
                    is_collapsed,
                    is_locked,
                    effective_shortcut.unwrap_or_default(),
                    user_color,
                ));
                paths.push(path.clone());
                if !is_collapsed {
                    flatten_team_tree(
                        &folder.children,
                        depth + 1,
                        &path,
                        team_shortcuts,
                        icons,
                        default_icon_tint,
                        default_injection,
                        collapsed,
                        paths,
                        out,
                    );
                }
            }
            Node::Snippet(snippet) => {
                let user_color = snippet.color.as_deref().and_then(parse_color_hex);
                let icon_color = user_color.unwrap_or(default_icon_tint);
                let effective_mode = snippet.injection.unwrap_or(default_injection);
                let glyph = if effective_mode == InjectionMode::Typing {
                    icons.keyboard_glyph.clone()
                } else {
                    icons.snippet_glyph.clone()
                };
                out.push(make_row(
                    snippet.name.clone(),
                    glyph,
                    icon_color,
                    false,
                    depth,
                    false,
                    false,
                    false,
                    String::new(),
                    user_color,
                ));
                paths.push(path);
            }
        }
    }
}

fn get_node_ref<'a>(nodes: &'a [Node], path: &[usize]) -> Option<&'a Node> {
    let (first, rest) = path.split_first()?;
    let node = nodes.get(*first)?;
    if rest.is_empty() {
        return Some(node);
    }
    match node {
        Node::Folder(folder) => get_node_ref(&folder.children, rest),
        Node::Snippet(_) => None,
    }
}

fn get_node_mut<'a>(nodes: &'a mut [Node], path: &[usize]) -> Option<&'a mut Node> {
    let (first, rest) = path.split_first()?;
    let node = nodes.get_mut(*first)?;
    if rest.is_empty() {
        return Some(node);
    }
    match node {
        Node::Folder(folder) => get_node_mut(&mut folder.children, rest),
        Node::Snippet(_) => None,
    }
}

fn remove_node_by_path(nodes: &mut Vec<Node>, path: &[usize]) -> bool {
    let (first, rest) = match path.split_first() {
        Some(v) => v,
        None => return false,
    };
    if rest.is_empty() {
        if *first < nodes.len() {
            nodes.remove(*first);
            return true;
        }
        return false;
    }
    if let Some(Node::Folder(folder)) = nodes.get_mut(*first) {
        return remove_node_by_path(&mut folder.children, rest);
    }
    false
}

/// Detach the node at `path` from `nodes` and return it.
///
/// Walks the same way as `remove_node_by_path` but yields the removed
/// node so the caller can reinsert it elsewhere (used for drag-and-drop
/// reordering / reparenting).
fn take_node_at_path(nodes: &mut Vec<Node>, path: &[usize]) -> Option<Node> {
    let (first, rest) = path.split_first()?;
    if rest.is_empty() {
        if *first < nodes.len() {
            return Some(nodes.remove(*first));
        }
        return None;
    }
    if let Some(Node::Folder(folder)) = nodes.get_mut(*first) {
        return take_node_at_path(&mut folder.children, rest);
    }
    None
}

/// Insert `node` so it ends up at `path` (i.e. as the `path.last()`-th
/// child of the parent identified by everything before `path.last()`).
fn insert_node_at_path(nodes: &mut Vec<Node>, path: &[usize], node: Node) -> bool {
    let (last, parent_path) = match path.split_last() {
        Some(v) => v,
        None => return false,
    };
    if parent_path.is_empty() {
        let idx = (*last).min(nodes.len());
        nodes.insert(idx, node);
        return true;
    }
    let parent = match get_node_mut(nodes, parent_path) {
        Some(Node::Folder(folder)) => folder,
        _ => return false,
    };
    let idx = (*last).min(parent.children.len());
    parent.children.insert(idx, node);
    true
}

/// True iff `inner` starts with every element of `outer` (i.e. `inner`
/// lives at or below `outer` in the tree). Used to forbid moving a
/// folder into one of its own descendants.
fn path_contains(outer: &[usize], inner: &[usize]) -> bool {
    inner.starts_with(outer)
}

/// Compute the destination path for a drag-and-drop move.
///
/// `src_path` is the original tree path of the dragged node. `target_path`
/// is the path of the row the user dropped on. When `into_folder` is
/// true and the target is a folder, the node becomes that folder's last
/// child; otherwise it is reordered to sit immediately above the target
/// at the target's depth.
///
/// Returns `None` if the move would be illegal (drop into self/descendant
/// or no-op).
fn compute_move_destination(
    src_path: &[usize],
    target_path: &[usize],
    into_folder: bool,
    target_is_folder: bool,
) -> Option<Vec<usize>> {
    if src_path == target_path {
        return None;
    }
    if path_contains(src_path, target_path) {
        return None;
    }
    let dest = if into_folder && target_is_folder {
        // Append into the target folder.
        let mut p = target_path.to_vec();
        // We don't know the exact child count at compute-time so use
        // u32::MAX-ish; the inserter clamps to the actual length.
        p.push(usize::MAX);
        p
    } else {
        target_path.to_vec()
    };
    Some(dest)
}

/// Perform a tree move: take from `src_path`, insert at `dest_path`.
///
/// Removing the source shifts every *sibling of src* (i.e. nodes with
/// the same parent and a greater child-index) down by one. That shift
/// propagates into `dest_path` only when `dest` sits in the same parent
/// as `src` AND the dest child-index at that depth is greater than the
/// src child-index. Critically, removing a child does NOT shift indices
/// at any other depth — earlier code that adjusted at the longest common
/// prefix would, for example, drag `[0,0]` (child of folder A) onto
/// folder B at top-level `[1]`, see "src[0]=0 < dest[0]=1", subtract 1,
/// and then incorrectly drop into A instead of B.
fn move_node_in_tree(nodes: &mut Vec<Node>, src_path: &[usize], mut dest_path: Vec<usize>) -> bool {
    if src_path.is_empty() || dest_path.is_empty() {
        return false;
    }
    // Reject moving into src's own subtree (dest is at or below src).
    if dest_path.starts_with(src_path) {
        return false;
    }
    // Apply the shift only when dest passes through src's parent at the
    // same depth as src and lands at a later sibling slot.
    let src_depth = src_path.len();
    if dest_path.len() >= src_depth {
        let src_parent = &src_path[..src_depth - 1];
        let dest_prefix = &dest_path[..src_depth - 1];
        if src_parent == dest_prefix {
            let src_idx = src_path[src_depth - 1];
            let dest_idx = dest_path[src_depth - 1];
            if dest_idx != usize::MAX && dest_idx > src_idx {
                dest_path[src_depth - 1] -= 1;
            }
        }
    }
    let Some(node) = take_node_at_path(nodes, src_path) else {
        return false;
    };
    insert_node_at_path(nodes, &dest_path, node)
}

fn add_under_selected_or_root(
    personal_tree: &mut Vec<Node>,
    selected_path: Option<&[usize]>,
    new_node: Node,
) {
    if let Some(path) = selected_path {
        if let Some(Node::Folder(folder)) = get_node_mut(personal_tree, path) {
            folder.children.push(new_node);
            return;
        }
    }
    personal_tree.push(new_node);
}

fn rows_model(rows: Vec<TreeRowData>) -> ModelRc<TreeRowData> {
    ModelRc::new(VecModel::from(rows))
}

fn chips_model(chips: Vec<TokenChip>) -> ModelRc<TokenChip> {
    ModelRc::new(VecModel::from(chips))
}

fn empty_chips_model() -> ModelRc<TokenChip> {
    ModelRc::new(VecModel::from(Vec::<TokenChip>::new()))
}

/// Convert a flattened picker tree into the `NodePickerRow` model
/// the Slint side renders. `paths` is published back onto the
/// session so toggle/expand callbacks can resolve flat indices to
/// tree paths without re-walking.
fn picker_rows_model(rows: Vec<picker::PickerVisibleRow>) -> ModelRc<NodePickerRow> {
    let converted: Vec<NodePickerRow> = rows
        .into_iter()
        .map(|r| NodePickerRow {
            text: r.text.into(),
            depth: r.depth,
            is_folder: r.is_folder,
            has_children: r.has_children,
            expanded: r.expanded,
            color_hex: r.color_hex.into(),
            has_color: r.has_color,
            check_state: r.check_state,
            inject_kbd: r.inject_kbd,
        })
        .collect();
    ModelRc::new(VecModel::from(converted))
}

fn empty_picker_rows_model() -> ModelRc<NodePickerRow> {
    ModelRc::new(VecModel::from(Vec::<NodePickerRow>::new()))
}

/// Re-flatten the picker session and push the resulting rows + path
/// list back to both the UI and the session. Callers invoke this
/// after every mutation (toggle / expand / select-all) so the rest
/// of the picker plumbing only has to touch `roots`.
fn refresh_picker_view(window: &MainWindow, st: &mut AppState) {
    let Some(session) = st.picker_session.as_mut() else {
        window.set_picker_rows(empty_picker_rows_model());
        window.set_picker_summary(SharedString::new());
        window.set_picker_can_accept(false);
        return;
    };
    let (rows, paths) = picker::flatten(&session.roots);
    session.visible_paths = paths;
    window.set_picker_rows(picker_rows_model(rows));
    window.set_picker_summary(picker::format_summary(&session.roots).into());
    window.set_picker_can_accept(picker::can_accept(&session.roots));
}

/// Open the picker modal with the given session. Sets every header
/// string so the same UI block serves both export and import flows.
fn show_picker(
    window: &MainWindow,
    st: &mut AppState,
    session: picker::PickerSession,
    title: &str,
    subtitle: &str,
    ok_label: &str,
) {
    st.picker_session = Some(session);
    // Translate label text right at the boundary between Rust and the
    // UI so callers can keep using English source strings (which lets
    // grep/IDE search work the same way it does on the Python side).
    window.set_picker_title(i18n::tr(title).into());
    window.set_picker_subtitle(i18n::tr(subtitle).into());
    window.set_picker_ok_label(i18n::tr(ok_label).into());
    refresh_picker_view(window, st);
    window.set_show_picker_panel(true);
}

/// Open the generic 3-way confirmation modal. `kind` is echoed back
/// to the `confirm_yes / confirm_no / confirm_cancel` callbacks so
/// the same modal can drive several workflows without us having to
/// bake a new property set per prompt.
fn show_confirm(
    window: &MainWindow,
    title: &str,
    message: &str,
    yes_label: &str,
    no_label: Option<&str>,
    cancel_label: Option<&str>,
    kind: &str,
) {
    window.set_confirm_title(i18n::tr(title).into());
    window.set_confirm_message(i18n::tr(message).into());
    window.set_confirm_yes_label(i18n::tr(yes_label).into());
    window.set_confirm_no_label(i18n::tr(no_label.unwrap_or("No")).into());
    window.set_confirm_cancel_label(i18n::tr(cancel_label.unwrap_or("Cancel")).into());
    window.set_confirm_show_no(no_label.is_some());
    window.set_confirm_show_cancel(cancel_label.is_some());
    window.set_confirm_kind(kind.into());
    window.set_show_confirm_panel(true);
}

/// DeepL source-language list, ported verbatim from
/// `ui/translation_picker.py::SOURCE_LANGUAGES`. The "Auto-detect"
/// entry is prepended in the UI side (so the indices used by Slint
/// are `0 = auto`, `1+ = this list`).
const TRANSLATION_SOURCE_LANGS: &[(&str, &str)] = &[
    ("BG", "Bulgarian"),
    ("CS", "Czech"),
    ("DA", "Danish"),
    ("DE", "German"),
    ("EL", "Greek"),
    ("EN", "English"),
    ("ES", "Spanish"),
    ("ET", "Estonian"),
    ("FI", "Finnish"),
    ("FR", "French"),
    ("HU", "Hungarian"),
    ("ID", "Indonesian"),
    ("IT", "Italian"),
    ("JA", "Japanese"),
    ("KO", "Korean"),
    ("LT", "Lithuanian"),
    ("LV", "Latvian"),
    ("NB", "Norwegian"),
    ("NL", "Dutch"),
    ("PL", "Polish"),
    ("PT", "Portuguese"),
    ("RO", "Romanian"),
    ("RU", "Russian"),
    ("SK", "Slovak"),
    ("SL", "Slovenian"),
    ("SV", "Swedish"),
    ("TR", "Turkish"),
    ("UK", "Ukrainian"),
    ("ZH", "Chinese"),
];

/// DeepL target-language list, ported verbatim from
/// `ui/translation_picker.py::TARGET_LANGUAGES`.
const TRANSLATION_TARGET_LANGS: &[(&str, &str)] = &[
    ("EN-US", "English (US)"),
    ("EN-GB", "English (UK)"),
    ("DE", "German"),
    ("FR", "French"),
    ("ES", "Spanish"),
    ("IT", "Italian"),
    ("NL", "Dutch"),
    ("PT-PT", "Portuguese"),
    ("PT-BR", "Portuguese (Brazil)"),
    ("PL", "Polish"),
    ("RU", "Russian"),
    ("JA", "Japanese"),
    ("ZH", "Chinese"),
    ("BG", "Bulgarian"),
    ("CS", "Czech"),
    ("DA", "Danish"),
    ("EL", "Greek"),
    ("ET", "Estonian"),
    ("FI", "Finnish"),
    ("HU", "Hungarian"),
    ("ID", "Indonesian"),
    ("KO", "Korean"),
    ("LT", "Lithuanian"),
    ("LV", "Latvian"),
    ("NB", "Norwegian"),
    ("RO", "Romanian"),
    ("SK", "Slovak"),
    ("SL", "Slovenian"),
    ("SV", "Swedish"),
    ("TR", "Turkish"),
    ("UK", "Ukrainian"),
];

/// Build the DeepL `{TRANSLATION=...}{TRANSLATION_END}` token from
/// the picker's two indices. `source_idx == 0` is the synthetic
/// "Auto-detect" row inserted at the head of the source combo, so
/// any non-zero index references `TRANSLATION_SOURCE_LANGS[idx-1]`.
/// Returns `None` for out-of-bounds indices.
fn build_translation_pair_token(source_idx: i32, target_idx: i32) -> Option<String> {
    let target_code = TRANSLATION_TARGET_LANGS
        .get(target_idx as usize)
        .map(|(code, _)| (*code).to_string())?;
    let token = if source_idx <= 0 {
        format!("{{TRANSLATION={target_code}}}{{TRANSLATION_END}}")
    } else {
        let src_code = TRANSLATION_SOURCE_LANGS
            .get((source_idx - 1) as usize)
            .map(|(code, _)| (*code).to_string())?;
        format!("{{TRANSLATION={src_code}>{target_code}}}{{TRANSLATION_END}}")
    };
    Some(token)
}

/// Best-effort port of `ui/snippet_highlighter.py`'s rule set. Slint's
/// TextEdit can't be styled inline, so the editor surfaces these as a
/// chip strip beneath the body. Categories must stay 1:1 with the
/// Python file so the visual language carries between the two builds.
///
/// Rules are evaluated in priority order; if two matches overlap the
/// earlier (longer-prefix) match wins and the later match is skipped,
/// matching Qt's `setFormat` overwriting semantics where the *first*
/// rule that touches a character takes effect for our chip view (the
/// chip strip is line-flat so we can't represent overlap visually
/// anyway).
fn extract_token_chips(body: &str) -> Vec<TokenChip> {
    use regex::{Regex, RegexBuilder};
    use std::sync::OnceLock;

    if body.is_empty() {
        return Vec::new();
    }

    static RULES: OnceLock<Vec<(Regex, &'static str)>> = OnceLock::new();
    let rules = RULES.get_or_init(|| {
        let ci = |pat: &str| {
            RegexBuilder::new(pat)
                .case_insensitive(true)
                .build()
                .expect("token highlighter regex must compile")
        };
        // Same operator alternation as Python's `_OP`.
        let op = r"(?:==|=|!=|<>|not\s+in|!in|\bin\b|contains|matches|startswith|endswith)";
        let if_pat = format!(
            r"\{{\s*IF\s+[A-Za-z_][A-Za-z0-9_]*\s*{op}\??\s*[^{{}}]*\s*\}}"
        );
        let elsif_pat = format!(
            r"\{{\s*(?:ELSIF|ELIF|ELSEIF)\s+[A-Za-z_][A-Za-z0-9_]*\s*{op}\??\s*[^{{}}]*\s*\}}"
        );
        vec![
            (ci(r"\{\s*(?:DATE|CLIPBOARD)(?:\s*[:=]\s*[^{}]*)?\s*\}"), "date_clip"),
            (ci(r"\{\s*WAIT(?:\s*[:=]\s*[^{}]*)?\s*\}"), "wait"),
            (ci(r"\{\s*(?:TAB|ENTER|RETURN)(?:\s*[:=]\s*\d+)?\s*\}"), "key"),
            (
                ci(concat!(
                    r"\{\s*",
                    r"(?:",
                    r"(?:CTRL|CONTROL|ALT|SHIFT|WIN|WINDOWS|META|CMD|SUPER)",
                    r"(?:\s*\+\s*(?:CTRL|CONTROL|ALT|SHIFT|WIN|WINDOWS|META|CMD|SUPER",
                    r"|DEL|DELETE|ESC|ESCAPE|BACKSPACE|BKSP|SPACE|TAB|ENTER|RETURN",
                    r"|HOME|END|UP|DOWN|LEFT|RIGHT|PAGEUP|PGUP|PAGEDOWN|PGDN",
                    r"|INSERT|INS|CAPS|CAPSLOCK|F\d{1,2}|[A-Z0-9]))*",
                    r"|",
                    r"(?:DEL|DELETE|ESC|ESCAPE|BACKSPACE|BKSP|SPACE|HOME|END|UP|DOWN|LEFT|RIGHT",
                    r"|PAGEUP|PGUP|PAGEDOWN|PGDN|INSERT|INS|CAPS|CAPSLOCK|F\d{1,2})",
                    r")",
                    r"(?:\s*[:=]\s*\d+)?",
                    r"\s*\}",
                )),
                "key",
            ),
            (ci(r"\{\s*VAR\s*=\s*[^{}]+\s*\}"), "var_include"),
            (ci(r"\{\s*INCLUDE\s*[:=]\s*[^{}]+\s*\}"), "var_include"),
            (ci(r"\{\s*DATABASE\s*[:=]\s*[^{}]*\s*\}"), "database"),
            (ci(&if_pat), "branch"),
            (ci(&elsif_pat), "branch"),
            (ci(r"\{\s*ELSE\s*\}"), "branch"),
            (ci(r"\{\s*END\s*\}"), "branch"),
            (
                ci(r"\{\s*TRANSLATION(?:\s*[:=]\s*(?:[A-Za-z]{2}>)?[A-Za-z]{2}(?:-[A-Za-z]{2})?)?\s*\}"),
                "translation",
            ),
            (ci(r"\{\s*TRANSLATION_END\s*\}"), "translation"),
        ]
    });

    struct Hit {
        start: usize,
        end: usize,
        text: String,
        category: &'static str,
        // Index of the rule that produced this hit. Higher = later in
        // the rule list = wins on ties, mirroring Qt's `setFormat`
        // overwrite semantics in the Python highlighter (e.g. `{END}`
        // is matched by both the KEY rule and the BRANCH rule; Python
        // ends up branch-styled because BRANCH is later in the list).
        rule_idx: usize,
    }

    let mut hits: Vec<Hit> = Vec::new();
    for (rule_idx, (re, cat)) in rules.iter().enumerate() {
        for m in re.find_iter(body) {
            hits.push(Hit {
                start: m.start(),
                end: m.end(),
                text: m.as_str().to_string(),
                category: cat,
                rule_idx,
            });
        }
    }
    // Sort: earliest-start first (document order), then longest match
    // (so a `{IF foo == 'x'}` beats a stray `{END}` substring inside),
    // then higher rule-index (so later rules overwrite earlier ones on
    // exact-range ties — Python `setFormat` semantics).
    hits.sort_by(|a, b| {
        a.start
            .cmp(&b.start)
            .then(b.end.cmp(&a.end))
            .then(b.rule_idx.cmp(&a.rule_idx))
    });

    let mut emitted: Vec<TokenChip> = Vec::with_capacity(hits.len());
    let mut cursor: usize = 0;
    let mut last_start: Option<usize> = None;
    for hit in hits {
        // Same start as the previous chip means this is a tie that the
        // sort already resolved in favour of the higher-priority rule —
        // skip the duplicate so we emit one chip per range.
        if Some(hit.start) == last_start {
            continue;
        }
        if hit.start < cursor {
            continue;
        }
        cursor = hit.end;
        last_start = Some(hit.start);
        emitted.push(TokenChip {
            text: hit.text.into(),
            category: hit.category.into(),
        });
    }
    emitted
}

/// Convenience for `refresh_*_editor`: returns the chips for the body
/// of the currently selected snippet, or an empty vec for folders /
/// no-selection. Keeps the call site one-liner.
fn chips_for_node(node: Option<&Node>) -> Vec<TokenChip> {
    match node {
        Some(Node::Snippet(s)) => extract_token_chips(&s.text),
        _ => Vec::new(),
    }
}

/// Assemble a parameterised token from the popup's input page. `kind`
/// matches the strings the Slint popup emits via
/// `build_and_insert_token`. Returns the formatted token on success or
/// a user-facing error message on validation failure (echoed into the
/// status bar). Intentionally permissive on whitespace so users can
/// paste with extra spaces and it still does the right thing — Python
/// behaves the same way via `str.strip()` in its dialog handlers.
fn build_token(kind: &str, value: &str) -> Result<String, String> {
    let trimmed = value.trim();
    match kind {
        "date" => {
            if trimmed.is_empty() {
                Ok("{DATE}".to_string())
            } else {
                // Python uses `{DATE:%fmt}` (colon, no `=`) for custom
                // formats — keep the punctuation identical so generated
                // snippets are interchangeable across editions.
                Ok(format!("{{DATE:{}}}", trimmed))
            }
        }
        "wait" => {
            let ms: u32 = trimmed.parse().map_err(|_| {
                format!("Wait token: '{}' is not a valid millisecond value", trimmed)
            })?;
            if ms > 60_000 {
                return Err("Wait token: max 60000ms".to_string());
            }
            Ok(format!("{{WAIT={}}}", ms))
        }
        "var" => {
            if trimmed.is_empty() {
                return Err("Variable token: name cannot be empty".to_string());
            }
            // Python doesn't validate the name shape; mirror that.
            Ok(format!("{{VAR={}}}", trimmed))
        }
        "database" => {
            if trimmed.is_empty() {
                return Err("Database token: lookup spec cannot be empty".to_string());
            }
            Ok(format!("{{DATABASE={}}}", trimmed))
        }
        "include" => {
            if trimmed.is_empty() {
                return Err("Include token: snippet name cannot be empty".to_string());
            }
            Ok(format!("{{INCLUDE={}}}", trimmed))
        }
        "custom_key" => {
            if trimmed.is_empty() {
                return Err("Key combo: combo cannot be empty".to_string());
            }
            // Translate the `keyboard`-package style ("ctrl+shift+a") that
            // HotkeyCapture and the user are most likely to type into the
            // brace token form ("{CTRL+SHIFT+A}") that the injector
            // expects. We rely on the existing token parser to validate
            // the shape downstream, so we just normalise casing here.
            let combo = trimmed
                .split('+')
                .map(|p| p.trim().to_uppercase())
                .filter(|p| !p.is_empty())
                .collect::<Vec<_>>()
                .join("+");
            if combo.is_empty() {
                return Err("Key combo: combo cannot be empty".to_string());
            }
            Ok(format!("{{{}}}", combo))
        }
        "custom_translation" => {
            if trimmed.is_empty() {
                return Err("Translation: target language code cannot be empty".to_string());
            }
            // DeepL codes are case-insensitive; uppercase keeps the
            // emitted token consistent with the one-click options.
            Ok(format!(
                "{{TRANSLATION={}}}{{TRANSLATION_END}}",
                trimmed.to_uppercase()
            ))
        }
        other => Err(format!("Unknown token kind: {}", other)),
    }
}

/// Resolve the `color` field of a node for the editor's swatch + text-input
/// pair. Returns (`text_for_input`, `parsed_color`, `has_color`). Invalid
/// hex strings are echoed back as text but the swatch falls back to "no
/// colour" so the user can fix the typo without losing it.
fn node_color_inputs(raw: Option<&str>) -> (String, Color, bool) {
    let text = raw.map(|s| s.to_string()).unwrap_or_default();
    let parsed = parse_color_hex(&text);
    let has_color = parsed.is_some();
    (
        text,
        parsed.unwrap_or(Color::from_argb_u8(0, 0, 0, 0)),
        has_color,
    )
}

fn node_color_str(node: &Node) -> Option<&str> {
    match node {
        Node::Folder(folder) => folder.color.as_deref(),
        Node::Snippet(snippet) => snippet.color.as_deref(),
    }
}

/// When the selected personal or team tree path changes, bump this so
/// Slint `TextInput` / `TextEdit` widgets resync from Rust-backed strings.
fn bump_editor_sync(window: &MainWindow, st: &mut AppState) {
    let cur_p = st
        .selected_personal
        .and_then(|i| st.personal_paths.get(i).cloned());
    let cur_t = st.selected_team.and_then(|i| st.team_paths.get(i).cloned());
    if cur_p != st.prev_personal_editor_path || cur_t != st.prev_team_editor_path {
        st.prev_personal_editor_path = cur_p;
        st.prev_team_editor_path = cur_t;
        st.editor_sync_version = st.editor_sync_version.wrapping_add(1);
    }
    window.set_editor_sync_version(st.editor_sync_version);
}

fn refresh_personal_editor(window: &MainWindow, st: &mut AppState) {
    let mut rows = Vec::new();
    let mut paths = Vec::new();
    let default_injection = st.cfg.settings.default_injection;
    let icon_tint = default_tree_icon_tint(st.is_light_theme);
    flatten_tree(
        &st.cfg.tree_personal,
        0,
        &[],
        &st.icons,
        icon_tint,
        default_injection,
        &st.personal_collapsed,
        &mut paths,
        &mut rows,
    );
    if let Some(idx) = st.selected_personal {
        if idx < rows.len() {
            rows[idx].is_selected = true;
        }
    }
    st.personal_paths = paths;
    window.set_personal_tree_rows(rows_model(rows));
    window.set_selected_personal_index(st.selected_personal.map(|i| i as i32).unwrap_or(-1));

    let selected_path = st
        .selected_personal
        .and_then(|idx| st.personal_paths.get(idx))
        .cloned();
    if let Some(path) = selected_path {
        if let Some(node) = get_node_ref(&st.cfg.tree_personal, &path) {
            let (color_text, color, has_color) = node_color_inputs(node_color_str(node));
            window.set_personal_tokens(chips_model(chips_for_node(Some(node))));
            match node {
                Node::Folder(folder) => {
                    window.set_selected_personal_kind("folder".into());
                    window.set_selected_personal_name(folder.name.clone().into());
                    window.set_selected_personal_text(String::new().into());
                    let sc = folder.shortcut.clone().unwrap_or_default();
                    window.set_selected_personal_shortcut(sc.clone().into());
                    window.set_selected_personal_match_expr(
                        match_rule_to_expr(folder.r#match.as_ref()).into(),
                    );
                    window.set_selected_personal_injection_index(0);
                    window.set_selected_personal_prompt_untranslated(true);
                    let hint = if sc.trim().is_empty() {
                        SharedString::new()
                    } else {
                        i18n::tr_format("Shortcut: {0}", &[&sc]).into()
                    };
                    window.set_editor_hint_text(hint);
                }
                Node::Snippet(snippet) => {
                    window.set_selected_personal_kind("snippet".into());
                    window.set_selected_personal_name(snippet.name.clone().into());
                    window.set_selected_personal_text(snippet.text.clone().into());
                    window.set_selected_personal_shortcut(String::new().into());
                    window.set_selected_personal_match_expr(
                        match_rule_to_expr(snippet.r#match.as_ref()).into(),
                    );
                    window.set_selected_personal_injection_index(snippet_injection_index(
                        snippet.injection,
                    ));
                    window.set_selected_personal_prompt_untranslated(
                        snippet.prompt_untranslated_before_paste,
                    );
                    window.set_editor_hint_text(SharedString::new());
                }
            }
            window.set_selected_personal_color_text(color_text.into());
            window.set_selected_personal_color(color);
            window.set_selected_personal_has_color(has_color);
            bump_editor_sync(window, st);
            return;
        }
    }
    st.selected_personal = None;
    window.set_selected_personal_kind("none".into());
    window.set_selected_personal_name(String::new().into());
    window.set_selected_personal_text(String::new().into());
    window.set_selected_personal_shortcut(String::new().into());
    window.set_selected_personal_match_expr(String::new().into());
    window.set_selected_personal_injection_index(0);
    window.set_selected_personal_prompt_untranslated(true);
    window.set_selected_personal_color_text(String::new().into());
    window.set_selected_personal_color(Color::from_argb_u8(0, 0, 0, 0));
    window.set_selected_personal_has_color(false);
    window.set_personal_tokens(empty_chips_model());
    window.set_editor_hint_text(SharedString::new());
    bump_editor_sync(window, st);
}

fn team_effective_shortcut(
    folder: &Folder,
    settings: &poltergeist_core::models::Settings,
) -> String {
    settings
        .team_shortcuts
        .get(&folder.id)
        .cloned()
        .or_else(|| folder.shortcut.clone())
        .unwrap_or_default()
}

fn refresh_team_editor(window: &MainWindow, st: &mut AppState) {
    let mut rows = Vec::new();
    let mut paths = Vec::new();
    let default_injection = st.cfg.settings.default_injection;
    let icon_tint = default_tree_icon_tint(st.is_light_theme);
    flatten_team_tree(
        &st.team_tree,
        0,
        &[],
        &st.cfg.settings.team_shortcuts,
        &st.icons,
        icon_tint,
        default_injection,
        &st.team_collapsed,
        &mut paths,
        &mut rows,
    );
    if let Some(idx) = st.selected_team {
        if idx < rows.len() {
            rows[idx].is_selected = true;
        }
    }
    st.team_paths = paths;
    window.set_team_tree_rows(rows_model(rows));
    window.set_selected_team_index(st.selected_team.map(|i| i as i32).unwrap_or(-1));

    let selected_path = st
        .selected_team
        .and_then(|idx| st.team_paths.get(idx))
        .cloned();
    if let Some(path) = selected_path {
        if let Some(node) = get_node_ref(&st.team_tree, &path) {
            let (color_text, color, has_color) = node_color_inputs(node_color_str(node));
            window.set_team_tokens(chips_model(chips_for_node(Some(node))));
            match node {
                Node::Folder(folder) => {
                    window.set_selected_team_kind("folder".into());
                    window.set_selected_team_name(folder.name.clone().into());
                    window.set_selected_team_text(String::new().into());
                    let sc = team_effective_shortcut(folder, &st.cfg.settings);
                    window.set_selected_team_shortcut(sc.clone().into());
                    window.set_selected_team_match_expr(
                        match_rule_to_expr(folder.r#match.as_ref()).into(),
                    );
                    let hint = if sc.trim().is_empty() {
                        SharedString::new()
                    } else {
                        i18n::tr_format("Shortcut: {0}", &[&sc]).into()
                    };
                    window.set_editor_hint_text(hint);
                }
                Node::Snippet(snippet) => {
                    window.set_selected_team_kind("snippet".into());
                    window.set_selected_team_name(snippet.name.clone().into());
                    window.set_selected_team_text(snippet.text.clone().into());
                    window.set_selected_team_shortcut(String::new().into());
                    window.set_selected_team_match_expr(
                        match_rule_to_expr(snippet.r#match.as_ref()).into(),
                    );
                    window.set_editor_hint_text(SharedString::new());
                }
            }
            window.set_selected_team_color_text(color_text.into());
            window.set_selected_team_color(color);
            window.set_selected_team_has_color(has_color);
            bump_editor_sync(window, st);
            return;
        }
    }
    st.selected_team = None;
    window.set_selected_team_kind("none".into());
    window.set_selected_team_name(String::new().into());
    window.set_selected_team_text(String::new().into());
    window.set_selected_team_shortcut(String::new().into());
    window.set_selected_team_match_expr(String::new().into());
    window.set_selected_team_color_text(String::new().into());
    window.set_selected_team_color(Color::from_argb_u8(0, 0, 0, 0));
    window.set_selected_team_has_color(false);
    window.set_team_tokens(empty_chips_model());
    window.set_editor_hint_text(SharedString::new());
    bump_editor_sync(window, st);
}

fn parse_import_tree(path: &Path) -> anyhow::Result<Vec<Node>> {
    let raw = fs::read_to_string(path)?;
    let data: serde_json::Value = serde_json::from_str(&raw)?;
    let maybe_tree = if let Some(tree) = data.get("tree") {
        tree.clone()
    } else {
        data
    };
    let nodes = serde_json::from_value::<Vec<Node>>(maybe_tree)?;
    Ok(nodes)
}

fn top_level_folder(nodes: &[Node], folder_id: &str) -> Option<Folder> {
    nodes.iter().find_map(|node| match node {
        Node::Folder(folder) if folder.id == folder_id => Some(folder.clone()),
        _ => None,
    })
}

/// Format a Slint key event into a `keyboard`/`global-hotkey` style combo.
///
/// Slint represents non-printable keys (arrows, F-keys, Home/End, etc.)
/// using Private-Use-Area codepoints in `event.text`. We map the most
/// common ones back to their lowercase names so the produced string
/// matches the format the rest of the app uses ("ctrl+alt+f1",
/// "ctrl+shift+space", "alt+a"). Returns "" when the event carried no
/// usable key (modifier-only or unmappable codepoint).
fn format_hotkey_event(text: &str, ctrl: bool, alt: bool, shift: bool, meta: bool) -> String {
    let Some(first) = text.chars().next() else {
        return String::new();
    };
    let key_name = key_event_text_to_name(first);
    let Some(key_name) = key_name else {
        return String::new();
    };

    let mut parts: Vec<&str> = Vec::with_capacity(5);
    if ctrl {
        parts.push("ctrl");
    }
    if alt {
        parts.push("alt");
    }
    if shift {
        parts.push("shift");
    }
    if meta {
        parts.push("windows");
    }
    parts.push(&key_name);
    parts.join("+")
}

/// Map a Slint key codepoint to the matching `keyboard`-package name.
/// `None` means the press was unmappable (and the caller should ignore
/// the event rather than emit a partial combo).
fn key_event_text_to_name(c: char) -> Option<String> {
    match c {
        '\u{0008}' => Some("backspace".into()),
        '\u{0009}' => Some("tab".into()),
        '\u{000a}' | '\u{000d}' => Some("enter".into()),
        '\u{001b}' => Some("esc".into()),
        '\u{007f}' => Some("delete".into()),
        ' ' => Some("space".into()),
        '\u{F700}' => Some("up".into()),
        '\u{F701}' => Some("down".into()),
        '\u{F702}' => Some("left".into()),
        '\u{F703}' => Some("right".into()),
        // F1..F24 occupy F704..F71B in Slint's Key enum. Anything past
        // F24 is exotic enough to ignore.
        '\u{F704}'..='\u{F71B}' => Some(format!("f{}", (c as u32 - 0xF704) + 1)),
        '\u{F727}' => Some("insert".into()),
        '\u{F729}' => Some("home".into()),
        '\u{F72B}' => Some("end".into()),
        '\u{F72C}' => Some("page up".into()),
        '\u{F72D}' => Some("page down".into()),
        // Printable ASCII: lowercased so "Shift+a" and "a" produce the
        // same key segment ("shift+a"), matching the Python parent.
        c if c.is_ascii_alphanumeric() => Some(c.to_ascii_lowercase().to_string()),
        // Punctuation that the OS hotkey hooks understand verbatim.
        c if c.is_ascii_graphic() => Some(c.to_string()),
        _ => None,
    }
}

fn normalize_hotkey(raw: &str) -> Option<String> {
    let normalized = raw.trim().to_ascii_lowercase();
    if normalized.is_empty() {
        None
    } else {
        Some(normalized)
    }
}

/// Per-snippet injection-mode picker order in the editor combobox.
/// Index 0 = "Default" (use the global setting). Order must match the labels
/// declared in `ui/main.slint` for the snippet ComboBox.
fn snippet_injection_index(mode: Option<InjectionMode>) -> i32 {
    match mode {
        None => 0,
        Some(InjectionMode::Clipboard) => 1,
        Some(InjectionMode::ClipboardShiftInsert) => 2,
        Some(InjectionMode::Typing) => 3,
        Some(InjectionMode::TypingCompat) => 4,
    }
}

fn snippet_injection_from_index(idx: i32) -> Option<InjectionMode> {
    match idx {
        1 => Some(InjectionMode::Clipboard),
        2 => Some(InjectionMode::ClipboardShiftInsert),
        3 => Some(InjectionMode::Typing),
        4 => Some(InjectionMode::TypingCompat),
        _ => None,
    }
}

/// Settings panel default-injection picker order (no "Default" entry —
/// these are real modes only).
fn default_injection_index(mode: InjectionMode) -> i32 {
    match mode {
        InjectionMode::Clipboard => 0,
        InjectionMode::ClipboardShiftInsert => 1,
        InjectionMode::Typing => 2,
        InjectionMode::TypingCompat => 3,
    }
}

fn default_injection_from_index(idx: i32) -> InjectionMode {
    match idx {
        1 => InjectionMode::ClipboardShiftInsert,
        2 => InjectionMode::Typing,
        3 => InjectionMode::TypingCompat,
        _ => InjectionMode::Clipboard,
    }
}

fn theme_index(mode: ThemeMode) -> i32 {
    match mode {
        ThemeMode::Auto => 0,
        ThemeMode::Light => 1,
        ThemeMode::Dark => 2,
    }
}

fn theme_from_index(idx: i32) -> ThemeMode {
    match idx {
        1 => ThemeMode::Light,
        2 => ThemeMode::Dark,
        _ => ThemeMode::Auto,
    }
}

/// Resolves the configured ThemeMode to an actual light/dark choice.
/// Auto consults the Windows AppsUseLightTheme registry value; on
/// non-Windows targets (or when the read fails) Auto falls back to
/// Dark to match the Python parent app's default.
fn effective_is_light(mode: ThemeMode) -> bool {
    match mode {
        ThemeMode::Light => true,
        ThemeMode::Dark => false,
        ThemeMode::Auto => {
            poltergeist_platform_win::theme::system_uses_light_theme().unwrap_or(false)
        }
    }
}

/// Font Awesome tree-row glyphs (FA7 Regular/Solid where noted). Kept in one
/// place so `flatten_tree` / `flatten_team_tree` stay allocation-light.
#[derive(Clone)]
struct IconAssets {
    folder_glyph: String,
    snippet_glyph: String,
    /// Typing-mode snippets use the keyboard glyph; other injection modes use
    /// the clipboard-style glyph (parity with the old Python `_snippet_icon`).
    keyboard_glyph: String,
    team_locked_color: Color,
}

impl Default for IconAssets {
    fn default() -> Self {
        Self {
            // f07c = folder-open (Regular)
            folder_glyph: '\u{f07c}'.to_string(),
            // f328 = clipboard-list (Regular)
            snippet_glyph: '\u{f328}'.to_string(),
            // f11c = keyboard (Regular)
            keyboard_glyph: '\u{f11c}'.to_string(),
            team_locked_color: Color::from_argb_u8(0xff, 0xb5, 0xba, 0xc1),
        }
    }
}

fn share_status_text(status: team_pack::ShareStatus, version: i64) -> String {
    // The version-bearing variants use placeholder substitution so a
    // single source string covers both the "(pack vN)" and bare cases
    // — translators only have to localise the prefix once.
    match status {
        team_pack::ShareStatus::Reachable => {
            if version > 0 {
                i18n::tr_format("Reachable (pack v{0})", &[&version])
            } else {
                i18n::tr("Reachable")
            }
        }
        team_pack::ShareStatus::Cached => {
            if version > 0 {
                i18n::tr_format("Share unreachable - using cache (pack v{0})", &[&version])
            } else {
                i18n::tr("Share unreachable - using cache")
            }
        }
        team_pack::ShareStatus::Unreachable => i18n::tr("Share unreachable and no cache"),
        team_pack::ShareStatus::Unconfigured => i18n::tr("No share configured"),
    }
}

/// 0 = unconfigured, 1 = reachable, 2 = cached, 3 = unreachable.
/// Mirrors the slint-side switch in the share-path status indicator.
fn share_status_kind(status: team_pack::ShareStatus) -> i32 {
    match status {
        team_pack::ShareStatus::Unconfigured => 0,
        team_pack::ShareStatus::Reachable => 1,
        team_pack::ShareStatus::Cached => 2,
        team_pack::ShareStatus::Unreachable => 3,
    }
}

/// Human-readable native names mirrored to Slint's language combo.
/// We match the Python build's SUPPORTED_LANGUAGES list so the labels
/// look the same on both ports.
const SUPPORTED_LANGUAGES: &[(&str, &str)] = &[
    ("en", "English"),
    ("de", "Deutsch"),
    ("es", "Español"),
    ("fr", "Français"),
];

fn language_index_from_code(code: &str) -> i32 {
    let normalized = code.trim().to_ascii_lowercase();
    if normalized.is_empty() {
        return 0;
    }
    SUPPORTED_LANGUAGES
        .iter()
        .position(|(lc, _)| *lc == normalized)
        .map(|p| p as i32)
        .unwrap_or(0)
}

fn language_code_from_index(idx: i32) -> String {
    let i = idx.max(0) as usize;
    SUPPORTED_LANGUAGES
        .get(i)
        .map(|(code, _)| (*code).to_string())
        .unwrap_or_else(|| "en".to_string())
}

fn available_languages_model() -> ModelRc<SharedString> {
    as_model(
        SUPPORTED_LANGUAGES
            .iter()
            .map(|(_, native)| (*native).to_string())
            .collect(),
    )
}

/// Switch the active translation locale for both the Slint `@tr(...)`
/// strings *and* the Rust-side `i18n::tr` catalog.
///
/// Slint flips translations live (no app restart) once you call this,
/// so the language picker in Options can take effect immediately —
/// matching how Python's `_apply_translations` would have to restart
/// the Qt application. We simply log on failure rather than aborting
/// because the supported list is hard-coded above and we'd rather keep
/// the app usable in English than crash on a stale config value.
fn apply_bundled_translation(code: &str) {
    let normalized = code.trim().to_ascii_lowercase();
    let target: &str = if normalized.is_empty() || normalized == "en" {
        ""
    } else {
        &normalized
    };
    // Keep the Rust-side lookup in lock-step with Slint so any
    // status-bar / dialog string we build in Rust uses the same locale
    // the UI is currently rendering with.
    i18n::set_locale(target);
    match slint::select_bundled_translation(target) {
        Ok(()) => {}
        Err(slint::SelectBundledTranslationError::NoTranslationsBundled) => {
            // Build was produced without `with_bundled_translations` —
            // not an error, just means we ship English only.
        }
        Err(slint::SelectBundledTranslationError::LanguageNotFound {
            available_languages,
        }) => {
            eprintln!(
                "i18n: requested locale '{}' not bundled. Available: {:?}",
                target, available_languages
            );
        }
    }
}

/// Apply a deepl status string to the Slint `deepl_status_kind` enum:
/// 1 = success, 2 = failure, 0 = unknown / not configured.
fn deepl_status_kind_from_msg(api_key: &str, ok: Option<bool>) -> i32 {
    if api_key.trim().is_empty() {
        return 0;
    }
    match ok {
        Some(true) => 1,
        Some(false) => 2,
        None => 0,
    }
}

/// Validate `key` against the DeepL `usage` endpoint and reflect the
/// result on the Options panel. Shared between the explicit "Validate"
/// button and the 700ms debounce timer triggered by typing.
///
/// `from_button` controls whether the result also tweaks the global
/// status bar (the button explicitly asked, so we surface success
/// loudly; the debounce path stays quiet on success and only
/// announces failures via the inline label).
fn run_deepl_validation(window: &MainWindow, key: &str, from_button: bool) {
    let key = key.trim();
    if key.is_empty() {
        window.set_deepl_status_text("No API key".into());
        window.set_deepl_status_kind(0);
        if from_button {
            window.set_status_text(i18n::tr("DeepL validate skipped: no API key").into());
        }
        return;
    }
    match TranslationService::new(key.to_string()) {
        Ok(service) => match service.validate() {
            Ok((ok, msg)) => {
                window.set_deepl_status_text(msg.clone().into());
                window.set_deepl_status_kind(deepl_status_kind_from_msg(key, Some(ok)));
                if from_button {
                    if ok {
                        window.set_status_text(i18n::tr("DeepL key validated").into());
                    } else {
                        window.set_status_text(
                            i18n::tr_format("DeepL validation failed: {0}", &[&msg]).into(),
                        );
                    }
                }
            }
            Err(err) => {
                window.set_deepl_status_text(format!("Validation error: {err}").into());
                window.set_deepl_status_kind(2);
                if from_button {
                    window.set_status_text(i18n::tr("DeepL validation failed").into());
                }
            }
        },
        Err(err) => {
            window.set_deepl_status_text(format!("DeepL setup failed: {err}").into());
            window.set_deepl_status_kind(2);
            if from_button {
                window.set_status_text(i18n::tr("DeepL validation failed").into());
            }
        }
    }
}

/// `chrono::Local::now().format(<fmt>)` with safety guards. Returns
/// "(invalid format)" on parser errors so the Options preview never
/// panics or surfaces a stack trace — matches Python's
/// `_update_date_preview` exception branch.
///
/// An empty `fmt` string falls back to `%d/%m/%Y` to mirror the
/// app-wide default applied during `Save settings`.
fn format_date_preview(fmt: &str) -> String {
    use chrono::format::StrftimeItems;
    let trimmed = fmt.trim();
    let effective = if trimmed.is_empty() {
        "%d/%m/%Y"
    } else {
        trimmed
    };
    // `parse_to_owned` validates the spec without rendering — invalid
    // tokens (e.g. `%Q`) bubble up as `ParseError` instead of
    // panicking inside the format machinery.
    let items = match StrftimeItems::new(effective).parse_to_owned() {
        Ok(items) => items,
        Err(_) => return "(invalid format)".to_string(),
    };
    chrono::Local::now()
        .format_with_items(items.as_slice().iter())
        .to_string()
}

/// Spawn the OS default-browser handler for `url`. No-op on errors —
/// the About modal links are non-critical UX and we don't want a
/// missing `xdg-open` to crash the app on a stripped-down Linux box.
fn open_url_in_browser(url: &str) {
    #[cfg(target_os = "windows")]
    {
        // `cmd /c start "" <url>` opens via the registered protocol
        // handler. The empty quoted string is the window title arg
        // that `start` requires when the next token might look like a
        // command path.
        let _ = std::process::Command::new("cmd")
            .args(["/c", "start", "", url])
            .spawn();
    }
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("open").arg(url).spawn();
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let _ = std::process::Command::new("xdg-open").arg(url).spawn();
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos", unix)))]
    {
        let _ = url;
    }
}

fn notify_tray(base_dir: &Path, title: &str, message: &str) {
    #[cfg(target_os = "windows")]
    {
        let mut toast = Toast::new(Toast::POWERSHELL_APP_ID)
            .title(title)
            .text1(message);
        let icon_path = base_dir.join("assets").join("AppIcon.ico");
        if icon_path.exists() {
            toast = toast.icon(&icon_path, IconCrop::Square, "Poltergeist");
        }
        let _ = toast.sound(None).show();
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = (base_dir, title, message);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Edition {
    User,
    Admin,
}

fn detect_edition(base_dir: &Path) -> Edition {
    #[cfg(feature = "admin-edition")]
    {
        let _ = base_dir;
        Edition::Admin
    }
    #[cfg(not(feature = "admin-edition"))]
    {
        let env = std::env::var("POLTERGEIST_EDITION")
            .ok()
            .map(|v| v.trim().to_ascii_lowercase());
        if matches!(env.as_deref(), Some("admin")) {
            return Edition::Admin;
        }
        if matches!(env.as_deref(), Some("user")) {
            return Edition::User;
        }
        if base_dir.join("_admin.flag").exists() {
            Edition::Admin
        } else {
            Edition::User
        }
    }
}

fn desired_hotkey_bindings(cfg: &PoltergeistConfig, team_tree: &[Node]) -> Vec<(String, String)> {
    let mut bindings = Vec::new();

    // Team folder shortcuts have highest precedence.
    for node in team_tree {
        if let Node::Folder(folder) = node {
            let hk = cfg
                .settings
                .team_shortcuts
                .get(&folder.id)
                .cloned()
                .or_else(|| folder.shortcut.clone());
            if let Some(hotkey) = hk.as_deref().and_then(normalize_hotkey) {
                bindings.push((format!("team:{}", folder.id), hotkey));
            }
        }
    }

    // Personal folder shortcuts are second.
    for node in &cfg.tree_personal {
        if let Node::Folder(folder) = node {
            if let Some(hotkey) = folder.shortcut.as_deref().and_then(normalize_hotkey) {
                bindings.push((format!("personal:{}", folder.id), hotkey));
            }
        }
    }

    // Main popup hotkey is fallback.
    if let Some(main_hotkey) = normalize_hotkey(&cfg.settings.hotkey) {
        bindings.push((String::from("main"), main_hotkey));
    }

    bindings
}

fn install_hotkeys(
    manager: &Rc<RefCell<Option<HotkeyManager>>>,
    cfg: &PoltergeistConfig,
    team_tree: &[Node],
) -> Option<HashMap<String, String>> {
    let mut guard = manager.borrow_mut();
    guard
        .as_mut()
        .map(|m| m.install(desired_hotkey_bindings(cfg, team_tree)))
}

fn open_popup_for_nodes(
    window: &MainWindow,
    snippet_popup: &SnippetPopup,
    st: &mut AppState,
    source_nodes: &[Node],
    source_label: &str,
) {
    st.current_context = capture_context(&st.cfg.settings.context_patterns);
    // Build a fresh navigation stack rooted at the requested source,
    // pruned by the current context (matches the Python QMenu's
    // _filter_nodes pass — empty folders and rejected snippets are
    // dropped entirely).
    let filtered = filter_nodes_for_popup(source_nodes, &st.current_context);
    st.popup_nav_stack = vec![filtered];
    refresh_popup_visible(snippet_popup, st);
    window.set_status_text(i18n::tr_format("Popup opened from {0}", &[&source_label]).into());
    snippet_popup.set_is_light(window.get_is_light_theme());
    position_popup_at_cursor(snippet_popup, st);
    let _ = snippet_popup.show();
}

/// Filter `nodes` against `context`, recursively pruning empty folders
/// and snippets whose match-rule rejects the current context. Mirrors
/// `popup_menu._filter_nodes` in the Python build so the picker doesn't
/// surface dead entries the user can't act on.
fn filter_nodes_for_popup(nodes: &[Node], context: &HashMap<String, String>) -> Vec<Node> {
    let mut out = Vec::new();
    for node in nodes {
        match node {
            Node::Snippet(snippet) => {
                if snippet_matches(snippet, context) {
                    out.push(Node::Snippet(snippet.clone()));
                }
            }
            Node::Folder(folder) => {
                if !folder_matches(folder, context) {
                    continue;
                }
                let kids = filter_nodes_for_popup(&folder.children, context);
                if kids.is_empty() {
                    continue;
                }
                let mut clone = folder.clone();
                clone.children = kids;
                out.push(Node::Folder(clone));
            }
        }
    }
    out
}

fn collect_popup_snippets(nodes: &[Node], prefix: &str, out: &mut Vec<(String, Snippet)>) {
    for node in nodes {
        match node {
            Node::Folder(folder) => {
                let folder_name = if folder.name.trim().is_empty() {
                    "Folder".to_string()
                } else {
                    folder.name.clone()
                };
                let next_prefix = if prefix.is_empty() {
                    folder_name
                } else {
                    format!("{prefix} / {folder_name}")
                };
                collect_popup_snippets(&folder.children, &next_prefix, out);
            }
            Node::Snippet(snippet) => {
                let snippet_name = if snippet.name.trim().is_empty() {
                    "Snippet".to_string()
                } else {
                    snippet.name.clone()
                };
                let label = if prefix.is_empty() {
                    snippet_name
                } else {
                    format!("{prefix} / {snippet_name}")
                };
                out.push((label, snippet.clone()));
            }
        }
    }
}

/// Recompute the popup's primary column (top-level folders + root snippets).
fn refresh_popup_visible(popup: &SnippetPopup, st: &mut AppState) {
    let mut main_items: Vec<PopupMainItem> = Vec::new();
    st.popup_main_kinds.clear();
    st.popup_sub_visible_kinds.clear();
    st.popup_visible_kinds.clear();
    if let Some(top) = st.popup_nav_stack.first() {
        for node in top {
            match node {
                Node::Folder(f) => {
                    let title = if f.name.trim().is_empty() {
                        i18n::tr("Folder").to_string()
                    } else {
                        f.name.clone()
                    };
                    main_items.push(PopupMainItem {
                        title: title.into(),
                        kind: 0,
                    });
                    st.popup_main_kinds.push(PopupTopKind::Folder(f.clone()));
                }
                Node::Snippet(s) => {
                    let title = if s.name.trim().is_empty() {
                        i18n::tr("Snippet").to_string()
                    } else {
                        s.name.clone()
                    };
                    main_items.push(PopupMainItem {
                        title: title.into(),
                        kind: 1,
                    });
                    st.popup_main_kinds.push(PopupTopKind::Snippet(s.clone()));
                }
            }
        }
    }
    popup.set_main_entries(ModelRc::new(VecModel::from(main_items)));
    popup.set_submenu_visible(false);
    popup.set_sub_entries(ModelRc::new(VecModel::from(Vec::<PopupItem>::new())));
    popup.set_open_folder_row(-1);
    st.popup_open_main_idx = None;
}

fn popup_fill_submenu_for_main_index(st: &mut AppState, main_idx: usize, popup: &SnippetPopup) {
    let Some(kind) = st.popup_main_kinds.get(main_idx) else {
        popup_clear_submenu(st, popup);
        return;
    };
    let PopupTopKind::Folder(folder) = kind else {
        popup_clear_submenu(st, popup);
        return;
    };
    let mut flattened: Vec<(String, Snippet)> = Vec::new();
    collect_popup_snippets(&folder.children, "", &mut flattened);
    let mut items: Vec<PopupItem> = Vec::new();
    let mut kinds: Vec<PopupVisibleKind> = Vec::new();
    for (label, snippet) in flattened {
        items.push(PopupItem {
            title: label.into(),
            kind: 2,
        });
        kinds.push(PopupVisibleKind::InjectSnippet(snippet));
    }
    let has_sub = !items.is_empty();
    st.popup_sub_visible_kinds = kinds;
    popup.set_sub_entries(ModelRc::new(VecModel::from(items)));
    popup.set_submenu_visible(has_sub);
    st.popup_open_main_idx = Some(main_idx);
    popup.set_open_folder_row(main_idx as i32);
}

fn popup_clear_submenu(st: &mut AppState, popup: &SnippetPopup) {
    st.popup_sub_visible_kinds.clear();
    st.popup_open_main_idx = None;
    popup.set_submenu_visible(false);
    popup.set_sub_entries(ModelRc::new(VecModel::from(Vec::<PopupItem>::new())));
    popup.set_open_folder_row(-1);
}

/// Place the popup at the OS cursor (clamped to the screen) and stash
/// its physical-pixel bounds in `AppState` so the click-outside watcher
/// can tell whether a stray mouse press belongs to us.
fn position_popup_at_cursor(popup: &SnippetPopup, st: &mut AppState) {
    if let Some((x, y)) = poltergeist_platform_win::cursor::position() {
        let pos_x = x.saturating_sub(8);
        let pos_y = y.saturating_sub(4);
        popup
            .window()
            .set_position(slint::PhysicalPosition::new(pos_x, pos_y));
        let scale = popup.window().scale_factor().max(1.0);
        let size = popup.window().size();
        // Window size in physical pixels — at first show this may be
        // the placeholder default; the watcher refreshes it the next
        // tick once Slint has laid the popup out.
        let w = size.width as i32;
        let h = if size.height == 0 {
            (460.0_f32 * scale) as i32
        } else {
            size.height as i32
        };
        st.popup_bounds = Some((pos_x, pos_y, w.max(1), h.max(1)));
    } else {
        st.popup_bounds = None;
    }
}

enum TrayAction {
    Open,
    Options,
    About,
    Pause(bool),
    Exit,
}

struct TrayRuntime {
    _tray: TrayIcon,
    tray_id: TrayIconId,
    open_id: MenuId,
    options_id: MenuId,
    about_id: MenuId,
    pause_id: MenuId,
    exit_id: MenuId,
    pause_item: CheckMenuItem,
}

impl TrayRuntime {
    fn new(base_dir: &Path, edition: Edition) -> Option<Self> {
        let menu = Menu::new();
        let open_item = MenuItem::new("Open Poltergeist", true, None);
        let options_item = MenuItem::new("Options...", true, None);
        let about_item = MenuItem::new("About", true, None);
        let pause_item = CheckMenuItem::new("Pause hotkey", true, false, None);
        let exit_item = MenuItem::new("Exit", true, None);

        menu.append(&open_item).ok()?;
        menu.append(&options_item).ok()?;
        menu.append(&about_item).ok()?;
        menu.append(&PredefinedMenuItem::separator()).ok()?;
        menu.append(&pause_item).ok()?;
        menu.append(&PredefinedMenuItem::separator()).ok()?;
        menu.append(&exit_item).ok()?;

        let builder = TrayIconBuilder::new()
            .with_tooltip("Poltergeist")
            .with_menu_on_left_click(false)
            .with_menu(Box::new(menu));
        let builder = if let Some(icon) = load_tray_icon(base_dir, edition) {
            builder.with_icon(icon)
        } else {
            builder
        };
        let tray = builder.build().ok()?;
        let tray_id = tray.id().clone();

        Some(Self {
            _tray: tray,
            tray_id,
            open_id: open_item.id().clone(),
            options_id: options_item.id().clone(),
            about_id: about_item.id().clone(),
            pause_id: pause_item.id().clone(),
            exit_id: exit_item.id().clone(),
            pause_item,
        })
    }

    fn set_paused(&self, paused: bool) {
        self.pause_item.set_checked(paused);
    }

    fn poll_action(&self) -> Option<TrayAction> {
        let mut latest = None;
        while let Ok(event) = TrayIconEvent::receiver().try_recv() {
            let is_own_icon = event.id() == &self.tray_id;
            if !is_own_icon {
                continue;
            }
            match event {
                TrayIconEvent::Click {
                    button,
                    button_state,
                    ..
                } if button == MouseButton::Left && button_state == MouseButtonState::Up => {
                    latest = Some(TrayAction::Open);
                }
                TrayIconEvent::DoubleClick {
                    button: MouseButton::Left,
                    ..
                } => {
                    latest = Some(TrayAction::Open);
                }
                _ => {}
            }
        }
        while let Ok(event) = MenuEvent::receiver().try_recv() {
            let action = if event.id == self.open_id {
                Some(TrayAction::Open)
            } else if event.id == self.options_id {
                Some(TrayAction::Options)
            } else if event.id == self.about_id {
                Some(TrayAction::About)
            } else if event.id == self.pause_id {
                Some(TrayAction::Pause(self.pause_item.is_checked()))
            } else if event.id == self.exit_id {
                Some(TrayAction::Exit)
            } else {
                None
            };
            if action.is_some() {
                latest = action;
            }
        }
        latest
    }
}

fn load_tray_icon(base_dir: &Path, edition: Edition) -> Option<TrayIconImage> {
    let icon_names = if edition == Edition::Admin {
        ["AppIconAdmin.ico", "AppIcon.ico"]
    } else {
        ["AppIcon.ico", "AppIconAdmin.ico"]
    };
    let dirs = [
        base_dir.join("assets"),
        base_dir.to_path_buf(),
        base_dir.join("target").join("debug").join("assets"),
    ];
    for dir in dirs {
        for icon_name in icon_names {
            let path = dir.join(icon_name);
            if path.exists() {
                if let Ok(icon) = TrayIconImage::from_path(path, None) {
                    return Some(icon);
                }
            }
        }
    }
    for icon_name in icon_names {
        let path = base_dir.join(icon_name);
        if path.exists() {
            if let Ok(icon) = TrayIconImage::from_path(path, None) {
                return Some(icon);
            }
        }
    }
    None
}

struct AppState {
    cfg: PoltergeistConfig,
    edition: Edition,
    team_tree: Vec<Node>,
    db_registry: DatabaseRegistry,
    /// Folder paths whose children are currently hidden in the personal
    /// tree. Lives only in memory (matches Python's QTreeWidget which
    /// `expandAll()`s on every full reload).
    personal_collapsed: HashSet<Vec<usize>>,
    team_collapsed: HashSet<Vec<usize>>,
    current_context: HashMap<String, String>,
    /// Flat path mapping for the personal tree, parallel to the visual rows.
    personal_paths: Vec<Vec<usize>>,
    selected_personal: Option<usize>,
    /// Same as `personal_paths` but for the team tree.
    team_paths: Vec<Vec<usize>>,
    selected_team: Option<usize>,
    team_manifest_version: i64,
    team_source: team_pack::ShareStatus,
    icons: IconAssets,
    /// Cached so that "Show light mode" toggles don't have to consult the
    /// settings every redraw — flips on toggle_theme() and during apply.
    is_light_theme: bool,
    /// Snippet picker navigation stack — the top entry is the
    /// list currently visible in the SnippetPopup window. Pushing
    /// a folder's filtered children drills in; popping goes back.
    popup_nav_stack: Vec<Vec<Node>>,
    /// Top-level rows in the snippet popup (folder vs root snippet).
    popup_main_kinds: Vec<PopupTopKind>,
    /// Snippets listed in the hover submenu for the active folder.
    popup_sub_visible_kinds: Vec<PopupVisibleKind>,
    /// Previous personal tree path used to bump Slint editor `sync-version`
    /// when the user selects a different node.
    prev_personal_editor_path: Option<Vec<usize>>,
    /// Same for the team editor pane.
    prev_team_editor_path: Option<Vec<usize>>,
    /// Incremented when the selected editor target changes so Slint text
    /// widgets resync from Rust-backed properties.
    editor_sync_version: i32,
    /// Main popup row index whose folder submenu is open (-1 = none).
    popup_open_main_idx: Option<usize>,
    /// Legacy flat list — kept empty; injection resolves via
    /// `popup_main_kinds` / `popup_sub_visible_kinds`.
    popup_visible_kinds: Vec<PopupVisibleKind>,
    /// Screen-space bounds of the snippet popup in physical pixels
    /// while it's visible. Used by the 125ms tick to dismiss the
    /// popup when the user clicks anywhere outside it.
    popup_bounds: Option<(i32, i32, i32, i32)>,
    /// Foreground window captured at the moment the global hotkey
    /// fired (before our popup steals focus). The injector uses this
    /// to restore focus to the user's actual target app right before
    /// sending keystrokes; without it, paste/typing lands in whatever
    /// window happens to be focused when the popup closes.
    target_hwnd: Option<WindowHandle>,
    /// While the "Review text before paste" modal is up, the injection
    /// is paused mid-flight. This holds everything `confirm_review` needs
    /// to resume: the prepared (no-DeepL) body, the original snippet
    /// text (for translation-pair extraction), the captured target
    /// window, and the resolved injection mode. `cancel_review` simply
    /// drops it.
    pending_review: Option<PendingReview>,
    /// Selective import/export session. Holds a deep clone of the
    /// candidate tree plus the chosen file path; alive only while the
    /// picker (and the optional follow-up merge/replace prompt) is on
    /// screen.
    picker_session: Option<picker::PickerSession>,
}

/// State carried across the user's pre-paste review confirmation.
/// See `AppState.pending_review` for lifecycle notes.
struct PendingReview {
    snippet_name: String,
    snippet_text_original: String,
    prepared_no_deepl: String,
    preview_text: String,
    shared_source: bool,
    injection_mode: InjectionMode,
    target_hwnd: Option<WindowHandle>,
}

#[derive(Clone, Debug)]
enum PopupVisibleKind {
    InjectSnippet(Snippet),
}

/// One row in the snippet popup's primary (left) column.
#[derive(Clone, Debug)]
enum PopupTopKind {
    Folder(Folder),
    Snippet(Snippet),
}

fn find_snippet_by_name(nodes: &[Node], needle: &str) -> Option<String> {
    let wanted = needle.trim().to_ascii_lowercase();
    if wanted.is_empty() {
        return None;
    }
    for node in nodes {
        match node {
            Node::Snippet(s) => {
                if s.name.trim().to_ascii_lowercase() == wanted {
                    return Some(s.text.clone());
                }
            }
            Node::Folder(f) => {
                if let Some(found) = find_snippet_by_name(&f.children, needle) {
                    return Some(found);
                }
            }
        }
    }
    None
}

fn folder_matches(
    folder: &poltergeist_core::models::Folder,
    context: &HashMap<String, String>,
) -> bool {
    evaluate_match_rule(folder.r#match.as_ref(), Some(context))
}

fn snippet_matches(snippet: &Snippet, context: &HashMap<String, String>) -> bool {
    evaluate_match_rule(snippet.r#match.as_ref(), Some(context))
}

fn to_platform_mode(mode: InjectionMode) -> PlatformInjectionMode {
    match mode {
        InjectionMode::Clipboard => PlatformInjectionMode::Clipboard,
        InjectionMode::ClipboardShiftInsert => PlatformInjectionMode::ClipboardShiftInsert,
        InjectionMode::Typing => PlatformInjectionMode::Typing,
        InjectionMode::TypingCompat => PlatformInjectionMode::TypingCompat,
    }
}

/// Run a snippet through the include/conditional/translation pipeline
/// and inject the result via the platform driver. Returns a
/// human-readable status message either way so callers can surface it
/// to the status bar without re-formatting failure cases.
///
/// This is the shared core for the global hotkey path (the snippet
/// picker popup) and any in-app trigger — keeping it in one place
/// ensures both routes honour the same DeepL guard, conditional
/// expansion order, and injection mode resolution.
fn inject_snippet_now(
    state: &Rc<RefCell<AppState>>,
    snippet: &Snippet,
    main_window: &slint::Weak<MainWindow>,
) -> Result<String, String> {
    let st = state.borrow();
    let injection_mode = snippet
        .injection
        .unwrap_or(st.cfg.settings.default_injection);
    let all_nodes = st
        .cfg
        .tree_personal
        .iter()
        .chain(st.team_tree.iter())
        .cloned()
        .collect::<Vec<_>>();
    let snippet_lookup = |name: &str| -> Option<String> { find_snippet_by_name(&all_nodes, name) };
    let prepared = tokens::expand_conditionals(
        &tokens::expand_includes(&snippet.text, Some(&snippet_lookup)),
        Some(&st.current_context),
    );

    // Branch A: snippet contains TRANSLATION blocks AND the user opted
    // into the review-before-paste flow. Stash everything we need to
    // resume, pop the modal, and bail out — `confirm_review` will call
    // back into `finalize_review_inject` once the user clicks OK.
    if snippet.prompt_untranslated_before_paste
        && TranslationService::text_has_translations(&prepared)
    {
        let clipboard_text = Clipboard::new()
            .ok()
            .and_then(|mut cb| cb.get_text().ok())
            .unwrap_or_default();
        let shared = TranslationService::uniform_expanded_translation_body_if_any(
            &prepared,
            &st.cfg.settings.default_date_format,
            &clipboard_text,
            Some(&st.current_context),
            Some(&st.db_registry),
            None,
        );
        let (preview_text, shared_source) = if let Some(body) = shared {
            (body, true)
        } else {
            (
                TranslationService::expand_translation_sources(
                    &prepared,
                    &st.cfg.settings.default_date_format,
                    &clipboard_text,
                    Some(&st.current_context),
                    Some(&st.db_registry),
                    None,
                ),
                false,
            )
        };
        let snippet_name = snippet.name.clone();
        let snippet_text_original = snippet.text.clone();
        let target_hwnd = st.target_hwnd;
        drop(st);
        {
            let mut st_mut = state.borrow_mut();
            st_mut.pending_review = Some(PendingReview {
                snippet_name: snippet_name.clone(),
                snippet_text_original,
                prepared_no_deepl: prepared,
                preview_text: preview_text.clone(),
                shared_source,
                injection_mode,
                target_hwnd,
            });
        }
        if let Some(window) = main_window.upgrade() {
            window.set_review_text(preview_text.into());
            // Make sure the main window is visible — otherwise the modal
            // is invisible (it's an overlay inside MainWindow).
            let _ = window.show();
            window.set_show_review_panel(true);
        }
        return Ok(format!("Review pending for '{snippet_name}'"));
    }

    // Branch B: no review flow — go straight through DeepL (if needed)
    // and inject. This is the original path.
    let mut prepared = prepared;
    if TranslationService::text_has_translations(&prepared) {
        if st.cfg.settings.deepl_api_key.trim().is_empty() {
            return Err("Snippet requires DeepL but no API key is configured".to_string());
        }
        match TranslationService::new(st.cfg.settings.deepl_api_key.clone()) {
            Ok(service) => match service.expand_translations(
                &prepared,
                &st.cfg.settings.default_date_format,
                &Clipboard::new()
                    .ok()
                    .and_then(|mut cb| cb.get_text().ok())
                    .unwrap_or_default(),
                Some(&st.current_context),
                Some(&st.db_registry),
                None,
                None,
            ) {
                Ok(translated) => prepared = translated,
                Err(err) => return Err(format!("Translation failed: {err}")),
            },
            Err(err) => return Err(format!("DeepL init failed: {err}")),
        }
    }
    let injection_result = inject(InjectParams {
        snippet_text: &prepared,
        mode: to_platform_mode(injection_mode),
        default_date_format: &st.cfg.settings.default_date_format,
        target_hwnd: st.target_hwnd,
        paste_delay_ms: 60,
        restore_delay_ms: 250,
        context: Some(&st.current_context),
        databases: Some(&st.db_registry),
        snippet_lookup: None,
        expanded_override: None,
    });
    match injection_result {
        Ok(()) => Ok(format!("Injected snippet '{}'", snippet.name)),
        Err(err) => Err(format!("Injection failed: {err}")),
    }
}

/// Called from the `confirm_review` Slint callback. Takes the (possibly
/// edited) text from the review modal, runs the DeepL pass with the
/// right body-override semantics, and injects. Mirrors the
/// `_on_snippet_chosen` post-dialog branch in Python's `app.py`.
fn finalize_review_inject(
    state: &Rc<RefCell<AppState>>,
    edited_text: &str,
) -> Result<String, String> {
    let pending = state
        .borrow_mut()
        .pending_review
        .take()
        .ok_or_else(|| "No review pending".to_string())?;

    let st = state.borrow();
    let clipboard_text = Clipboard::new()
        .ok()
        .and_then(|mut cb| cb.get_text().ok())
        .unwrap_or_default();
    let edited_changed = edited_text != pending.preview_text;

    // Pick the right injection strategy based on what the user did.
    let (final_text, expanded_override): (String, Option<String>) = if !edited_changed {
        // No edit → run normal DeepL expansion as if the modal didn't exist.
        if st.cfg.settings.deepl_api_key.trim().is_empty() {
            return Err("Snippet requires DeepL but no API key is configured".to_string());
        }
        let service = TranslationService::new(st.cfg.settings.deepl_api_key.clone())
            .map_err(|e| format!("DeepL init failed: {e}"))?;
        let translated = service
            .expand_translations(
                &pending.prepared_no_deepl,
                &st.cfg.settings.default_date_format,
                &clipboard_text,
                Some(&st.current_context),
                Some(&st.db_registry),
                None,
                None,
            )
            .map_err(|e| format!("Translation failed: {e}"))?;
        (translated, None)
    } else if pending.shared_source {
        // Single shared source → re-translate with the edited body
        // for every TRANSLATION block (body_override).
        if st.cfg.settings.deepl_api_key.trim().is_empty() {
            return Err("Snippet requires DeepL but no API key is configured".to_string());
        }
        let service = TranslationService::new(st.cfg.settings.deepl_api_key.clone())
            .map_err(|e| format!("DeepL init failed: {e}"))?;
        let translated = service
            .expand_translations(
                &pending.prepared_no_deepl,
                &st.cfg.settings.default_date_format,
                &clipboard_text,
                Some(&st.current_context),
                Some(&st.db_registry),
                None,
                Some(edited_text),
            )
            .map_err(|e| format!("Translation failed: {e}"))?;
        (translated, None)
    } else {
        // Multiple distinct sources → only safe to translate edits when
        // the *original* snippet has a single TRANSLATION pair (parity
        // with Python's RawTextDialog edit branch). Otherwise refuse.
        let pairs = TranslationService::translation_pairs_in_text(&pending.snippet_text_original);
        if pairs.len() == 1 {
            if st.cfg.settings.deepl_api_key.trim().is_empty() {
                return Err("Snippet requires DeepL but no API key is configured".to_string());
            }
            let service = TranslationService::new(st.cfg.settings.deepl_api_key.clone())
                .map_err(|e| format!("DeepL init failed: {e}"))?;
            let (src, tgt) = &pairs[0];
            let translated = service
                .translate_plain_text(edited_text, src.as_deref(), tgt)
                .map_err(|e| format!("Translation failed: {e}"))?;
            (translated.clone(), Some(translated))
        } else {
            return Err(
                "Edited translation preview requires exactly one TRANSLATION block in the snippet"
                    .to_string(),
            );
        }
    };

    let injection_result = inject(InjectParams {
        snippet_text: &final_text,
        mode: to_platform_mode(pending.injection_mode),
        default_date_format: &st.cfg.settings.default_date_format,
        target_hwnd: pending.target_hwnd,
        paste_delay_ms: 60,
        restore_delay_ms: 250,
        context: Some(&st.current_context),
        databases: Some(&st.db_registry),
        snippet_lookup: None,
        expanded_override: expanded_override.as_deref(),
    });
    drop(st);
    state.borrow_mut().target_hwnd = None;
    match injection_result {
        Ok(()) => Ok(format!("Injected snippet '{}'", pending.snippet_name)),
        Err(err) => Err(format!("Injection failed: {err}")),
    }
}

fn capture_context(patterns: &[String]) -> HashMap<String, String> {
    let clipboard_text = Clipboard::new()
        .ok()
        .and_then(|mut cb| cb.get_text().ok())
        .unwrap_or_default();
    context_svc::parse(&clipboard_text, patterns)
}

fn main() -> Result<()> {
    init_logging();
    tracing::info!("starting Poltergeist Rust app");

    let app_base = base_dir();
    let edition = detect_edition(&app_base);

    // Single-instance enforcement runs *before* any UI/tray/hotkey
    // initialisation so a duplicate launch dies cleanly without
    // briefly stealing focus or fighting over the global hotkey.
    // We deliberately use the same Global\ mutex names as the Python
    // parent (`main.py`), so a Rust+Python mixed install on the same
    // machine still cooperates within the same edition.
    let _instance_guard =
        match poltergeist_platform_win::single_instance::try_acquire(edition == Edition::Admin) {
            poltergeist_platform_win::single_instance::AcquireResult::Acquired(guard) => guard,
            poltergeist_platform_win::single_instance::AcquireResult::AlreadyRunning => {
                tracing::info!("another Poltergeist instance is already running; exiting");
                poltergeist_platform_win::single_instance::show_already_running_dialog(
                    edition == Edition::Admin,
                );
                return Ok(());
            }
        };

    // Captured *before* `config::load` would otherwise materialise the
    // file via downstream save paths — we use it later to drive the
    // first-run tray balloon (see `polish-first-run-toast`).
    let first_run = config::is_first_run(&app_base);
    let cfg = config::load(&app_base);
    let team_pack = if edition == Edition::User {
        team_pack::read_pack_sync(&cfg.settings.team_share_path, &app_base)
    } else {
        let source = team_pack::probe_status(&cfg.settings.team_share_path, &app_base);
        team_pack::TeamPack {
            tree: cfg.tree_team.clone(),
            manifest: team_pack::TeamManifest::default(),
            source,
        }
    };

    let mut db_registry = DatabaseRegistry::new();
    let _ = db_registry.load_from_sources(
        team_pack::share_root(&cfg.settings.team_share_path).as_deref(),
        Some(&team_pack::cache_dir(&app_base)),
    );

    let mut personal_names = Vec::new();
    collect_snippet_names(&cfg.tree_personal, &mut personal_names);
    let mut team_names = Vec::new();
    collect_snippet_names(&team_pack.tree, &mut team_names);
    let should_start_hidden = !(cfg.tree_personal.is_empty() && team_pack.tree.is_empty());

    let icons = IconAssets::default();

    let translation_summary = if !cfg.settings.deepl_api_key.trim().is_empty() {
        match TranslationService::new(cfg.settings.deepl_api_key.clone()) {
            Ok(service) => {
                let (_, msg) = service
                    .validate()
                    .unwrap_or((false, "DeepL validation failed".to_string()));
                msg
            }
            Err(err) => format!("DeepL setup failed: {err}"),
        }
    } else {
        "DeepL not configured".to_string()
    };

    let main_window = MainWindow::new()?;
    let (sw_top, sw_bottom) = build_color_swatch_row_pair();
    main_window.set_color_swatch_row_top(sw_top);
    main_window.set_color_swatch_row_bottom(sw_bottom);

    // Borderless snippet picker — separate top-level window so it can be
    // positioned at the OS cursor (rather than inside MainWindow's frame)
    // and so triggering it via the global hotkey doesn't yank the whole
    // app to the foreground with its title bar. Callbacks are wired
    // *after* `state` is created below since they need to mutate it.
    let snippet_popup = SnippetPopup::new()?;
    let initial_is_light = effective_is_light(cfg.settings.theme);
    let state = Rc::new(RefCell::new(AppState {
        cfg: cfg.clone(),
        edition,
        team_tree: team_pack.tree.clone(),
        db_registry,
        personal_collapsed: HashSet::new(),
        team_collapsed: HashSet::new(),
        current_context: HashMap::new(),
        personal_paths: Vec::new(),
        selected_personal: None,
        team_paths: Vec::new(),
        selected_team: None,
        team_manifest_version: team_pack.manifest.version,
        team_source: team_pack.source,
        icons: icons.clone(),
        is_light_theme: initial_is_light,
        popup_nav_stack: Vec::new(),
        popup_main_kinds: Vec::new(),
        popup_sub_visible_kinds: Vec::new(),
        prev_personal_editor_path: None,
        prev_team_editor_path: None,
        editor_sync_version: 0,
        popup_open_main_idx: None,
        popup_visible_kinds: Vec::new(),
        popup_bounds: None,
        target_hwnd: None,
        pending_review: None,
        picker_session: None,
    }));
    let _ = (personal_names, team_names);

    if let (Some(w), Some(h)) = (
        state.borrow().cfg.settings.main_window_width,
        state.borrow().cfg.settings.main_window_height,
    ) {
        let w = w.max(860.0) as f32;
        let h = h.max(560.0) as f32;
        main_window.window().set_size(LogicalSize::new(w, h));
    }

    let weak_close = main_window.as_weak();
    let state_close = Rc::clone(&state);
    let base_close = app_base.clone();
    main_window.window().on_close_requested(move || {
        if let Some(window) = weak_close.upgrade() {
            persist_main_window_geometry(window.window(), &state_close, &base_close);
            window.set_status_text(
                i18n::tr("Window hidden to tray. Use tray icon or hotkey to reopen.").into(),
            );
        }
        CloseRequestResponse::HideWindow
    });

    // Debounced save when the user resizes the main window (Slint `changed width/height`).
    let resize_save_enabled = Rc::new(Cell::new(false));
    let resize_geometry_gate_timer = Rc::new(Timer::default());
    {
        let en = Rc::clone(&resize_save_enabled);
        resize_geometry_gate_timer.start(
            TimerMode::SingleShot,
            Duration::from_millis(900),
            move || {
                en.set(true);
            },
        );
    }
    let resize_geometry_save_timer: Rc<Timer> = Rc::new(Timer::default());
    let weak_geom = main_window.as_weak();
    let state_geom = Rc::clone(&state);
    let base_geom = app_base.clone();
    let enabled_geom = Rc::clone(&resize_save_enabled);
    let t_geom = Rc::clone(&resize_geometry_save_timer);
    main_window.on_main_window_geometry_changed(move || {
        if !enabled_geom.get() {
            return;
        }
        let weak2 = weak_geom.clone();
        let st = Rc::clone(&state_geom);
        let b = base_geom.clone();
        t_geom.start(
            TimerMode::SingleShot,
            Duration::from_millis(450),
            move || {
                if let Some(w) = weak2.upgrade() {
                    persist_main_window_geometry(w.window(), &st, &b);
                }
            },
        );
    });

    // ---- SnippetPopup callbacks (need state) ----
    {
        let popup_weak = snippet_popup.as_weak();
        let weak_main = main_window.as_weak();
        let state_sel = Rc::clone(&state);
        snippet_popup.on_main_row_clicked(move |idx| {
            let i = usize::try_from(idx).unwrap_or(usize::MAX);
            let snippet = {
                let st = state_sel.borrow();
                match st.popup_main_kinds.get(i).cloned() {
                    Some(PopupTopKind::Snippet(s)) => Some(s),
                    _ => None,
                }
            };
            if let Some(snippet) = snippet {
                if let Some(p) = popup_weak.upgrade() {
                    let _ = p.hide();
                }
                let outcome = inject_snippet_now(&state_sel, &snippet, &weak_main);
                if let Some(mw) = weak_main.upgrade() {
                    let msg = match outcome {
                        Ok(s) => s,
                        Err(s) => s,
                    };
                    mw.set_status_text(SharedString::from(msg));
                }
                let mut st = state_sel.borrow_mut();
                st.popup_bounds = None;
                st.target_hwnd = None;
            }
        });
    }
    {
        let popup_hover = snippet_popup.clone_strong();
        let state_hover = Rc::clone(&state);
        snippet_popup.on_folder_hover(move |idx| {
            let i = usize::try_from(idx).unwrap_or(usize::MAX);
            let mut st = state_hover.borrow_mut();
            popup_fill_submenu_for_main_index(&mut st, i, &popup_hover);
        });
    }
    {
        let popup_lv = snippet_popup.clone_strong();
        let state_lv = Rc::clone(&state);
        snippet_popup.on_folder_leave(move || {
            let mut st = state_lv.borrow_mut();
            popup_clear_submenu(&mut st, &popup_lv);
        });
    }
    {
        let popup_weak = snippet_popup.as_weak();
        let weak_main = main_window.as_weak();
        let state_sub = Rc::clone(&state);
        snippet_popup.on_sub_selected(move |idx| {
            let i = usize::try_from(idx).unwrap_or(usize::MAX);
            let action = {
                let st = state_sub.borrow();
                st.popup_sub_visible_kinds.get(i).cloned()
            };
            match action {
                Some(PopupVisibleKind::InjectSnippet(snippet)) => {
                    if let Some(p) = popup_weak.upgrade() {
                        let _ = p.hide();
                    }
                    let outcome = inject_snippet_now(&state_sub, &snippet, &weak_main);
                    if let Some(mw) = weak_main.upgrade() {
                        let msg = match outcome {
                            Ok(s) => s,
                            Err(s) => s,
                        };
                        mw.set_status_text(SharedString::from(msg));
                    }
                    let mut st = state_sub.borrow_mut();
                    st.popup_bounds = None;
                    st.target_hwnd = None;
                }
                None => {}
            }
        });
    }
    {
        let popup_weak = snippet_popup.as_weak();
        let state_dis = Rc::clone(&state);
        snippet_popup.on_dismissed(move || {
            if let Some(p) = popup_weak.upgrade() {
                let _ = p.hide();
            }
            let mut st = state_dis.borrow_mut();
            st.popup_bounds = None;
            st.target_hwnd = None;
        });
    }

    main_window.set_status_text(
        i18n::tr_format(
            "Loaded {0} | Edition: {1} | Team: {2} | Databases: {3} | DeepL: {4}",
            &[
                &config::config_path(&app_base).display(),
                &format!("{:?}", edition),
                &format!("{:?}", team_pack.source),
                &state.borrow().db_registry.database_names().len(),
                &translation_summary,
            ],
        )
        .into(),
    );
    main_window.set_hotkey_text(cfg.settings.hotkey.clone().into());
    main_window.set_date_format_text(cfg.settings.default_date_format.clone().into());
    main_window.set_date_format_preview_text(
        format_date_preview(&cfg.settings.default_date_format).into(),
    );
    main_window
        .set_default_injection_index(default_injection_index(cfg.settings.default_injection));
    main_window.set_theme_index(theme_index(cfg.settings.theme));
    main_window.set_available_languages(available_languages_model());
    main_window.set_language_index(language_index_from_code(&cfg.settings.language));
    apply_bundled_translation(&cfg.settings.language);
    main_window.set_start_with_windows(cfg.settings.start_with_windows);
    main_window.set_deepl_api_key_text(cfg.settings.deepl_api_key.clone().into());
    main_window.set_deepl_status_text(translation_summary.clone().into());
    main_window.set_deepl_status_kind(if cfg.settings.deepl_api_key.trim().is_empty() {
        0
    } else {
        // Initial validation summary is informational; we don't know
        // whether the saved key is valid until the user clicks Validate.
        0
    });
    main_window.set_team_share_text(cfg.settings.team_share_path.clone().into());
    main_window.set_team_share_status_text(
        share_status_text(team_pack.source, team_pack.manifest.version).into(),
    );
    main_window.set_team_share_status_kind(share_status_kind(team_pack.source));
    main_window.set_app_version_text(env!("CARGO_PKG_VERSION").into());
    main_window.set_show_about_panel(false);
    {
        let src_labels: Vec<SharedString> = std::iter::once(SharedString::from("Auto-detect"))
            .chain(
                TRANSLATION_SOURCE_LANGS
                    .iter()
                    .map(|(code, label)| SharedString::from(format!("{label} ({code})"))),
            )
            .collect();
        let tgt_labels: Vec<SharedString> = TRANSLATION_TARGET_LANGS
            .iter()
            .map(|(code, label)| SharedString::from(format!("{label} ({code})")))
            .collect();
        main_window.set_translation_source_options(ModelRc::new(VecModel::from(src_labels)));
        main_window.set_translation_target_options(ModelRc::new(VecModel::from(tgt_labels)));
        main_window.set_translation_source_index(0);
        main_window.set_translation_target_index(0);
    }
    main_window.set_context_patterns_text(cfg.settings.context_patterns.join("; ").into());
    main_window.set_hotkeys_paused(false);
    main_window.set_is_admin_edition(edition == Edition::Admin);
    main_window.set_is_light_theme(initial_is_light);
    apply_accent_from_settings(&main_window, &cfg.settings, initial_is_light);
    sync_options_accent_fields(&main_window, &cfg.settings);
    main_window.set_show_options_panel(false);
    main_window.set_show_team_panel(false);
    main_window.set_personal_tree_rows(rows_model(Vec::new()));
    main_window.set_selected_personal_kind("none".into());
    main_window.set_selected_personal_name(String::new().into());
    main_window.set_selected_personal_text(String::new().into());
    main_window.set_selected_personal_shortcut(String::new().into());
    main_window.set_selected_personal_match_expr(String::new().into());
    main_window.set_selected_personal_injection_index(0);
    main_window.set_selected_personal_prompt_untranslated(true);
    main_window.set_team_tree_rows(rows_model(Vec::new()));
    main_window.set_selected_team_kind("none".into());
    main_window.set_selected_team_name(String::new().into());
    main_window.set_selected_team_text(String::new().into());
    main_window.set_selected_team_shortcut(String::new().into());
    {
        let mut st = state.borrow_mut();
        refresh_personal_editor(&main_window, &mut st);
        refresh_team_editor(&main_window, &mut st);
    }
    let _ = icons;

    let mut hotkey_manager = HotkeyManager::new().ok();
    if let Some(manager) = hotkey_manager.as_mut() {
        let skipped = manager.install(desired_hotkey_bindings(&cfg, &team_pack.tree));
        if !skipped.is_empty() {
            main_window.set_status_text(
                i18n::tr_format(
                    "Hotkey registration warnings: {0}",
                    &[&format!("{:?}", skipped)],
                )
                .into(),
            );
        }
    } else {
        main_window.set_status_text(
            i18n::tr("Hotkey manager unavailable - popup can be opened via button").into(),
        );
    }
    let hotkeys = Rc::new(RefCell::new(hotkey_manager));
    let tray = Rc::new(RefCell::new(TrayRuntime::new(&app_base, edition)));
    if tray.borrow().is_none() {
        main_window.set_status_text(
            i18n::tr("Tray icon unavailable; using in-window controls as fallback").into(),
        );
    } else if let Some(runtime) = tray.borrow().as_ref() {
        runtime.set_paused(false);
        if should_start_hidden {
            let _ = main_window.hide();
            main_window.set_status_text(
                i18n::tr_format(
                    "Running in tray. Activate with {0} or click the tray icon.",
                    &[&cfg.settings.hotkey],
                )
                .into(),
            );
        } else {
            notify_tray(
                &app_base,
                &i18n::tr("Poltergeist is running"),
                &i18n::tr_format(
                    "Activate with {0}. Right-click the tray icon for more.",
                    &[&cfg.settings.hotkey],
                ),
            );
        }
    }

    let hotkey_poll_timer = Timer::default();
    {
        let weak_hotkey = main_window.as_weak();
        let state_hotkey = Rc::clone(&state);
        let hotkeys_poll = Rc::clone(&hotkeys);
        let tray_poll = Rc::clone(&tray);
        let popup_for_hotkey = snippet_popup.clone_strong();
        hotkey_poll_timer.start(TimerMode::Repeated, Duration::from_millis(125), move || {
            let tray_action = tray_poll
                .borrow()
                .as_ref()
                .and_then(TrayRuntime::poll_action);
            if let Some(action) = tray_action {
                if let Some(window) = weak_hotkey.upgrade() {
                    match action {
                        TrayAction::Open => {
                            let _ = window.show();
                            window.set_status_text(i18n::tr("Opened from tray").into());
                        }
                        TrayAction::Options => {
                            let _ = window.show();
                            // Close the About panel if it's open, then open Options.
                            window.set_show_about_panel(false);
                            window.set_show_options_panel(true);
                            window.set_status_text(i18n::tr("Options opened from tray").into());
                        }
                        TrayAction::About => {
                            let _ = window.show();
                            // Close Options if it's open, then open About.
                            window.set_show_options_panel(false);
                            window.set_show_about_panel(true);
                            window.set_status_text(i18n::tr("About opened from tray").into());
                        }
                        TrayAction::Pause(paused) => {
                            if let Some(manager) = hotkeys_poll.borrow_mut().as_mut() {
                                let _ = manager.set_paused(paused);
                                window.set_hotkeys_paused(paused);
                            }
                            window.set_status_text(
                                if paused {
                                    i18n::tr("Hotkeys paused from tray")
                                } else {
                                    i18n::tr("Hotkeys resumed from tray")
                                }
                                .into(),
                            );
                        }
                        TrayAction::Exit => {
                            let _ = window.hide();
                            std::process::exit(0);
                        }
                    }
                }
            }

            let ids = {
                let manager_guard = hotkeys_poll.borrow();
                if let Some(manager) = manager_guard.as_ref() {
                    manager.poll_events()
                } else {
                    Vec::new()
                }
            };
            if !ids.is_empty() {
                let mut triggered = Vec::new();
                {
                    let manager_guard = hotkeys_poll.borrow();
                    if let Some(manager) = manager_guard.as_ref() {
                        for id in ids {
                            if let Some(name) = manager.binding_name_for_id(id) {
                                triggered.push(name);
                            }
                        }
                    }
                }
                if !triggered.is_empty() {
                    if let Some(window) = weak_hotkey.upgrade() {
                        // If the picker is already open, treat the hotkey
                        // as a toggle: hide it and skip dispatch. Without
                        // this guard a second tap would just re-open the
                        // picker on top of itself.
                        if popup_for_hotkey.window().is_visible() {
                            let _ = popup_for_hotkey.hide();
                            let mut st = state_hotkey.borrow_mut();
                            st.popup_bounds = None;
                            st.target_hwnd = None;
                        } else {
                            // Capture the foreground window NOW, before
                            // we open the popup (which steals focus).
                            // Without this, set_foreground() inside the
                            // injector has no idea where the user wanted
                            // their keystrokes to land.
                            let captured_target = current_foreground();
                            let mut st = state_hotkey.borrow_mut();
                            st.target_hwnd = captured_target;
                            if let Some(binding) = triggered.first() {
                                if binding == "main" {
                                    let all_nodes = st
                                        .cfg
                                        .tree_personal
                                        .iter()
                                        .chain(st.team_tree.iter())
                                        .cloned()
                                        .collect::<Vec<_>>();
                                    open_popup_for_nodes(
                                        &window,
                                        &popup_for_hotkey,
                                        &mut st,
                                        &all_nodes,
                                        "main hotkey",
                                    );
                                } else if let Some(folder_id) = binding.strip_prefix("team:") {
                                    if let Some(folder) = top_level_folder(&st.team_tree, folder_id)
                                    {
                                        let source = vec![Node::Folder(folder)];
                                        open_popup_for_nodes(
                                            &window,
                                            &popup_for_hotkey,
                                            &mut st,
                                            &source,
                                            "team folder hotkey",
                                        );
                                    }
                                } else if let Some(folder_id) = binding.strip_prefix("personal:") {
                                    if let Some(folder) =
                                        top_level_folder(&st.cfg.tree_personal, folder_id)
                                    {
                                        let source = vec![Node::Folder(folder)];
                                        open_popup_for_nodes(
                                            &window,
                                            &popup_for_hotkey,
                                            &mut st,
                                            &source,
                                            "personal folder hotkey",
                                        );
                                    }
                                }
                            }
                        }
                    }
                }
            }

            // Click-outside watcher: if the picker is visible and the
            // user has just clicked outside its physical-pixel bounds,
            // dismiss it. Runs every tick (not gated on hotkey events)
            // so the user doesn't have to wait for a hotkey to dismiss
            // the menu by clicking elsewhere on the desktop.
            if popup_for_hotkey.window().is_visible() {
                let bounds = state_hotkey.borrow().popup_bounds;
                if let Some(bounds) = bounds {
                    if poltergeist_platform_win::cursor::primary_buttons_down() {
                        if let Some((cx, cy)) = poltergeist_platform_win::cursor::position() {
                            let (x, y, w, h) = bounds;
                            let inside = cx >= x && cx < x + w && cy >= y && cy < y + h;
                            if !inside {
                                let _ = popup_for_hotkey.hide();
                                let mut st = state_hotkey.borrow_mut();
                                st.popup_bounds = None;
                                st.target_hwnd = None;
                            }
                        }
                    }
                }
            }
        });
    }

    let team_poll_timer = Timer::default();
    {
        let weak_team_poll = main_window.as_weak();
        let state_team_poll = Rc::clone(&state);
        let hotkeys_team_poll = Rc::clone(&hotkeys);
        let base_team_poll = app_base.clone();
        team_poll_timer.start(
            TimerMode::Repeated,
            Duration::from_secs(5 * 60),
            move || {
                if let Some(window) = weak_team_poll.upgrade() {
                    let mut st = state_team_poll.borrow_mut();
                    if st.edition != Edition::User
                        || st.cfg.settings.team_share_path.trim().is_empty()
                    {
                        return;
                    }
                    let refreshed_pack = team_pack::read_pack_sync(
                        &st.cfg.settings.team_share_path,
                        &base_team_poll,
                    );
                    let newer_available = refreshed_pack.manifest.version
                        > st.team_manifest_version
                        || (st.team_tree.is_empty() && !refreshed_pack.tree.is_empty());
                    if !newer_available {
                        return;
                    }
                    st.team_tree = refreshed_pack.tree;
                    st.team_manifest_version = refreshed_pack.manifest.version;
                    st.team_source = refreshed_pack.source;
                    let share_root = team_pack::share_root(&st.cfg.settings.team_share_path);
                    let cache_dir = team_pack::cache_dir(&base_team_poll);
                    let _ = st
                        .db_registry
                        .load_from_sources(share_root.as_deref(), Some(&cache_dir));
                    window.set_team_share_status_text(
                        share_status_text(st.team_source, st.team_manifest_version).into(),
                    );
                    refresh_team_editor(&window, &mut st);
                    let hotkey_warnings =
                        install_hotkeys(&hotkeys_team_poll, &st.cfg, &st.team_tree)
                            .filter(|w| !w.is_empty());
                    if let Some(warnings) = hotkey_warnings {
                        window.set_status_text(
                            i18n::tr_format(
                                "Team share updated; hotkey warnings: {0}",
                                &[&format!("{:?}", warnings)],
                            )
                            .into(),
                        );
                    } else {
                        window.set_status_text(
                            i18n::tr_format(
                                "Team share updated to pack v{0}",
                                &[&st.team_manifest_version],
                            )
                            .into(),
                        );
                    }
                }
            },
        );
    }

    // ------- Debounced auto-save --------------------------------------
    //
    // Every edit callback in main.slint also fires `root.request_save()`,
    // which restarts this 150ms single-shot timer. After the user pauses
    // editing for that long the JSON is flushed to disk and the toolbar
    // dot flips to "Saved". The dot color/state is mirrored by the
    // `save_state_kind` property on MainWindow (0=idle, 1=pending,
    // 2=saved). We keep the timer alive in an Rc so each invocation of
    // the request_save callback can reach .start() to reset it.
    let autosave_timer: Rc<Timer> = Rc::new(Timer::default());
    let autosave_pending: Rc<Cell<bool>> = Rc::new(Cell::new(false));
    {
        let weak_req = main_window.as_weak();
        let state_req = Rc::clone(&state);
        let base_req = app_base.clone();
        let timer_req = Rc::clone(&autosave_timer);
        let pending_req = Rc::clone(&autosave_pending);
        main_window.on_request_save(move || {
            pending_req.set(true);
            if let Some(window) = weak_req.upgrade() {
                window.set_save_state_kind(1);
                window.set_save_status_text(SharedString::from("Auto-save pending…"));
            }
            let weak_inner = weak_req.clone();
            let state_inner = Rc::clone(&state_req);
            let base_inner = base_req.clone();
            let pending_inner = Rc::clone(&pending_req);
            timer_req.start(
                TimerMode::SingleShot,
                Duration::from_millis(150),
                move || {
                    pending_inner.set(false);
                    let mut st = state_inner.borrow_mut();
                    st.cfg.tree_team = st.team_tree.clone();
                    let result = config::save(&base_inner, &st.cfg);
                    if let Some(window) = weak_inner.upgrade() {
                        match result {
                            Ok(()) => {
                                window.set_save_state_kind(2);
                                window.set_save_status_text(SharedString::from("Auto-saved"));
                            }
                            Err(err) => {
                                window.set_save_state_kind(0);
                                window.set_save_status_text(SharedString::from(format!(
                                    "Auto-save failed: {err}"
                                )));
                            }
                        }
                    }
                },
            );
        });
    }

    // The About dialog is now driven entirely by the `show_about_panel`
    // property the toolbar button toggles in `main.slint`. Rust no longer
    // needs to run anything when the user clicks About.

    let weak_select = main_window.as_weak();
    let state_select = Rc::clone(&state);
    main_window.on_select_personal_row(move |index| {
        if let Some(window) = weak_select.upgrade() {
            let mut st = state_select.borrow_mut();
            let idx = usize::try_from(index).unwrap_or(usize::MAX);
            st.selected_personal = if idx < st.personal_paths.len() {
                Some(idx)
            } else {
                None
            };
            refresh_personal_editor(&window, &mut st);
        }
    });

    let weak_team_select = main_window.as_weak();
    let state_team_select = Rc::clone(&state);
    main_window.on_select_team_row(move |index| {
        if let Some(window) = weak_team_select.upgrade() {
            let mut st = state_team_select.borrow_mut();
            let idx = usize::try_from(index).unwrap_or(usize::MAX);
            st.selected_team = if idx < st.team_paths.len() {
                Some(idx)
            } else {
                None
            };
            refresh_team_editor(&window, &mut st);
            if st.edition == Edition::Admin {
                window.set_status_text(
                    i18n::tr(
                        "Admin mode: team nodes are locally editable; publish to share to deploy.",
                    )
                    .into(),
                );
            } else {
                window.set_status_text(
                    i18n::tr("Team content is read-only. Shortcut overrides are user-level only.")
                        .into(),
                );
            }
        }
    });

    let weak_toggle_personal = main_window.as_weak();
    let state_toggle_personal = Rc::clone(&state);
    main_window.on_toggle_personal_folder(move |index| {
        if let Some(window) = weak_toggle_personal.upgrade() {
            let mut st = state_toggle_personal.borrow_mut();
            let idx = usize::try_from(index).unwrap_or(usize::MAX);
            // Snapshot the selected path so we can re-find it once the
            // tree is rebuilt — the flat index is unstable across an
            // expand/collapse, but the path is.
            let prev_selected_path = st
                .selected_personal
                .and_then(|i| st.personal_paths.get(i))
                .cloned();
            let path = match st.personal_paths.get(idx) {
                Some(p) => p.clone(),
                None => return,
            };
            // Confirm the row at this index is actually a folder; clicking
            // a snippet row would never reach the chevron but defend
            // against future refactors.
            if !matches!(
                get_node_ref(&st.cfg.tree_personal, &path),
                Some(Node::Folder(_))
            ) {
                return;
            }
            if !st.personal_collapsed.insert(path.clone()) {
                st.personal_collapsed.remove(&path);
            }
            refresh_personal_editor(&window, &mut st);
            if let Some(prev) = prev_selected_path {
                st.selected_personal = st.personal_paths.iter().position(|p| p == &prev);
                refresh_personal_editor(&window, &mut st);
            }
        }
    });

    let weak_toggle_team = main_window.as_weak();
    let state_toggle_team = Rc::clone(&state);
    main_window.on_toggle_team_folder(move |index| {
        if let Some(window) = weak_toggle_team.upgrade() {
            let mut st = state_toggle_team.borrow_mut();
            let idx = usize::try_from(index).unwrap_or(usize::MAX);
            let prev_selected_path = st.selected_team.and_then(|i| st.team_paths.get(i)).cloned();
            let path = match st.team_paths.get(idx) {
                Some(p) => p.clone(),
                None => return,
            };
            if !matches!(get_node_ref(&st.team_tree, &path), Some(Node::Folder(_))) {
                return;
            }
            if !st.team_collapsed.insert(path.clone()) {
                st.team_collapsed.remove(&path);
            }
            refresh_team_editor(&window, &mut st);
            if let Some(prev) = prev_selected_path {
                st.selected_team = st.team_paths.iter().position(|p| p == &prev);
                refresh_team_editor(&window, &mut st);
            }
        }
    });

    let weak_set_personal_color = main_window.as_weak();
    let state_set_personal_color = Rc::clone(&state);
    main_window.on_set_personal_color(move |raw| {
        if let Some(window) = weak_set_personal_color.upgrade() {
            let mut st = state_set_personal_color.borrow_mut();
            let Some(path) = st
                .selected_personal
                .and_then(|idx| st.personal_paths.get(idx))
                .cloned()
            else {
                window.set_status_text(i18n::tr("No personal node selected").into());
                return;
            };
            let trimmed = raw.trim().to_string();
            // Empty input clears the colour. Otherwise require a literal
            // we can parse so the swatch always reflects the stored
            // value; if the user typed garbage we keep the old colour
            // and warn instead of silently setting an unrenderable hex.
            let new_color = if trimmed.is_empty() {
                None
            } else if parse_color_hex(&trimmed).is_some() {
                Some(trimmed.clone())
            } else {
                window.set_status_text(
                    i18n::tr("Color must be in #rrggbb or #rrggbbaa form (e.g. #ff8800)").into(),
                );
                return;
            };
            if let Some(node) = get_node_mut(&mut st.cfg.tree_personal, &path) {
                match node {
                    Node::Folder(folder) => folder.color = new_color,
                    Node::Snippet(snippet) => snippet.color = new_color,
                }
                refresh_personal_editor(&window, &mut st);
                window.set_status_text(
                    if trimmed.is_empty() {
                        i18n::tr("Cleared node colour")
                    } else {
                        i18n::tr_format("Set node colour to {0}", &[&trimmed])
                    }
                    .into(),
                );
            }
        }
    });

    let weak_set_team_color = main_window.as_weak();
    let state_set_team_color = Rc::clone(&state);
    main_window.on_set_team_color(move |raw| {
        if let Some(window) = weak_set_team_color.upgrade() {
            let mut st = state_set_team_color.borrow_mut();
            if st.edition != Edition::Admin {
                window.set_status_text(
                    i18n::tr("Team node colours can only be set in the Admin edition").into(),
                );
                return;
            }
            let Some(path) = st
                .selected_team
                .and_then(|idx| st.team_paths.get(idx))
                .cloned()
            else {
                window.set_status_text(i18n::tr("No team node selected").into());
                return;
            };
            let trimmed = raw.trim().to_string();
            let new_color = if trimmed.is_empty() {
                None
            } else if parse_color_hex(&trimmed).is_some() {
                Some(trimmed.clone())
            } else {
                window.set_status_text(
                    i18n::tr("Color must be in #rrggbb or #rrggbbaa form (e.g. #ff8800)").into(),
                );
                return;
            };
            if let Some(node) = get_node_mut(&mut st.team_tree, &path) {
                match node {
                    Node::Folder(folder) => folder.color = new_color,
                    Node::Snippet(snippet) => snippet.color = new_color,
                }
                refresh_team_editor(&window, &mut st);
                window.set_status_text(
                    if trimmed.is_empty() {
                        i18n::tr("Cleared team node colour")
                    } else {
                        i18n::tr_format("Set team node colour to {0}", &[&trimmed])
                    }
                    .into(),
                );
            }
        }
    });

    let weak_insert_token = main_window.as_weak();
    main_window.on_insert_personal_token(move |token| {
        if let Some(window) = weak_insert_token.upgrade() {
            let current = window.get_selected_personal_text();
            let mut new_text = current.to_string();
            new_text.push_str(token.as_str());
            window.set_selected_personal_text(new_text.into());
            window.invoke_update_personal_text(window.get_selected_personal_text());
            window.set_status_text(i18n::tr_format("Inserted token {0}", &[&token]).into());
        }
    });

    let weak_insert_team_token = main_window.as_weak();
    main_window.on_insert_team_token(move |token| {
        if let Some(window) = weak_insert_team_token.upgrade() {
            // Match the per-callback admin gate that `update_team_text`
            // uses; admins are the only callers in practice (the popup's
            // team trigger only renders when `is_admin_edition`), but
            // belt-and-braces in case the popup is reached some other way.
            if !window.get_is_admin_edition() {
                window.set_status_text(
                    i18n::tr("Team tree editing is only available in admin mode").into(),
                );
                return;
            }
            let current = window.get_selected_team_text();
            let mut new_text = current.to_string();
            new_text.push_str(token.as_str());
            window.set_selected_team_text(new_text.into());
            window.invoke_update_team_text(window.get_selected_team_text());
            window.set_status_text(i18n::tr_format("Inserted team token {0}", &[&token]).into());
        }
    });

    let weak_build_token = main_window.as_weak();
    main_window.on_build_and_insert_token(move |target, kind, value| {
        let Some(window) = weak_build_token.upgrade() else {
            return;
        };
        let token = match build_token(kind.as_str(), value.as_str()) {
            Ok(t) => t,
            Err(msg) => {
                window.set_status_text(msg.into());
                return;
            }
        };
        if target.as_str() == "team" {
            window.invoke_insert_team_token(token.into());
        } else {
            window.invoke_insert_personal_token(token.into());
        }
    });

    let weak_translation_picker = main_window.as_weak();
    main_window.on_accept_translation_picker(move |source_idx, target_idx, target| {
        let Some(window) = weak_translation_picker.upgrade() else {
            return;
        };
        let Some(token) = build_translation_pair_token(source_idx, target_idx) else {
            window
                .set_status_text(i18n::tr("Translation picker: invalid language selection").into());
            return;
        };
        if target.as_str() == "team" {
            window.invoke_insert_team_token(token.into());
        } else {
            window.invoke_insert_personal_token(token.into());
        }
    });

    let weak_add_team_folder = main_window.as_weak();
    let state_add_team_folder = Rc::clone(&state);
    let hotkeys_add_team_folder = Rc::clone(&hotkeys);
    main_window.on_add_team_folder(move || {
        if let Some(window) = weak_add_team_folder.upgrade() {
            let mut st = state_add_team_folder.borrow_mut();
            if st.edition != Edition::Admin {
                window.set_status_text(
                    i18n::tr("Team tree editing is only available in admin mode").into(),
                );
                return;
            }
            let selected_path = st
                .selected_team
                .and_then(|idx| st.team_paths.get(idx))
                .cloned();
            add_under_selected_or_root(
                &mut st.team_tree,
                selected_path.as_deref(),
                Node::Folder(default_folder()),
            );
            refresh_team_editor(&window, &mut st);
            let hotkey_warnings = install_hotkeys(&hotkeys_add_team_folder, &st.cfg, &st.team_tree)
                .filter(|w| !w.is_empty());
            if let Some(warnings) = hotkey_warnings {
                window.set_status_text(
                    i18n::tr_format(
                        "Added team folder; hotkey warnings: {0}",
                        &[&format!("{:?}", warnings)],
                    )
                    .into(),
                );
            } else {
                window.set_status_text(i18n::tr("Added folder to team tree").into());
            }
        }
    });

    let weak_add_team_snippet = main_window.as_weak();
    let state_add_team_snippet = Rc::clone(&state);
    let hotkeys_add_team_snippet = Rc::clone(&hotkeys);
    main_window.on_add_team_snippet(move || {
        if let Some(window) = weak_add_team_snippet.upgrade() {
            let mut st = state_add_team_snippet.borrow_mut();
            if st.edition != Edition::Admin {
                window.set_status_text(
                    i18n::tr("Team tree editing is only available in admin mode").into(),
                );
                return;
            }
            let selected_path = st
                .selected_team
                .and_then(|idx| st.team_paths.get(idx))
                .cloned();
            add_under_selected_or_root(
                &mut st.team_tree,
                selected_path.as_deref(),
                Node::Snippet(default_snippet()),
            );
            refresh_team_editor(&window, &mut st);
            let hotkey_warnings =
                install_hotkeys(&hotkeys_add_team_snippet, &st.cfg, &st.team_tree)
                    .filter(|w| !w.is_empty());
            if let Some(warnings) = hotkey_warnings {
                window.set_status_text(
                    i18n::tr_format(
                        "Added team snippet; hotkey warnings: {0}",
                        &[&format!("{:?}", warnings)],
                    )
                    .into(),
                );
            } else {
                window.set_status_text(i18n::tr("Added snippet to team tree").into());
            }
        }
    });

    let weak_rename_team = main_window.as_weak();
    let state_rename_team = Rc::clone(&state);
    main_window.on_rename_team_selected(move |name| {
        if let Some(window) = weak_rename_team.upgrade() {
            let mut st = state_rename_team.borrow_mut();
            if st.edition != Edition::Admin {
                window.set_status_text(
                    i18n::tr("Team tree editing is only available in admin mode").into(),
                );
                return;
            }
            let Some(path) = st
                .selected_team
                .and_then(|idx| st.team_paths.get(idx))
                .cloned()
            else {
                window.set_status_text(i18n::tr("No team node selected").into());
                return;
            };
            let new_name = name.trim();
            if new_name.is_empty() {
                window.set_status_text(i18n::tr("Name cannot be empty").into());
                return;
            }
            if let Some(node) = get_node_mut(&mut st.team_tree, &path) {
                match node {
                    Node::Folder(folder) => folder.name = new_name.to_string(),
                    Node::Snippet(snippet) => snippet.name = new_name.to_string(),
                }
                refresh_team_editor(&window, &mut st);
                window.set_status_text(i18n::tr("Renamed selected team node").into());
            }
        }
    });

    let weak_update_team_text = main_window.as_weak();
    let state_update_team_text = Rc::clone(&state);
    main_window.on_update_team_text(move |text| {
        if let Some(window) = weak_update_team_text.upgrade() {
            let mut st = state_update_team_text.borrow_mut();
            if st.edition != Edition::Admin {
                window.set_status_text(
                    i18n::tr("Team tree editing is only available in admin mode").into(),
                );
                return;
            }
            let Some(path) = st
                .selected_team
                .and_then(|idx| st.team_paths.get(idx))
                .cloned()
            else {
                window.set_status_text(i18n::tr("No team node selected").into());
                return;
            };
            if let Some(node) = get_node_mut(&mut st.team_tree, &path) {
                match node {
                    Node::Snippet(snippet) => {
                        snippet.text = text.to_string();
                        refresh_team_editor(&window, &mut st);
                        window.set_status_text(i18n::tr("Updated team snippet text").into());
                    }
                    Node::Folder(_) => {
                        window.set_status_text(i18n::tr("Selected team node is a folder").into());
                    }
                }
            }
        }
    });

    let weak_add_folder = main_window.as_weak();
    let state_add_folder = Rc::clone(&state);
    let hotkeys_add_folder = Rc::clone(&hotkeys);
    main_window.on_add_personal_folder(move || {
        if let Some(window) = weak_add_folder.upgrade() {
            let mut st = state_add_folder.borrow_mut();
            let selected_path = st
                .selected_personal
                .and_then(|idx| st.personal_paths.get(idx))
                .cloned();
            add_under_selected_or_root(
                &mut st.cfg.tree_personal,
                selected_path.as_deref(),
                Node::Folder(default_folder()),
            );
            refresh_personal_editor(&window, &mut st);
            let hotkey_warnings = install_hotkeys(&hotkeys_add_folder, &st.cfg, &st.team_tree)
                .filter(|w| !w.is_empty());
            if let Some(warnings) = hotkey_warnings {
                window.set_status_text(
                    i18n::tr_format(
                        "Added folder; hotkey warnings: {0}",
                        &[&format!("{:?}", warnings)],
                    )
                    .into(),
                );
            } else {
                window.set_status_text(i18n::tr("Added folder to personal tree").into());
            }
        }
    });

    let weak_add_snippet = main_window.as_weak();
    let state_add_snippet = Rc::clone(&state);
    let hotkeys_add_snippet = Rc::clone(&hotkeys);
    main_window.on_add_personal_snippet(move || {
        if let Some(window) = weak_add_snippet.upgrade() {
            let mut st = state_add_snippet.borrow_mut();
            let selected_path = st
                .selected_personal
                .and_then(|idx| st.personal_paths.get(idx))
                .cloned();
            add_under_selected_or_root(
                &mut st.cfg.tree_personal,
                selected_path.as_deref(),
                Node::Snippet(default_snippet()),
            );
            refresh_personal_editor(&window, &mut st);
            let hotkey_warnings = install_hotkeys(&hotkeys_add_snippet, &st.cfg, &st.team_tree)
                .filter(|w| !w.is_empty());
            if let Some(warnings) = hotkey_warnings {
                window.set_status_text(
                    i18n::tr_format(
                        "Added snippet; hotkey warnings: {0}",
                        &[&format!("{:?}", warnings)],
                    )
                    .into(),
                );
            } else {
                window.set_status_text(i18n::tr("Added snippet to personal tree").into());
            }
        }
    });

    let weak_rename = main_window.as_weak();
    let state_rename = Rc::clone(&state);
    main_window.on_rename_personal_selected(move |name| {
        if let Some(window) = weak_rename.upgrade() {
            let mut st = state_rename.borrow_mut();
            let Some(path) = st
                .selected_personal
                .and_then(|idx| st.personal_paths.get(idx))
                .cloned()
            else {
                window.set_status_text(i18n::tr("No personal node selected").into());
                return;
            };
            let new_name = name.trim();
            if new_name.is_empty() {
                window.set_status_text(i18n::tr("Name cannot be empty").into());
                return;
            }
            if let Some(node) = get_node_mut(&mut st.cfg.tree_personal, &path) {
                match node {
                    Node::Folder(folder) => folder.name = new_name.to_string(),
                    Node::Snippet(snippet) => snippet.name = new_name.to_string(),
                }
                refresh_personal_editor(&window, &mut st);
                window.set_status_text(i18n::tr("Renamed selected personal node").into());
            }
        }
    });

    let weak_update_text = main_window.as_weak();
    let state_update_text = Rc::clone(&state);
    main_window.on_update_personal_text(move |text| {
        if let Some(window) = weak_update_text.upgrade() {
            let mut st = state_update_text.borrow_mut();
            let Some(path) = st
                .selected_personal
                .and_then(|idx| st.personal_paths.get(idx))
                .cloned()
            else {
                window.set_status_text(i18n::tr("No personal node selected").into());
                return;
            };
            if let Some(node) = get_node_mut(&mut st.cfg.tree_personal, &path) {
                match node {
                    Node::Snippet(snippet) => {
                        snippet.text = text.to_string();
                        refresh_personal_editor(&window, &mut st);
                        window.set_status_text(i18n::tr("Updated snippet text").into());
                    }
                    Node::Folder(_) => {
                        window.set_status_text(
                            i18n::tr("Selected node is a folder, not a snippet").into(),
                        );
                    }
                }
            }
        }
    });

    let weak_update_shortcut = main_window.as_weak();
    let state_update_shortcut = Rc::clone(&state);
    let hotkeys_update_shortcut = Rc::clone(&hotkeys);
    main_window.on_update_personal_shortcut(move |shortcut| {
        if let Some(window) = weak_update_shortcut.upgrade() {
            let mut st = state_update_shortcut.borrow_mut();
            let Some(path) = st
                .selected_personal
                .and_then(|idx| st.personal_paths.get(idx))
                .cloned()
            else {
                window.set_status_text(i18n::tr("No personal node selected").into());
                return;
            };
            if let Some(node) = get_node_mut(&mut st.cfg.tree_personal, &path) {
                match node {
                    Node::Folder(folder) => {
                        folder.shortcut = normalize_hotkey(&shortcut);
                        refresh_personal_editor(&window, &mut st);
                        let hotkey_warnings =
                            install_hotkeys(&hotkeys_update_shortcut, &st.cfg, &st.team_tree)
                                .filter(|w| !w.is_empty());
                        if let Some(warnings) = hotkey_warnings {
                            window.set_status_text(
                                i18n::tr_format(
                                    "Updated folder shortcut; warnings: {0}",
                                    &[&format!("{:?}", warnings)],
                                )
                                .into(),
                            );
                        } else {
                            window.set_status_text(i18n::tr("Updated folder shortcut").into());
                        }
                    }
                    Node::Snippet(_) => {
                        window.set_status_text(
                            i18n::tr("Shortcuts can only be assigned to folders").into(),
                        );
                    }
                }
            }
        }
    });

    let weak_update_match = main_window.as_weak();
    let state_update_match = Rc::clone(&state);
    main_window.on_update_personal_match_expr(move |expr| {
        if let Some(window) = weak_update_match.upgrade() {
            let mut st = state_update_match.borrow_mut();
            let Some(path) = st
                .selected_personal
                .and_then(|idx| st.personal_paths.get(idx))
                .cloned()
            else {
                window.set_status_text(i18n::tr("No personal node selected").into());
                return;
            };

            let trimmed = expr.trim();
            let parsed = if trimmed.is_empty() {
                Some(None)
            } else {
                match_rule_from_expr(trimmed).map(Some)
            };
            let Some(rule) = parsed else {
                window.set_status_text(
                    i18n::tr("Invalid filter expression. Example: country = DE; type contains Sto")
                        .into(),
                );
                return;
            };

            if let Some(node) = get_node_mut(&mut st.cfg.tree_personal, &path) {
                match node {
                    Node::Folder(folder) => folder.r#match = rule,
                    Node::Snippet(snippet) => snippet.r#match = rule,
                }
                refresh_personal_editor(&window, &mut st);
                window.set_status_text(i18n::tr("Updated filter expression").into());
            }
        }
    });

    let weak_update_team_match = main_window.as_weak();
    let state_update_team_match = Rc::clone(&state);
    main_window.on_update_team_match_expr(move |expr| {
        if let Some(window) = weak_update_team_match.upgrade() {
            let mut st = state_update_team_match.borrow_mut();
            if st.edition != Edition::Admin {
                window.set_status_text(
                    i18n::tr("Team filter expressions can only be edited in the Admin edition")
                        .into(),
                );
                return;
            }
            let Some(path) = st
                .selected_team
                .and_then(|idx| st.team_paths.get(idx))
                .cloned()
            else {
                window.set_status_text(i18n::tr("No team node selected").into());
                return;
            };

            let trimmed = expr.trim();
            let parsed = if trimmed.is_empty() {
                Some(None)
            } else {
                match_rule_from_expr(trimmed).map(Some)
            };
            let Some(rule) = parsed else {
                window.set_status_text(
                    i18n::tr("Invalid filter expression. Example: country = DE; type contains Sto")
                        .into(),
                );
                return;
            };

            if let Some(node) = get_node_mut(&mut st.team_tree, &path) {
                match node {
                    Node::Folder(folder) => folder.r#match = rule,
                    Node::Snippet(snippet) => snippet.r#match = rule,
                }
                refresh_team_editor(&window, &mut st);
                window.set_status_text(i18n::tr("Updated team filter expression").into());
            }
        }
    });

    let weak_update_mode = main_window.as_weak();
    let state_update_mode = Rc::clone(&state);
    main_window.on_update_personal_injection_index(move |mode_index| {
        if let Some(window) = weak_update_mode.upgrade() {
            let mut st = state_update_mode.borrow_mut();
            let Some(path) = st
                .selected_personal
                .and_then(|idx| st.personal_paths.get(idx))
                .cloned()
            else {
                window.set_status_text(i18n::tr("No personal node selected").into());
                return;
            };
            let mode = snippet_injection_from_index(mode_index);
            if let Some(node) = get_node_mut(&mut st.cfg.tree_personal, &path) {
                match node {
                    Node::Snippet(snippet) => {
                        snippet.injection = mode;
                        refresh_personal_editor(&window, &mut st);
                        window.set_status_text(i18n::tr("Updated snippet injection mode").into());
                    }
                    Node::Folder(_) => {
                        window.set_status_text(
                            i18n::tr("Injection mode can only be set on snippets").into(),
                        );
                    }
                }
            }
        }
    });

    let weak_update_prompt = main_window.as_weak();
    let state_update_prompt = Rc::clone(&state);
    main_window.on_update_personal_prompt_untranslated(move |prompt| {
        if let Some(window) = weak_update_prompt.upgrade() {
            let mut st = state_update_prompt.borrow_mut();
            let Some(path) = st
                .selected_personal
                .and_then(|idx| st.personal_paths.get(idx))
                .cloned()
            else {
                window.set_status_text(i18n::tr("No personal node selected").into());
                return;
            };
            if let Some(node) = get_node_mut(&mut st.cfg.tree_personal, &path) {
                match node {
                    Node::Snippet(snippet) => {
                        snippet.prompt_untranslated_before_paste = prompt;
                        refresh_personal_editor(&window, &mut st);
                        window.set_status_text(
                            i18n::tr("Updated untranslated preview prompt flag").into(),
                        );
                    }
                    Node::Folder(_) => window.set_status_text(
                        i18n::tr("Prompt flag can only be set on snippets").into(),
                    ),
                }
            }
        }
    });

    let weak_update_team_shortcut = main_window.as_weak();
    let state_update_team_shortcut = Rc::clone(&state);
    let hotkeys_update_team_shortcut = Rc::clone(&hotkeys);
    main_window.on_update_team_shortcut(move |shortcut| {
        if let Some(window) = weak_update_team_shortcut.upgrade() {
            let mut st = state_update_team_shortcut.borrow_mut();
            let Some(path) = st
                .selected_team
                .and_then(|idx| st.team_paths.get(idx))
                .cloned()
            else {
                window.set_status_text(i18n::tr("No team node selected").into());
                return;
            };
            if path.len() != 1 {
                window.set_status_text(
                    i18n::tr("Team shortcuts can only be assigned to top-level team folders")
                        .into(),
                );
                return;
            }
            if let Some(Node::Folder(folder)) = get_node_ref(&st.team_tree, &path).cloned() {
                let folder_id = folder.id.clone();
                let normalized = normalize_hotkey(&shortcut);
                if st.edition == Edition::Admin {
                    if let Some(Node::Folder(folder_mut)) = get_node_mut(&mut st.team_tree, &path) {
                        folder_mut.shortcut = normalized;
                    }
                } else {
                    if let Some(value) = normalized {
                        st.cfg
                            .settings
                            .team_shortcuts
                            .insert(folder_id.clone(), value);
                    } else {
                        st.cfg.settings.team_shortcuts.remove(&folder_id);
                    }
                }
                let hotkey_warnings =
                    install_hotkeys(&hotkeys_update_team_shortcut, &st.cfg, &st.team_tree)
                        .filter(|w| !w.is_empty());
                refresh_team_editor(&window, &mut st);
                if let Some(warnings) = hotkey_warnings {
                    window.set_status_text(
                        i18n::tr_format(
                            "Updated team folder shortcut; warnings: {0}",
                            &[&format!("{:?}", warnings)],
                        )
                        .into(),
                    );
                } else {
                    window.set_status_text(i18n::tr("Updated team folder shortcut").into());
                }
            }
        }
    });

    let weak_copy_team = main_window.as_weak();
    let state_copy_team = Rc::clone(&state);
    main_window.on_copy_team_text(move || {
        if let Some(window) = weak_copy_team.upgrade() {
            let st = state_copy_team.borrow();
            let copied = st
                .selected_team
                .and_then(|idx| st.team_paths.get(idx))
                .and_then(|row| get_node_ref(&st.team_tree, row))
                .and_then(|node| match node {
                    Node::Snippet(snippet) => Some(snippet.text.clone()),
                    Node::Folder(_) => None,
                });
            match copied {
                Some(text) => {
                    let mut clipboard = match Clipboard::new() {
                        Ok(v) => v,
                        Err(err) => {
                            window.set_status_text(
                                i18n::tr_format("Unable to access clipboard: {0}", &[&err]).into(),
                            );
                            return;
                        }
                    };
                    match clipboard.set_text(text) {
                        Ok(()) => {
                            window.set_status_text(i18n::tr("Copied team snippet text").into())
                        }
                        Err(err) => window
                            .set_status_text(i18n::tr_format("Copy failed: {0}", &[&err]).into()),
                    }
                }
                None => window.set_status_text(
                    i18n::tr("Select a team snippet first to copy its text").into(),
                ),
            }
        }
    });

    let weak_delete_team = main_window.as_weak();
    let state_delete_team = Rc::clone(&state);
    let hotkeys_delete_team = Rc::clone(&hotkeys);
    main_window.on_delete_team_selected(move || {
        if let Some(window) = weak_delete_team.upgrade() {
            let mut st = state_delete_team.borrow_mut();
            if st.edition != Edition::Admin {
                window.set_status_text(
                    i18n::tr("Team tree editing is only available in admin mode").into(),
                );
                return;
            }
            let Some(path) = st
                .selected_team
                .and_then(|idx| st.team_paths.get(idx))
                .cloned()
            else {
                window.set_status_text(i18n::tr("No team node selected").into());
                return;
            };
            if remove_node_by_path(&mut st.team_tree, &path) {
                st.selected_team = None;
                refresh_team_editor(&window, &mut st);
                let hotkey_warnings = install_hotkeys(&hotkeys_delete_team, &st.cfg, &st.team_tree)
                    .filter(|w| !w.is_empty());
                if let Some(warnings) = hotkey_warnings {
                    window.set_status_text(
                        i18n::tr_format(
                            "Deleted team node; hotkey warnings: {0}",
                            &[&format!("{:?}", warnings)],
                        )
                        .into(),
                    );
                } else {
                    window.set_status_text(i18n::tr("Deleted selected team node").into());
                }
            } else {
                window.set_status_text(i18n::tr("Failed to delete selected team node").into());
            }
        }
    });

    let weak_import_team = main_window.as_weak();
    let state_import_team = Rc::clone(&state);
    main_window.on_import_team(move || {
        if let Some(window) = weak_import_team.upgrade() {
            {
                let st = state_import_team.borrow();
                if st.edition != Edition::Admin {
                    window.set_status_text(
                        i18n::tr("Team tree editing is only available in admin mode").into(),
                    );
                    return;
                }
            }
            let Some(path) = rfd::FileDialog::new()
                .add_filter("JSON", &["json"])
                .set_title(&i18n::tr("Import Team Tree JSON"))
                .pick_file()
            else {
                window.set_status_text(i18n::tr("Import cancelled").into());
                return;
            };
            let imported = match parse_import_tree(&path) {
                Ok(tree) => tree,
                Err(err) => {
                    window.set_status_text(
                        i18n::tr_format("Import failed: {0}", &[&err]).into(),
                    );
                    return;
                }
            };
            if imported.is_empty() {
                window.set_status_text(i18n::tr("No snippets found in file").into());
                return;
            }
            let mut st = state_import_team.borrow_mut();
            let session = picker::PickerSession::new(
                picker::PickerPurpose::ImportTeam,
                &imported,
                path,
            );
            show_picker(
                &window,
                &mut st,
                session,
                "Import snippets (Team)",
                "Tick what you'd like to pull in from this pack. Everything is selected by default; untick anything you want to skip.",
                "Import...",
            );
        }
    });

    let weak_export_team = main_window.as_weak();
    let state_export_team = Rc::clone(&state);
    main_window.on_export_team(move || {
        if let Some(window) = weak_export_team.upgrade() {
            // Read-only enforcement is unnecessary here (export is
            // a read-only op), but we keep it for symmetry with the
            // Python build where the menu entry is gated.
            let nodes = {
                let st = state_export_team.borrow();
                if st.team_tree.is_empty() {
                    window.set_status_text(i18n::tr("There's nothing to export yet.").into());
                    return;
                }
                st.team_tree.clone()
            };
            let default_name = if nodes.len() == 1 {
                if let Node::Folder(f) = &nodes[0] {
                    format!("{}.poltergeist.json", f.name)
                } else {
                    "snippets.poltergeist.json".to_string()
                }
            } else {
                "snippets.poltergeist.json".to_string()
            };
            let Some(path) = rfd::FileDialog::new()
                .add_filter("JSON", &["json"])
                .set_file_name(&default_name)
                .set_title(&i18n::tr("Export snippets (Team)"))
                .save_file()
            else {
                window.set_status_text(i18n::tr("Export cancelled").into());
                return;
            };
            let mut st = state_export_team.borrow_mut();
            let session = picker::PickerSession::new(
                picker::PickerPurpose::ExportTeam,
                &nodes,
                path,
            );
            show_picker(
                &window,
                &mut st,
                session,
                "Export snippets (Team)",
                "Tick the folders and snippets you want to share. Ticking a folder selects everything inside it, and you can still uncheck individual items afterwards.",
                "Export...",
            );
        }
    });

    let weak_move_personal = main_window.as_weak();
    let state_move_personal = Rc::clone(&state);
    let hotkeys_move_personal = Rc::clone(&hotkeys);
    main_window.on_move_personal(move |from_idx, to_idx, into_folder| {
        let Some(window) = weak_move_personal.upgrade() else {
            return;
        };
        let mut st = state_move_personal.borrow_mut();
        let from_idx = from_idx as usize;
        let to_idx = to_idx as usize;
        let Some(src_path) = st.personal_paths.get(from_idx).cloned() else {
            window.set_status_text(i18n::tr("Invalid drag source").into());
            return;
        };
        let Some(target_path) = st.personal_paths.get(to_idx).cloned() else {
            window.set_status_text(i18n::tr("Invalid drop target").into());
            return;
        };
        // Was the *currently selected* node the one we're dragging? If so
        // we want to keep it selected after the move.
        let was_selected_drag = st.selected_personal == Some(from_idx);
        let target_is_folder = matches!(
            get_node_ref(&st.cfg.tree_personal, &target_path),
            Some(Node::Folder(_))
        );
        let Some(dest_path) =
            compute_move_destination(&src_path, &target_path, into_folder, target_is_folder)
        else {
            window.set_status_text(i18n::tr("Cannot drop here").into());
            return;
        };
        if !move_node_in_tree(&mut st.cfg.tree_personal, &src_path, dest_path) {
            window.set_status_text(i18n::tr("Move failed").into());
            return;
        }
        st.selected_personal = if was_selected_drag {
            None
        } else {
            st.selected_personal
        };
        refresh_personal_editor(&window, &mut st);
        let hotkey_warnings = install_hotkeys(&hotkeys_move_personal, &st.cfg, &st.team_tree)
            .filter(|w| !w.is_empty());
        if let Some(warnings) = hotkey_warnings {
            window.set_status_text(
                i18n::tr_format(
                    "Moved personal node; hotkey warnings: {0}",
                    &[&format!("{:?}", warnings)],
                )
                .into(),
            );
        } else {
            window.set_status_text(i18n::tr("Moved personal node").into());
        }
    });

    let weak_move_team = main_window.as_weak();
    let state_move_team = Rc::clone(&state);
    main_window.on_move_team(move |from_idx, to_idx, into_folder| {
        let Some(window) = weak_move_team.upgrade() else {
            return;
        };
        let mut st = state_move_team.borrow_mut();
        if st.edition != Edition::Admin {
            // Defense-in-depth: the Slint TreeRow drag callbacks are
            // already gated on `is_admin_edition`, but a stray
            // invocation (programmatic, future code) must still be a
            // no-op so users can never reorder the team tree locally
            // and accidentally drift from the share.
            window.set_status_text(
                i18n::tr("Team tree editing is only available in admin mode").into(),
            );
            return;
        }
        let from_idx = from_idx as usize;
        let to_idx = to_idx as usize;
        let Some(src_path) = st.team_paths.get(from_idx).cloned() else {
            window.set_status_text(i18n::tr("Invalid drag source").into());
            return;
        };
        let Some(target_path) = st.team_paths.get(to_idx).cloned() else {
            window.set_status_text(i18n::tr("Invalid drop target").into());
            return;
        };
        let target_is_folder = matches!(
            get_node_ref(&st.team_tree, &target_path),
            Some(Node::Folder(_))
        );
        let Some(dest_path) =
            compute_move_destination(&src_path, &target_path, into_folder, target_is_folder)
        else {
            window.set_status_text(i18n::tr("Cannot drop here").into());
            return;
        };
        if !move_node_in_tree(&mut st.team_tree, &src_path, dest_path) {
            window.set_status_text(i18n::tr("Move failed").into());
            return;
        }
        // Team tree changes only persist after Publish; refresh the local
        // view so the new ordering is visible immediately.
        refresh_team_editor(&window, &mut st);
        window.set_status_text(i18n::tr("Reordered team tree (publish to persist)").into());
    });

    let weak_delete = main_window.as_weak();
    let state_delete = Rc::clone(&state);
    let hotkeys_delete = Rc::clone(&hotkeys);
    main_window.on_delete_personal_selected(move || {
        if let Some(window) = weak_delete.upgrade() {
            let mut st = state_delete.borrow_mut();
            let Some(path) = st
                .selected_personal
                .and_then(|idx| st.personal_paths.get(idx))
                .cloned()
            else {
                window.set_status_text(i18n::tr("No personal node selected").into());
                return;
            };
            if remove_node_by_path(&mut st.cfg.tree_personal, &path) {
                st.selected_personal = None;
                refresh_personal_editor(&window, &mut st);
                let hotkey_warnings = install_hotkeys(&hotkeys_delete, &st.cfg, &st.team_tree)
                    .filter(|w| !w.is_empty());
                if let Some(warnings) = hotkey_warnings {
                    window.set_status_text(
                        i18n::tr_format(
                            "Deleted personal node; hotkey warnings: {0}",
                            &[&format!("{:?}", warnings)],
                        )
                        .into(),
                    );
                } else {
                    window.set_status_text(i18n::tr("Deleted selected personal node").into());
                }
            } else {
                window.set_status_text(i18n::tr("Failed to delete selected node").into());
            }
        }
    });

    let weak_import = main_window.as_weak();
    let state_import = Rc::clone(&state);
    main_window.on_import_personal(move || {
        if let Some(window) = weak_import.upgrade() {
            let Some(path) = rfd::FileDialog::new()
                .add_filter("JSON", &["json"])
                .set_title(&i18n::tr("Import Personal Tree JSON"))
                .pick_file()
            else {
                window.set_status_text(i18n::tr("Import cancelled").into());
                return;
            };
            let imported = match parse_import_tree(&path) {
                Ok(tree) => tree,
                Err(err) => {
                    window.set_status_text(
                        i18n::tr_format("Import failed: {0}", &[&err]).into(),
                    );
                    return;
                }
            };
            if imported.is_empty() {
                window.set_status_text(i18n::tr("No snippets found in file").into());
                return;
            }
            let mut st = state_import.borrow_mut();
            let session = picker::PickerSession::new(
                picker::PickerPurpose::ImportPersonal,
                &imported,
                path,
            );
            show_picker(
                &window,
                &mut st,
                session,
                "Import snippets (Personal)",
                "Tick what you'd like to pull in from this pack. Everything is selected by default; untick anything you want to skip.",
                "Import...",
            );
        }
    });

    let weak_export = main_window.as_weak();
    let state_export = Rc::clone(&state);
    main_window.on_export_personal(move || {
        if let Some(window) = weak_export.upgrade() {
            let nodes = {
                let st = state_export.borrow();
                if st.cfg.tree_personal.is_empty() {
                    window.set_status_text(i18n::tr("There's nothing to export yet.").into());
                    return;
                }
                st.cfg.tree_personal.clone()
            };
            let default_name = if nodes.len() == 1 {
                if let Node::Folder(f) = &nodes[0] {
                    format!("{}.poltergeist.json", f.name)
                } else {
                    "snippets.poltergeist.json".to_string()
                }
            } else {
                "snippets.poltergeist.json".to_string()
            };
            let Some(path) = rfd::FileDialog::new()
                .add_filter("JSON", &["json"])
                .set_file_name(&default_name)
                .set_title(&i18n::tr("Export snippets (Personal)"))
                .save_file()
            else {
                window.set_status_text(i18n::tr("Export cancelled").into());
                return;
            };
            let mut st = state_export.borrow_mut();
            let session = picker::PickerSession::new(
                picker::PickerPurpose::ExportPersonal,
                &nodes,
                path,
            );
            show_picker(
                &window,
                &mut st,
                session,
                "Export snippets (Personal)",
                "Tick the folders and snippets you want to share. Ticking a folder selects everything inside it, and you can still uncheck individual items afterwards.",
                "Export...",
            );
        }
    });

    let weak_apply = main_window.as_weak();
    let state_apply = Rc::clone(&state);
    let reload_base_apply = app_base.clone();
    let hotkeys_apply = Rc::clone(&hotkeys);
    let weak_preview_accent = main_window.as_weak();
    main_window.on_preview_options_accent(move |accent_hex| {
        let Some(window) = weak_preview_accent.upgrade() else {
            return;
        };
        let is_light = window.get_is_light_theme();
        let raw = accent_hex.to_string();
        let base = parse_color_hex(raw.trim()).unwrap_or_else(default_accent_base_color);
        window.set_options_accent_preview(base);
        apply_accent_theme(&window, base, is_light);
    });

    let weak_opt_panel = main_window.as_weak();
    let state_opt_panel = Rc::clone(&state);
    main_window.on_options_panel_visibility(move |visible| {
        let Some(window) = weak_opt_panel.upgrade() else {
            return;
        };
        let is_light = window.get_is_light_theme();
        let st = state_opt_panel.borrow();
        if visible {
            sync_options_accent_fields(&window, &st.cfg.settings);
            window.set_context_patterns_text(st.cfg.settings.context_patterns.join("; ").into());
        } else {
            apply_accent_from_settings(&window, &st.cfg.settings, is_light);
            window.set_context_patterns_text(st.cfg.settings.context_patterns.join("; ").into());
        }
    });

    main_window.on_apply_settings(
        move |hotkey,
              date_fmt,
              default_injection_idx,
              share,
              patterns,
              theme_idx,
              language_idx,
              deepl_key,
              autostart,
              accent_hex| {
            let mut st = state_apply.borrow_mut();
            let old_deepl_key = st.cfg.settings.deepl_api_key.clone();
            st.cfg.settings.hotkey = hotkey.trim().to_ascii_lowercase();
            st.cfg.settings.default_date_format = if date_fmt.trim().is_empty() {
                "%d/%m/%Y".to_string()
            } else {
                date_fmt.to_string()
            };
            st.cfg.settings.default_injection = default_injection_from_index(default_injection_idx);
            st.cfg.settings.theme = theme_from_index(theme_idx);
            st.cfg.settings.language = language_code_from_index(language_idx);
            // Slint flips bundled translations live; this updates every
            // `@tr(...)`-marked string in the UI without requiring an
            // app restart (Python had to restart Qt for the same).
            apply_bundled_translation(&st.cfg.settings.language);
            if let Some(window) = weak_apply.upgrade() {
                let (sw_top, sw_bottom) = build_color_swatch_row_pair();
                window.set_color_swatch_row_top(sw_top);
                window.set_color_swatch_row_bottom(sw_bottom);
            }
            // Keep parity with current Python rollout where autostart is intentionally disabled.
            st.cfg.settings.start_with_windows = false;
            if autostart {
                if let Some(window) = weak_apply.upgrade() {
                    window.set_status_text(
                        i18n::tr("Autostart is currently disabled pending security approval")
                            .into(),
                    );
                }
            }
            st.cfg.settings.deepl_api_key = deepl_key.trim().to_string();
            st.cfg.settings.team_share_path = share.trim().to_string();
            // The patterns field is a multi-line editor in the new UI;
            // accept newlines or semicolons as the separator so existing
            // configs continue to work.
            st.cfg.settings.context_patterns = patterns
                .split(|c: char| c == '\n' || c == ';')
                .map(str::trim)
                .filter(|p| !p.is_empty())
                .map(ToOwned::to_owned)
                .collect::<Vec<_>>();
            st.cfg.settings.accent_color = accent_color_option_from_picker_hex(accent_hex.as_str());
            let refreshed_pack = if st.edition == Edition::User {
                team_pack::read_pack_sync(&st.cfg.settings.team_share_path, &reload_base_apply)
            } else {
                team_pack::TeamPack {
                    tree: st.team_tree.clone(),
                    manifest: team_pack::TeamManifest::default(),
                    source: team_pack::probe_status(
                        &st.cfg.settings.team_share_path,
                        &reload_base_apply,
                    ),
                }
            };
            st.team_tree = refreshed_pack.tree;
            st.team_manifest_version = refreshed_pack.manifest.version;
            st.team_source = refreshed_pack.source;
            let share_path = st.cfg.settings.team_share_path.clone();
            let share_root = team_pack::share_root(&share_path);
            let cache_dir = team_pack::cache_dir(&reload_base_apply);
            let _ = st
                .db_registry
                .load_from_sources(share_root.as_deref(), Some(&cache_dir));
            let hotkey_warnings = install_hotkeys(&hotkeys_apply, &st.cfg, &st.team_tree)
                .filter(|warnings| !warnings.is_empty());
            if let Some(window) = weak_apply.upgrade() {
                window.set_default_injection_index(default_injection_index(
                    st.cfg.settings.default_injection,
                ));
                window.set_theme_index(theme_index(st.cfg.settings.theme));
                window.set_language_index(language_index_from_code(&st.cfg.settings.language));
                window.set_start_with_windows(st.cfg.settings.start_with_windows);
                st.is_light_theme = effective_is_light(st.cfg.settings.theme);
                window.set_is_light_theme(st.is_light_theme);
                apply_accent_from_settings(&window, &st.cfg.settings, st.is_light_theme);
                sync_options_accent_fields(&window, &st.cfg.settings);
                window.set_date_format_text(st.cfg.settings.default_date_format.clone().into());
                window.set_date_format_preview_text(
                    format_date_preview(&st.cfg.settings.default_date_format).into(),
                );
                window.set_deepl_api_key_text(st.cfg.settings.deepl_api_key.clone().into());
                let key = st.cfg.settings.deepl_api_key.clone();
                if key.trim().is_empty() {
                    window.set_deepl_status_text("No API key".into());
                    window.set_deepl_status_kind(0);
                } else if key != old_deepl_key {
                    match TranslationService::new(key.clone()) {
                        Ok(service) => match service.validate() {
                            Ok((ok, msg)) => {
                                window.set_deepl_status_text(msg.into());
                                window.set_deepl_status_kind(deepl_status_kind_from_msg(
                                    &key,
                                    Some(ok),
                                ));
                            }
                            Err(err) => {
                                window.set_deepl_status_text(
                                    format!("Validation error: {err}").into(),
                                );
                                window.set_deepl_status_kind(2);
                            }
                        },
                        Err(err) => {
                            window
                                .set_deepl_status_text(format!("DeepL setup failed: {err}").into());
                            window.set_deepl_status_kind(2);
                        }
                    }
                } else {
                    window.set_deepl_status_text("Not yet validated".into());
                    window.set_deepl_status_kind(0);
                }
                window.set_team_share_status_text(
                    share_status_text(st.team_source, st.team_manifest_version).into(),
                );
                window.set_team_share_status_kind(share_status_kind(st.team_source));
                refresh_team_editor(&window, &mut st);
                if let Some(warnings) = hotkey_warnings {
                    window.set_status_text(
                        i18n::tr_format(
                            "Applied settings; hotkey warnings: {0}",
                            &[&format!("{:?}", warnings)],
                        )
                        .into(),
                    );
                } else {
                    window.set_status_text(
                        i18n::tr("Applied settings and refreshed runtime state").into(),
                    );
                }
            }
            let _ = config::save(&reload_base_apply, &st.cfg);
        },
    );

    let weak_validate_deepl = main_window.as_weak();
    let state_validate_deepl = Rc::clone(&state);
    main_window.on_validate_deepl_key(move || {
        if let Some(window) = weak_validate_deepl.upgrade() {
            // Validate against the live edit field so the user gets
            // feedback even before pressing Save (matches Python's
            // `_run_validation` which reads `self._deepl_key.text()`).
            let key = window.get_deepl_api_key_text().trim().to_string();
            // Reflect the typed value into AppState so subsequent flows
            // (auto-save, restart, …) can see it; persistence still
            // requires explicit Save.
            state_validate_deepl.borrow_mut().cfg.settings.deepl_api_key = key.clone();
            run_deepl_validation(&window, &key, true);
        }
    });

    // ---- Debounced live validation -------------------------------
    //
    // The Python options dialog uses a 700ms `QTimer.singleShot` that
    // restarts on every keystroke. We replicate that with a single
    // `slint::Timer` that we re-arm on each `deepl_key_edited`
    // callback. The timer captures the latest typed key via a shared
    // `RefCell<String>` so the debounce always validates the most
    // recent text, even when intermediate edits are coalesced.
    let deepl_debounce_timer: Rc<Timer> = Rc::new(Timer::default());
    let deepl_pending_key: Rc<RefCell<String>> = Rc::new(RefCell::new(String::new()));
    let weak_deepl_edit = main_window.as_weak();
    let timer_deepl_edit = Rc::clone(&deepl_debounce_timer);
    let pending_deepl_edit = Rc::clone(&deepl_pending_key);
    let state_deepl_edit = Rc::clone(&state);
    main_window.on_deepl_key_edited(move |text| {
        let key = text.to_string();
        *pending_deepl_edit.borrow_mut() = key.clone();
        // Also write through to AppState so the on-disk Save path and
        // any subsequent translation request see the latest value
        // without requiring the user to press Save first.
        state_deepl_edit.borrow_mut().cfg.settings.deepl_api_key = key.trim().to_string();
        if let Some(window) = weak_deepl_edit.upgrade() {
            // Mirror the field-bound property explicitly — when the
            // user types via the bound TextInput Slint already
            // updates `value`, but pushing it through here keeps the
            // displayed value and AppState in sync if either side
            // ever drifts.
            window.set_deepl_api_key_text(key.clone().into());
            if key.trim().is_empty() {
                window.set_deepl_status_text("No API key".into());
                window.set_deepl_status_kind(0);
                timer_deepl_edit.stop();
                return;
            }
            window.set_deepl_status_text("Validating...".into());
            window.set_deepl_status_kind(0);
        }
        let weak_inner = weak_deepl_edit.clone();
        let pending_inner = Rc::clone(&pending_deepl_edit);
        timer_deepl_edit.start(
            TimerMode::SingleShot,
            Duration::from_millis(700),
            move || {
                let key_now = pending_inner.borrow().clone();
                if let Some(window) = weak_inner.upgrade() {
                    run_deepl_validation(&window, key_now.trim(), false);
                }
            },
        );
    });

    let weak_refresh_share = main_window.as_weak();
    let state_refresh_share = Rc::clone(&state);
    let hotkeys_refresh_share = Rc::clone(&hotkeys);
    let app_base_refresh_share = app_base.clone();
    main_window.on_refresh_team_pack(move || {
        if let Some(window) = weak_refresh_share.upgrade() {
            let mut st = state_refresh_share.borrow_mut();
            if st.edition == Edition::Admin {
                let choice = MessageDialog::new()
                    .set_level(MessageLevel::Warning)
                    .set_title(&i18n::tr("Refresh from share"))
                    .set_description(
                        &i18n::tr("This will replace the local Team tree with the latest version from the share. Any unpublished edits will be lost.\n\nContinue?"),
                    )
                    .set_buttons(MessageButtons::YesNo)
                    .show();
                if !matches!(choice, MessageDialogResult::Yes) {
                    window.set_status_text(i18n::tr("Refresh cancelled").into());
                    return;
                }
            }
            let refreshed_pack = team_pack::read_pack_sync(
                &st.cfg.settings.team_share_path,
                &app_base_refresh_share,
            );
            st.team_tree = refreshed_pack.tree;
            st.team_manifest_version = refreshed_pack.manifest.version;
            st.team_source = refreshed_pack.source;

            let share_root = team_pack::share_root(&st.cfg.settings.team_share_path);
            let cache_dir = team_pack::cache_dir(&app_base_refresh_share);
            let _ = st
                .db_registry
                .load_from_sources(share_root.as_deref(), Some(&cache_dir));

            window.set_team_share_status_text(
                share_status_text(st.team_source, st.team_manifest_version).into(),
            );
            window.set_team_share_status_kind(share_status_kind(st.team_source));
            refresh_team_editor(&window, &mut st);
            let hotkey_warnings = install_hotkeys(&hotkeys_refresh_share, &st.cfg, &st.team_tree)
                .filter(|w| !w.is_empty());
            if let Some(warnings) = hotkey_warnings {
                window.set_status_text(
                    i18n::tr_format(
                        "Refreshed team pack; hotkey warnings: {0}",
                        &[&format!("{:?}", warnings)],
                    )
                    .into(),
                );
            } else {
                window.set_status_text(i18n::tr("Refreshed team pack from share/cache").into());
            }
        }
    });

    let weak_publish_share = main_window.as_weak();
    let state_publish_share = Rc::clone(&state);
    let hotkeys_publish_share = Rc::clone(&hotkeys);
    let app_base_publish_share = app_base.clone();
    main_window.on_publish_team_pack(move || {
        if let Some(window) = weak_publish_share.upgrade() {
            let mut st = state_publish_share.borrow_mut();
            if st.edition != Edition::Admin {
                window.set_status_text(i18n::tr("Publish blocked: only admin edition may publish").into());
                return;
            }
            // Match the Python flow: short-circuit with an info popup when
            // there's no share path configured, *before* prompting for
            // confirmation. Avoids the ugly "are you sure?" -> "actually
            // nothing happened" sequence.
            let share = st.cfg.settings.team_share_path.trim().to_string();
            if share.is_empty() {
                drop(st);
                MessageDialog::new()
                    .set_level(MessageLevel::Info)
                    .set_title(&i18n::tr("Publish to share"))
                    .set_description(&i18n::tr("Set a share path first."))
                    .set_buttons(MessageButtons::Ok)
                    .show();
                window.set_status_text(
                    i18n::tr("Publish failed: no team share path configured").into(),
                );
                return;
            }
            let confirm = MessageDialog::new()
                .set_level(MessageLevel::Info)
                .set_title(&i18n::tr("Publish to share"))
                .set_description(
                    &i18n::tr("This will publish the current Team tree to the share. All users will pick it up on their next startup or refresh.\n\nContinue?"),
                )
                .set_buttons(MessageButtons::YesNo)
                .show();
            if !matches!(confirm, MessageDialogResult::Yes) {
                window.set_status_text(i18n::tr("Publish cancelled").into());
                return;
            }
            match team_pack::publish_to_share(
                &share,
                &app_base_publish_share,
                &st.team_tree,
                Some(st.team_manifest_version),
            ) {
                Ok(manifest) => {
                    st.team_manifest_version = manifest.version;
                    st.team_source = team_pack::ShareStatus::Reachable;
                    window.set_team_share_status_text(
                        share_status_text(st.team_source, st.team_manifest_version).into(),
                    );
                    window.set_team_share_status_kind(share_status_kind(st.team_source));
                    let hotkey_warnings =
                        install_hotkeys(&hotkeys_publish_share, &st.cfg, &st.team_tree)
                            .filter(|w| !w.is_empty());
                    let version = st.team_manifest_version;
                    let warnings_clone = hotkey_warnings.clone();
                    if let Some(warnings) = hotkey_warnings {
                        window.set_status_text(
                            i18n::tr_format(
                                "Published team pack v{0}; hotkey warnings: {1}",
                                &[&version, &format!("{:?}", warnings)],
                            )
                            .into(),
                        );
                    } else {
                        window
                            .set_status_text(i18n::tr_format("Published team pack v{0}", &[&version]).into());
                    }
                    notify_tray(
                        &app_base_publish_share,
                        &i18n::tr("Team pack published"),
                        &i18n::tr_format("Deployed pack v{0} to the share.", &[&version]),
                    );
                    // Drop the AppState borrow before showing the modal —
                    // rfd's blocking dialog can pump the Slint event loop
                    // and re-enter our handlers (e.g. status auto-clears).
                    drop(st);
                    let mut description = i18n::tr_format("Published pack v{0}.", &[&version]);
                    if let Some(warnings) = warnings_clone {
                        description.push_str("\n\n");
                        description.push_str(&i18n::tr("Hotkey warnings:"));
                        description.push('\n');
                        for (combo, msg) in warnings {
                            description.push_str("  - ");
                            description.push_str(&combo);
                            description.push_str(": ");
                            description.push_str(&msg);
                            description.push('\n');
                        }
                    }
                    MessageDialog::new()
                        .set_level(MessageLevel::Info)
                        .set_title(&i18n::tr("Publish to share"))
                        .set_description(description)
                        .set_buttons(MessageButtons::Ok)
                        .show();
                }
                Err(err) => {
                    let msg = format!("{err}");
                    window.set_status_text(
                        i18n::tr_format("Publish failed: {0}", &[&msg]).into(),
                    );
                    drop(st);
                    MessageDialog::new()
                        .set_level(MessageLevel::Warning)
                        .set_title(&i18n::tr("Publish failed"))
                        .set_description(msg)
                        .set_buttons(MessageButtons::Ok)
                        .show();
                }
            }
        }
    });

    let weak_theme = main_window.as_weak();
    let state_theme = Rc::clone(&state);
    main_window.on_toggle_theme(move || {
        if let Some(window) = weak_theme.upgrade() {
            let mut st = state_theme.borrow_mut();
            st.is_light_theme = !st.is_light_theme;
            st.cfg.settings.theme = if st.is_light_theme {
                ThemeMode::Light
            } else {
                ThemeMode::Dark
            };
            window.set_is_light_theme(st.is_light_theme);
            window.set_theme_index(theme_index(st.cfg.settings.theme));
            window.set_status_text(
                if st.is_light_theme {
                    "Switched to light theme"
                } else {
                    "Switched to dark theme"
                }
                .into(),
            );
            apply_accent_from_settings(&window, &st.cfg.settings, st.is_light_theme);
            refresh_personal_editor(&window, &mut st);
            refresh_team_editor(&window, &mut st);
        }
    });

    let weak_pause = main_window.as_weak();
    let hotkeys_pause = Rc::clone(&hotkeys);
    let tray_pause = Rc::clone(&tray);
    main_window.on_toggle_hotkeys(move || {
        if let Some(manager) = hotkeys_pause.borrow_mut().as_mut() {
            let paused_now = !manager.is_paused();
            let _ = manager.set_paused(paused_now);
            if let Some(tray_runtime) = tray_pause.borrow().as_ref() {
                tray_runtime.set_paused(paused_now);
            }
            if let Some(window) = weak_pause.upgrade() {
                window.set_hotkeys_paused(paused_now);
                window.set_status_text(
                    if paused_now {
                        "Hotkeys paused"
                    } else {
                        "Hotkeys resumed"
                    }
                    .into(),
                );
            }
        }
    });

    let weak_exit = main_window.as_weak();
    main_window.on_exit_app(move || {
        if let Some(window) = weak_exit.upgrade() {
            let _ = window.hide();
        }
        std::process::exit(0);
    });

    // Review-before-paste callbacks. The Slint modal closes itself
    // before invoking these, so we just resume or abort the pending
    // injection that `inject_snippet_now()` parked on AppState.
    let weak_review_ok = main_window.as_weak();
    let state_review_ok = Rc::clone(&state);
    main_window.on_confirm_review(move |edited| {
        let outcome = finalize_review_inject(&state_review_ok, edited.as_str());
        if let Some(window) = weak_review_ok.upgrade() {
            let msg = match outcome {
                Ok(s) => s,
                Err(s) => s,
            };
            window.set_status_text(SharedString::from(msg));
        }
    });
    let weak_review_cancel = main_window.as_weak();
    let state_review_cancel = Rc::clone(&state);
    main_window.on_cancel_review(move || {
        let mut st = state_review_cancel.borrow_mut();
        let name = st
            .pending_review
            .take()
            .map(|p| p.snippet_name)
            .unwrap_or_default();
        // Drop the captured target HWND too, since the user explicitly
        // cancelled — they don't want anything sent to that window.
        st.target_hwnd = None;
        drop(st);
        if let Some(window) = weak_review_cancel.upgrade() {
            let msg = if name.is_empty() {
                "Review cancelled".to_string()
            } else {
                format!("Cancelled review for '{name}'")
            };
            window.set_status_text(SharedString::from(msg));
        }
    });

    main_window.on_format_hotkey(move |text, ctrl, alt, shift, meta| {
        format_hotkey_event(text.as_str(), ctrl, alt, shift, meta).into()
    });

    main_window.on_open_github(move || {
        open_url_in_browser("https://github.com/iShark5060");
    });

    // Live preview for the Default date-format input. Recomputes
    // `chrono::Local::now().format(<fmt>)` each keystroke and pushes
    // the result back to the Slint side; invalid format strings show
    // "(invalid format)" — same wording as Python's
    // `_update_date_preview` exception branch.
    let weak_date_preview = main_window.as_weak();
    main_window.on_date_format_edited(move |text| {
        if let Some(window) = weak_date_preview.upgrade() {
            window.set_date_format_preview_text(format_date_preview(&text).into());
        }
    });

    // Generic browser-open callback used by the About modal's Icon
    // credits links. Centralised so the Slint side can hand us any
    // http(s) URL without us needing a dedicated callback per host.
    main_window.on_open_url(move |url| {
        let url = url.trim().to_string();
        if !(url.starts_with("http://") || url.starts_with("https://")) {
            // Drop quietly — never shell out to anything that isn't an
            // http(s) URL, otherwise the cmd /c start trick on Windows
            // would happily launch local executables.
            return;
        }
        open_url_in_browser(&url);
    });

    let weak_popup = main_window.as_weak();
    let state_popup = Rc::clone(&state);
    let popup_for_open = snippet_popup.clone_strong();
    main_window.on_open_popup(move || {
        if let Some(window) = weak_popup.upgrade() {
            let mut st = state_popup.borrow_mut();
            let all_nodes = st
                .cfg
                .tree_personal
                .iter()
                .chain(st.team_tree.iter())
                .cloned()
                .collect::<Vec<_>>();
            open_popup_for_nodes(
                &window,
                &popup_for_open,
                &mut st,
                &all_nodes,
                "in-app trigger",
            );
        }
    });

    let popup_for_close = snippet_popup.clone_strong();
    main_window.on_close_popup(move || {
        let _ = popup_for_close.hide();
    });

    // ---- Selective import/export picker callbacks ----
    let weak_picker_toggle = main_window.as_weak();
    let state_picker_toggle = Rc::clone(&state);
    main_window.on_picker_toggle_check(move |idx| {
        let Some(window) = weak_picker_toggle.upgrade() else {
            return;
        };
        let mut st = state_picker_toggle.borrow_mut();
        let path = st
            .picker_session
            .as_ref()
            .and_then(|s| s.visible_paths.get(idx as usize).cloned());
        let Some(path) = path else { return };
        if let Some(session) = st.picker_session.as_mut() {
            picker::toggle_check(&mut session.roots, &path);
        }
        refresh_picker_view(&window, &mut st);
    });

    let weak_picker_expand = main_window.as_weak();
    let state_picker_expand = Rc::clone(&state);
    main_window.on_picker_toggle_expand(move |idx| {
        let Some(window) = weak_picker_expand.upgrade() else {
            return;
        };
        let mut st = state_picker_expand.borrow_mut();
        let path = st
            .picker_session
            .as_ref()
            .and_then(|s| s.visible_paths.get(idx as usize).cloned());
        let Some(path) = path else { return };
        if let Some(session) = st.picker_session.as_mut() {
            picker::toggle_expand(&mut session.roots, &path);
        }
        refresh_picker_view(&window, &mut st);
    });

    let weak_picker_all = main_window.as_weak();
    let state_picker_all = Rc::clone(&state);
    main_window.on_picker_select_all(move || {
        let Some(window) = weak_picker_all.upgrade() else {
            return;
        };
        let mut st = state_picker_all.borrow_mut();
        if let Some(session) = st.picker_session.as_mut() {
            picker::set_all(&mut session.roots, picker::PickerCheck::Checked);
        }
        refresh_picker_view(&window, &mut st);
    });

    let weak_picker_none = main_window.as_weak();
    let state_picker_none = Rc::clone(&state);
    main_window.on_picker_clear_all(move || {
        let Some(window) = weak_picker_none.upgrade() else {
            return;
        };
        let mut st = state_picker_none.borrow_mut();
        if let Some(session) = st.picker_session.as_mut() {
            picker::set_all(&mut session.roots, picker::PickerCheck::Unchecked);
        }
        refresh_picker_view(&window, &mut st);
    });

    let weak_picker_cancel = main_window.as_weak();
    let state_picker_cancel = Rc::clone(&state);
    main_window.on_picker_cancel(move || {
        let Some(window) = weak_picker_cancel.upgrade() else {
            return;
        };
        let mut st = state_picker_cancel.borrow_mut();
        st.picker_session = None;
        window.set_picker_rows(empty_picker_rows_model());
        window.set_status_text(i18n::tr("Cancelled selection").into());
    });

    let weak_picker_accept = main_window.as_weak();
    let state_picker_accept = Rc::clone(&state);
    let hotkeys_picker_accept = Rc::clone(&hotkeys);
    main_window.on_picker_accept(move || {
        let Some(window) = weak_picker_accept.upgrade() else {
            return;
        };
        let mut st = state_picker_accept.borrow_mut();
        // Pull the selection out of the session. For exports we
        // immediately write the file and clear the session; for
        // imports we keep the session alive so the merge/replace
        // confirmation modal can use it.
        let (purpose, file_path, filtered) = {
            let Some(session) = st.picker_session.as_mut() else {
                return;
            };
            let filtered = picker::build_filtered(&session.roots);
            (session.purpose, session.file_path.clone(), filtered)
        };
        if filtered.is_empty() {
            window.set_status_text(i18n::tr("Nothing selected").into());
            st.picker_session = None;
            window.set_picker_rows(empty_picker_rows_model());
            return;
        }
        match purpose {
            picker::PickerPurpose::ExportPersonal | picker::PickerPurpose::ExportTeam => {
                let payload = serde_json::json!({"version": 1, "tree": filtered});
                let body = match serde_json::to_string_pretty(&payload) {
                    Ok(b) => b,
                    Err(err) => {
                        window.set_status_text(
                            i18n::tr_format("Export failed: {0}", &[&err]).into(),
                        );
                        st.picker_session = None;
                        window.set_picker_rows(empty_picker_rows_model());
                        return;
                    }
                };
                if let Err(err) = fs::write(&file_path, body) {
                    window.set_status_text(
                        i18n::tr_format("Export failed: {0}", &[&err]).into(),
                    );
                } else {
                    let label = if purpose == picker::PickerPurpose::ExportPersonal {
                        i18n::tr("Exported personal snippets")
                    } else {
                        i18n::tr("Exported team snippets")
                    };
                    window.set_status_text(
                        i18n::tr_format(
                            "{0} to {1}",
                            &[&label, &file_path.display()],
                        )
                        .into(),
                    );
                }
                st.picker_session = None;
                window.set_picker_rows(empty_picker_rows_model());
            }
            picker::PickerPurpose::ImportPersonal | picker::PickerPurpose::ImportTeam => {
                // Stash the filtered set on the session, then prompt
                // the user for merge / replace / cancel. The yes / no
                // / cancel callbacks below pull `pending_filtered`
                // back out and apply it.
                if let Some(session) = st.picker_session.as_mut() {
                    session.pending_filtered = Some(filtered.clone());
                }
                let count = filtered.len();
                drop(st);
                // The body uses {0} so translators can reorder the
                // count placement freely; the rest of the multi-line
                // explanation stays in source-string form.
                let body = i18n::tr_format(
                    "Import {0} top-level entries.\n\nYes = merge into current tree (as new top-level entries)\nNo  = REPLACE entire tree",
                    &[&count],
                );
                show_confirm(
                    &window,
                    "Import snippets",
                    &body,
                    "Merge",
                    Some("Replace"),
                    Some("Cancel"),
                    "import_apply",
                );
            }
        }
        let _ = &hotkeys_picker_accept;
    });

    // ---- Generic 3-way confirm dispatcher ----
    let weak_confirm_yes = main_window.as_weak();
    let state_confirm_yes = Rc::clone(&state);
    let hotkeys_confirm_yes = Rc::clone(&hotkeys);
    main_window.on_confirm_yes(move |kind| {
        let Some(window) = weak_confirm_yes.upgrade() else {
            return;
        };
        if kind.as_str() == "import_apply" {
            let mut st = state_confirm_yes.borrow_mut();
            let Some(session) = st.picker_session.take() else {
                return;
            };
            let mut filtered = session.pending_filtered.unwrap_or_default();
            poltergeist_core::models::regenerate_ids(&mut filtered);
            let count = filtered.len();
            let path_display = session.file_path.display().to_string();
            match session.purpose {
                picker::PickerPurpose::ImportPersonal => {
                    st.cfg.tree_personal.extend(filtered);
                    st.selected_personal = None;
                    refresh_personal_editor(&window, &mut st);
                }
                picker::PickerPurpose::ImportTeam => {
                    st.team_tree.extend(filtered);
                    st.selected_team = None;
                    refresh_team_editor(&window, &mut st);
                }
                _ => {}
            }
            let _ = install_hotkeys(&hotkeys_confirm_yes, &st.cfg, &st.team_tree);
            window.set_status_text(
                i18n::tr_format("Merged {0} entries from {1}", &[&count, &path_display]).into(),
            );
            window.set_picker_rows(empty_picker_rows_model());
        }
    });

    let weak_confirm_no = main_window.as_weak();
    let state_confirm_no = Rc::clone(&state);
    let hotkeys_confirm_no = Rc::clone(&hotkeys);
    main_window.on_confirm_no(move |kind| {
        let Some(window) = weak_confirm_no.upgrade() else {
            return;
        };
        if kind.as_str() == "import_apply" {
            let mut st = state_confirm_no.borrow_mut();
            let Some(session) = st.picker_session.take() else {
                return;
            };
            let mut filtered = session.pending_filtered.unwrap_or_default();
            poltergeist_core::models::regenerate_ids(&mut filtered);
            let count = filtered.len();
            let path_display = session.file_path.display().to_string();
            match session.purpose {
                picker::PickerPurpose::ImportPersonal => {
                    st.cfg.tree_personal = filtered;
                    st.selected_personal = None;
                    refresh_personal_editor(&window, &mut st);
                }
                picker::PickerPurpose::ImportTeam => {
                    st.team_tree = filtered;
                    st.selected_team = None;
                    refresh_team_editor(&window, &mut st);
                }
                _ => {}
            }
            let _ = install_hotkeys(&hotkeys_confirm_no, &st.cfg, &st.team_tree);
            window.set_status_text(
                i18n::tr_format(
                    "Replaced tree with {0} entries from {1}",
                    &[&count, &path_display],
                )
                .into(),
            );
            window.set_picker_rows(empty_picker_rows_model());
        }
    });

    let weak_confirm_cancel = main_window.as_weak();
    let state_confirm_cancel = Rc::clone(&state);
    main_window.on_confirm_cancel(move |kind| {
        let Some(window) = weak_confirm_cancel.upgrade() else {
            return;
        };
        if kind.as_str() == "import_apply" {
            let mut st = state_confirm_cancel.borrow_mut();
            st.picker_session = None;
            window.set_status_text(i18n::tr("Import cancelled").into());
            window.set_picker_rows(empty_picker_rows_model());
        }
    });

    // We deliberately use `run_event_loop_until_quit()` instead of
    // `MainWindow::run()` so that the loop keeps running after the user
    // hits the X button — the close handler returns `HideWindow` and
    // the app continues to live in the system tray. Only the explicit
    // tray "Exit" or in-app exit_app() call calls `std::process::exit`.
    if !should_start_hidden {
        let _ = main_window.show();
    }
    // First-run greeting — fire a single Win32 toast so the user knows
    // the app is alive in the tray when launching from an installer or
    // a freshly extracted zip. Skipping the snippet-count summary
    // here keeps the message short enough for Action Center; deeper
    // walkthrough lives in Options > About on demand.
    if first_run {
        let combo = state.borrow().cfg.settings.hotkey.clone();
        let combo_display = if combo.trim().is_empty() {
            i18n::tr("your hotkey")
        } else {
            combo.to_uppercase()
        };
        notify_tray(
            &app_base,
            &i18n::tr("Poltergeist is running"),
            &i18n::tr_format(
                "Press {0} to open the snippet popup. Right-click the tray icon for Options.",
                &[&combo_display],
            ),
        );
    }
    slint::run_event_loop_until_quit()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        build_token, build_translation_pair_token, extract_token_chips, format_hotkey_event,
        TRANSLATION_SOURCE_LANGS, TRANSLATION_TARGET_LANGS,
    };

    #[test]
    fn format_hotkey_basic_letter() {
        assert_eq!(format_hotkey_event("a", false, false, false, false), "a");
    }

    #[test]
    fn format_hotkey_letter_uppercased_lowercases() {
        assert_eq!(format_hotkey_event("A", false, false, false, false), "a");
    }

    #[test]
    fn format_hotkey_with_all_modifiers() {
        assert_eq!(
            format_hotkey_event("k", true, true, true, true),
            "ctrl+alt+shift+windows+k"
        );
    }

    #[test]
    fn format_hotkey_special_keys_named() {
        // Slint Key.F1 = U+F704
        assert_eq!(
            format_hotkey_event("\u{F704}", true, false, false, false),
            "ctrl+f1"
        );
        // Slint Key.Space = " "
        assert_eq!(
            format_hotkey_event(" ", true, true, false, false),
            "ctrl+alt+space"
        );
        // Slint Key.Escape = U+001B — treated as "esc"
        assert_eq!(
            format_hotkey_event("\u{001b}", false, false, false, false),
            "esc"
        );
        // Slint Key.UpArrow = U+F700
        assert_eq!(
            format_hotkey_event("\u{F700}", false, false, false, false),
            "up"
        );
    }

    #[test]
    fn format_hotkey_unmappable_is_empty() {
        assert_eq!(format_hotkey_event("", true, true, false, false), "");
        assert_eq!(
            format_hotkey_event("\u{ABCD}", true, false, false, false),
            ""
        );
    }

    #[test]
    fn format_hotkey_modifier_order_is_stable() {
        // Modifiers always appear in ctrl, alt, shift, windows order
        // regardless of which were toggled — keeps stored shortcuts
        // canonical so equality checks against existing JSON work.
        assert_eq!(
            format_hotkey_event("a", false, true, true, false),
            "alt+shift+a"
        );
        assert_eq!(
            format_hotkey_event("a", true, false, true, false),
            "ctrl+shift+a"
        );
    }

    #[test]
    fn extract_chips_empty() {
        assert!(extract_token_chips("").is_empty());
        assert!(extract_token_chips("hello world").is_empty());
    }

    #[test]
    fn extract_chips_categories() {
        let body = "Hi {DATE:%Y-%m-%d} use {CLIPBOARD} then {WAIT=200}{TAB}{CTRL+C}\
                    {VAR=name}{INCLUDE=hdr}{DATABASE=country}\
                    {IF kind == 'a'}A{ELSE}B{END}\
                    {TRANSLATION=DE}hi{TRANSLATION_END}";
        let chips = extract_token_chips(body);
        let cats: Vec<&str> = chips.iter().map(|c| c.category.as_str()).collect();
        assert_eq!(
            cats,
            vec![
                "date_clip",
                "date_clip",
                "wait",
                "key",
                "key",
                "var_include",
                "var_include",
                "database",
                "branch",
                "branch",
                "branch",
                "translation",
                "translation",
            ]
        );
    }

    #[test]
    fn extract_chips_case_insensitive() {
        let body = "{date}{Date:%H}{CLIPBOARD}";
        let chips = extract_token_chips(body);
        assert_eq!(chips.len(), 3);
        assert!(chips.iter().all(|c| c.category == "date_clip"));
    }

    #[test]
    fn extract_chips_in_document_order() {
        // {WAIT=10} comes before {DATE} in the body — make sure
        // sort is by start position, not category-rule order.
        let body = "{WAIT=10} ... {DATE} ... {WAIT=20}";
        let chips = extract_token_chips(body);
        let cats: Vec<&str> = chips.iter().map(|c| c.category.as_str()).collect();
        assert_eq!(cats, vec!["wait", "date_clip", "wait"]);
    }

    #[test]
    fn extract_chips_skips_unknown_tokens() {
        // {UNKNOWN} is not a recognised token; it must not produce a chip.
        let body = "{UNKNOWN} and {DATE}";
        let chips = extract_token_chips(body);
        assert_eq!(chips.len(), 1);
        assert_eq!(chips[0].category.as_str(), "date_clip");
    }

    #[test]
    fn build_token_date() {
        assert_eq!(build_token("date", "").unwrap(), "{DATE}");
        assert_eq!(build_token("date", "%Y-%m-%d").unwrap(), "{DATE:%Y-%m-%d}");
        // Whitespace around the format gets trimmed (matches Python).
        assert_eq!(build_token("date", "  %H:%M  ").unwrap(), "{DATE:%H:%M}");
    }

    #[test]
    fn build_token_wait() {
        assert_eq!(build_token("wait", "0").unwrap(), "{WAIT=0}");
        assert_eq!(build_token("wait", "250").unwrap(), "{WAIT=250}");
        assert_eq!(build_token("wait", "60000").unwrap(), "{WAIT=60000}");
        assert!(build_token("wait", "60001").is_err());
        assert!(build_token("wait", "abc").is_err());
        assert!(build_token("wait", "").is_err());
    }

    #[test]
    fn build_token_var_database_include_require_value() {
        for kind in ["var", "database", "include"] {
            assert!(build_token(kind, "").is_err(), "{} empty", kind);
            assert!(build_token(kind, "   ").is_err(), "{} whitespace", kind);
        }
        assert_eq!(build_token("var", "country").unwrap(), "{VAR=country}");
        assert_eq!(
            build_token("database", "Sites,$region-$site,INET").unwrap(),
            "{DATABASE=Sites,$region-$site,INET}"
        );
        assert_eq!(
            build_token("include", "Folder/Snippet").unwrap(),
            "{INCLUDE=Folder/Snippet}"
        );
    }

    #[test]
    fn build_token_custom_key_normalises() {
        assert_eq!(
            build_token("custom_key", "ctrl+shift+a").unwrap(),
            "{CTRL+SHIFT+A}"
        );
        assert_eq!(
            build_token("custom_key", "Ctrl+Alt+F4").unwrap(),
            "{CTRL+ALT+F4}"
        );
        // Stray plus signs and whitespace are squashed.
        assert_eq!(
            build_token("custom_key", " ctrl + + alt + space ").unwrap(),
            "{CTRL+ALT+SPACE}"
        );
        assert!(build_token("custom_key", "").is_err());
        assert!(build_token("custom_key", "   ").is_err());
        assert!(build_token("custom_key", "+ + +").is_err());
    }

    #[test]
    fn build_token_custom_translation() {
        assert_eq!(
            build_token("custom_translation", "en-us").unwrap(),
            "{TRANSLATION=EN-US}{TRANSLATION_END}"
        );
        assert_eq!(
            build_token("custom_translation", "  PT-br  ").unwrap(),
            "{TRANSLATION=PT-BR}{TRANSLATION_END}"
        );
        assert!(build_token("custom_translation", "").is_err());
    }

    #[test]
    fn build_token_unknown_kind_errors() {
        assert!(build_token("not-a-real-kind", "x").is_err());
    }

    #[test]
    fn translation_pair_auto_detect() {
        // source idx 0 = "Auto-detect" -> single-language token
        let token = build_translation_pair_token(0, 0).unwrap();
        assert_eq!(token, "{TRANSLATION=EN-US}{TRANSLATION_END}");
    }

    #[test]
    fn translation_pair_explicit_source() {
        // source idx 1 = first entry of TRANSLATION_SOURCE_LANGS = "BG"
        // target idx 2 = third entry of TRANSLATION_TARGET_LANGS = "DE"
        let token = build_translation_pair_token(1, 2).unwrap();
        assert_eq!(token, "{TRANSLATION=BG>DE}{TRANSLATION_END}");
    }

    #[test]
    fn translation_pair_invalid_indices_return_none() {
        assert!(build_translation_pair_token(99, 0).is_none());
        assert!(build_translation_pair_token(0, 99).is_none());
    }

    #[test]
    fn translation_pair_specific_combos() {
        // Sanity-check the Python-equivalent EN -> ES round trip.
        // source list contains EN at index 5 (zero-based), so the
        // Slint index is 5+1 = 6. Target list contains ES at idx 4.
        let en_idx = TRANSLATION_SOURCE_LANGS
            .iter()
            .position(|(c, _)| *c == "EN")
            .map(|i| (i + 1) as i32)
            .expect("EN in source list");
        let es_idx = TRANSLATION_TARGET_LANGS
            .iter()
            .position(|(c, _)| *c == "ES")
            .map(|i| i as i32)
            .expect("ES in target list");
        let token = build_translation_pair_token(en_idx, es_idx).unwrap();
        assert_eq!(token, "{TRANSLATION=EN>ES}{TRANSLATION_END}");
    }
}
