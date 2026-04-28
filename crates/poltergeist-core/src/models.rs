use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use uuid::Uuid;

fn new_id() -> String {
    Uuid::new_v4().simple().to_string()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum InjectionMode {
    #[default]
    Clipboard,
    ClipboardShiftInsert,
    Typing,
    TypingCompat,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ThemeMode {
    #[default]
    Auto,
    Light,
    Dark,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MatchOp {
    Eq,
    Ne,
    Contains,
    Regex,
    In,
    NotIn,
    Startswith,
    Endswith,
    Never,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MatchCondition {
    pub var: String,
    #[serde(default = "default_match_op")]
    pub op: MatchOp,
    #[serde(default)]
    pub value: String,
    #[serde(default)]
    pub optional: bool,
}

const fn default_match_op() -> MatchOp {
    MatchOp::Eq
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct MatchRule {
    #[serde(default)]
    pub conditions: Vec<MatchCondition>,
}

impl MatchRule {
    pub fn is_empty(&self) -> bool {
        self.conditions.is_empty()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Snippet {
    #[serde(default = "new_id")]
    pub id: String,
    #[serde(default = "default_snippet_name")]
    pub name: String,
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub injection: Option<InjectionMode>,
    #[serde(default = "default_true")]
    pub prompt_untranslated_before_paste: bool,
    #[serde(default)]
    pub color: Option<String>,
    #[serde(default)]
    pub r#match: Option<MatchRule>,
}

fn default_true() -> bool {
    true
}

fn default_snippet_name() -> String {
    "New Snippet".to_string()
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Folder {
    #[serde(default = "new_id")]
    pub id: String,
    #[serde(default = "default_folder_name")]
    pub name: String,
    #[serde(default)]
    pub children: Vec<Node>,
    #[serde(default)]
    pub color: Option<String>,
    #[serde(default)]
    pub shortcut: Option<String>,
    #[serde(default)]
    pub r#match: Option<MatchRule>,
}

fn default_folder_name() -> String {
    "New Folder".to_string()
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Node {
    Folder(Folder),
    Snippet(Snippet),
}

impl Node {
    pub fn id_mut(&mut self) -> &mut String {
        match self {
            Node::Folder(folder) => &mut folder.id,
            Node::Snippet(snippet) => &mut snippet.id,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Settings {
    #[serde(default = "default_hotkey")]
    pub hotkey: String,
    #[serde(default)]
    pub default_injection: InjectionMode,
    #[serde(default = "default_date_format")]
    pub default_date_format: String,
    #[serde(default)]
    pub start_with_windows: bool,
    #[serde(default)]
    pub theme: ThemeMode,
    #[serde(default)]
    pub deepl_api_key: String,
    #[serde(default)]
    pub language: String,
    #[serde(default)]
    pub team_share_path: String,
    #[serde(default)]
    pub team_shortcuts: HashMap<String, String>,
    #[serde(default)]
    pub context_patterns: Vec<String>,
    #[serde(default)]
    pub accent_color: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub main_window_width: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub main_window_height: Option<f64>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            hotkey: default_hotkey(),
            default_injection: InjectionMode::Clipboard,
            default_date_format: default_date_format(),
            start_with_windows: false,
            theme: ThemeMode::Auto,
            deepl_api_key: String::new(),
            language: String::new(),
            team_share_path: String::new(),
            team_shortcuts: HashMap::new(),
            context_patterns: Vec::new(),
            accent_color: None,
            main_window_width: None,
            main_window_height: None,
        }
    }
}

fn default_hotkey() -> String {
    "ctrl+alt+space".to_string()
}

fn default_date_format() -> String {
    "%d/%m/%Y".to_string()
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PoltergeistConfig {
    #[serde(default = "default_version")]
    pub version: i32,
    #[serde(default)]
    pub settings: Settings,
    #[serde(default)]
    pub tree_personal: Vec<Node>,
    #[serde(default)]
    pub tree_team: Vec<Node>,
}

fn default_version() -> i32 {
    2
}

impl Default for PoltergeistConfig {
    fn default() -> Self {
        Self {
            version: 2,
            settings: Settings::default(),
            tree_personal: Vec::new(),
            tree_team: Vec::new(),
        }
    }
}

pub fn regenerate_ids(nodes: &mut [Node]) {
    for node in nodes {
        *node.id_mut() = new_id();
        if let Node::Folder(folder) = node {
            regenerate_ids(&mut folder.children);
        }
    }
}

pub fn iter_snippets<'a>(nodes: &'a [Node], out: &mut Vec<&'a Snippet>) {
    for node in nodes {
        match node {
            Node::Snippet(snippet) => out.push(snippet),
            Node::Folder(folder) => iter_snippets(&folder.children, out),
        }
    }
}

pub fn match_rule_from_expr(text: &str) -> Option<MatchRule> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }

    let clause_re = Regex::new(
        r"(?i)^\s*(?P<var>[A-Za-z_][A-Za-z0-9_]*)\s*(?P<op>(?:!=|<>|==|=|not\s+in|not_in|!in|\bin\b|contains|regex|matches|startswith|endswith)\??)\s*(?P<value>.*?)\s*$",
    )
    .expect("valid clause regex");

    let mut conditions = Vec::new();
    for chunk in trimmed.split(';') {
        let clause = chunk.trim();
        if clause.is_empty() {
            continue;
        }
        if matches!(clause.to_lowercase().as_str(), "hide" | "never" | "no") {
            conditions.push(MatchCondition {
                var: "_never".to_string(),
                op: MatchOp::Never,
                value: String::new(),
                optional: false,
            });
            continue;
        }
        let caps = clause_re.captures(clause)?;
        let var = caps.name("var")?.as_str().to_string();
        let mut op_raw = caps.name("op")?.as_str().to_ascii_lowercase();
        op_raw = op_raw.split_whitespace().collect::<Vec<_>>().join(" ");
        let optional = op_raw.ends_with('?');
        if optional {
            op_raw.pop();
            op_raw = op_raw.trim().to_string();
        }
        let op = match op_raw.as_str() {
            "=" | "==" => MatchOp::Eq,
            "!=" | "<>" => MatchOp::Ne,
            "contains" => MatchOp::Contains,
            "regex" | "matches" => MatchOp::Regex,
            "in" => MatchOp::In,
            "not in" | "not_in" | "!in" => MatchOp::NotIn,
            "startswith" => MatchOp::Startswith,
            "endswith" => MatchOp::Endswith,
            _ => MatchOp::Eq,
        };
        let value = caps
            .name("value")
            .map(|m| m.as_str().to_string())
            .unwrap_or_default();
        conditions.push(MatchCondition {
            var,
            op,
            value,
            optional,
        });
    }

    if conditions.is_empty() {
        None
    } else {
        Some(MatchRule { conditions })
    }
}

pub fn match_rule_to_expr(rule: Option<&MatchRule>) -> String {
    let Some(rule) = rule else {
        return String::new();
    };
    if rule.is_empty() {
        return String::new();
    }

    let mut parts = Vec::new();
    for cond in &rule.conditions {
        if cond.op == MatchOp::Never {
            parts.push("hide".to_string());
            continue;
        }
        let mut op = match cond.op {
            MatchOp::Eq => "=",
            MatchOp::Ne => "!=",
            MatchOp::Contains => "contains",
            MatchOp::Regex => "regex",
            MatchOp::In => "in",
            MatchOp::NotIn => "not in",
            MatchOp::Startswith => "startswith",
            MatchOp::Endswith => "endswith",
            MatchOp::Never => "never",
        }
        .to_string();
        if cond.optional {
            op.push('?');
        }
        parts.push(
            format!("{} {} {}", cond.var, op, cond.value)
                .trim()
                .to_string(),
        );
    }
    parts.join("; ")
}
