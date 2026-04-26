use chrono::Local;
use once_cell::sync::Lazy;
use regex::Regex;
use std::collections::HashMap;

use crate::models::{MatchCondition, MatchOp, MatchRule};

pub type SnippetLookup<'a> = dyn Fn(&str) -> Option<String> + 'a;

pub trait DatabaseLookup {
    fn lookup(&self, db_name: &str, key: &str, column: Option<&str>) -> Option<String>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Segment {
    Text(String),
    Wait(u64),
    Key(String),
    Hotkey(String),
}

static TOKEN_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r"\{\{|\}\}|\{\s*(?P<name>[A-Za-z][A-Za-z0-9_]*(?:\s*\+\s*[A-Za-z][A-Za-z0-9_]*)*)(?:\s*[:=]\s*(?P<arg>[^{}]*))?\s*\}",
    )
    .expect("valid token regex")
});
static INCLUDE_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?i)\{\s*INCLUDE\s*[:=]\s*(?P<name>[^{}]+?)\s*\}").expect("valid include")
});
static IF_OPEN_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r"(?i)\{\s*IF\s+(?P<var>[A-Za-z_][A-Za-z0-9_]*)\s*(?P<op>(?:==|=|!=|<>|not\s+in|!in|\bin\b|contains|matches|regex|startswith|endswith)\??)\s*(?P<value>[^{}]*?)\s*\}",
    )
    .expect("valid if regex")
});
static ELSIF_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r"(?i)\{\s*(?:ELSIF|ELIF|ELSEIF)\s+(?P<var>[A-Za-z_][A-Za-z0-9_]*)\s*(?P<op>(?:==|=|!=|<>|not\s+in|!in|\bin\b|contains|matches|regex|startswith|endswith)\??)\s*(?P<value>[^{}]*?)\s*\}",
    )
    .expect("valid elsif regex")
});
static ELSE_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"(?i)\{\s*ELSE\s*\}").expect("valid else"));
static END_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"(?i)\{\s*END\s*\}").expect("valid end"));
static CONTEXT_VAR_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"\$(?P<name>[A-Za-z_][A-Za-z0-9_]*)").expect("valid var"));

const INCLUDE_MAX_DEPTH: usize = 8;

fn normalize_key(name: &str) -> String {
    let key = name.trim().to_ascii_uppercase();
    match key.as_str() {
        "CTRL" | "CONTROL" => "ctrl".to_string(),
        "ALT" => "alt".to_string(),
        "SHIFT" => "shift".to_string(),
        "WIN" | "WINDOWS" | "META" | "CMD" | "SUPER" => "windows".to_string(),
        "DEL" | "DELETE" => "delete".to_string(),
        "ESC" | "ESCAPE" => "esc".to_string(),
        "BACKSPACE" | "BKSP" => "backspace".to_string(),
        "SPACE" => "space".to_string(),
        "TAB" => "tab".to_string(),
        "ENTER" | "RETURN" => "enter".to_string(),
        "HOME" => "home".to_string(),
        "END" => "end".to_string(),
        "UP" => "up".to_string(),
        "DOWN" => "down".to_string(),
        "LEFT" => "left".to_string(),
        "RIGHT" => "right".to_string(),
        "PAGEUP" | "PGUP" => "page up".to_string(),
        "PAGEDOWN" | "PGDN" => "page down".to_string(),
        "INSERT" | "INS" => "insert".to_string(),
        "CAPS" | "CAPSLOCK" => "caps lock".to_string(),
        _ if key.starts_with('F')
            && key[1..].chars().all(|c| c.is_ascii_digit())
            && key[1..].len() <= 2 =>
        {
            key.to_ascii_lowercase()
        }
        _ if key.len() == 1 => key.to_ascii_lowercase(),
        _ => key.to_ascii_lowercase(),
    }
}

fn parse_key_token(raw_name: &str) -> Option<String> {
    let parts: Vec<_> = raw_name
        .split('+')
        .map(|p| p.trim())
        .filter(|p| !p.is_empty())
        .collect();
    if parts.is_empty() {
        return None;
    }
    if parts.len() == 1 {
        let upper = parts[0].to_ascii_uppercase();
        let named = matches!(
            upper.as_str(),
            "CTRL"
                | "CONTROL"
                | "ALT"
                | "SHIFT"
                | "WIN"
                | "WINDOWS"
                | "META"
                | "CMD"
                | "SUPER"
                | "DEL"
                | "DELETE"
                | "ESC"
                | "ESCAPE"
                | "BACKSPACE"
                | "BKSP"
                | "SPACE"
                | "TAB"
                | "ENTER"
                | "RETURN"
                | "HOME"
                | "END"
                | "UP"
                | "DOWN"
                | "LEFT"
                | "RIGHT"
                | "PAGEUP"
                | "PGUP"
                | "PAGEDOWN"
                | "PGDN"
                | "INSERT"
                | "INS"
                | "CAPS"
                | "CAPSLOCK"
        );
        let is_f = upper.starts_with('F')
            && upper[1..].chars().all(|c| c.is_ascii_digit())
            && upper[1..].len() <= 2;
        if named || is_f {
            return Some(normalize_key(parts[0]));
        }
        return None;
    }
    Some(
        parts
            .into_iter()
            .map(normalize_key)
            .collect::<Vec<_>>()
            .join("+"),
    )
}

fn parse_wait_ms(arg: Option<&str>) -> Option<u64> {
    let value = arg?.trim().parse::<i64>().ok()?;
    Some(value.max(0) as u64)
}

fn parse_repeat_count(arg: Option<&str>) -> Option<usize> {
    let Some(raw) = arg else {
        return Some(1);
    };
    let value = raw.trim().parse::<i64>().ok()?;
    Some(value.max(0) as usize)
}

fn format_date(fmt: &str) -> String {
    Local::now()
        .format(if fmt.is_empty() { "%d/%m/%Y" } else { fmt })
        .to_string()
}

fn substitute_context(raw: &str, context: Option<&HashMap<String, String>>) -> String {
    if !raw.contains('$') {
        return raw.to_string();
    }
    let escaped = raw.replace("$$", "\0");
    let replaced = CONTEXT_VAR_RE.replace_all(&escaped, |caps: &regex::Captures<'_>| {
        let name = caps.name("name").map(|m| m.as_str()).unwrap_or_default();
        context
            .and_then(|ctx| ctx.get(name))
            .cloned()
            .unwrap_or_default()
    });
    replaced.replace('\0', "$")
}

fn resolve_database(
    arg: Option<&str>,
    databases: Option<&dyn DatabaseLookup>,
    context: Option<&HashMap<String, String>>,
) -> String {
    let Some(arg) = arg else {
        return String::new();
    };
    let Some(db) = databases else {
        return String::new();
    };
    let parts: Vec<_> = arg.split(',').map(|p| p.trim()).collect();
    if parts.len() < 2 {
        return String::new();
    }
    let db_name = substitute_context(parts[0], context);
    let key = substitute_context(parts[1], context);
    let column = if parts.len() > 2 {
        Some(substitute_context(parts[2], context))
    } else {
        None
    };
    if db_name.is_empty() || key.is_empty() {
        return String::new();
    }
    db.lookup(&db_name, &key, column.as_deref())
        .unwrap_or_default()
}

fn resolve_scalar_token(
    name: &str,
    arg: Option<&str>,
    default_date_format: &str,
    clipboard_text: &str,
    context: Option<&HashMap<String, String>>,
    databases: Option<&dyn DatabaseLookup>,
) -> Option<String> {
    match name.to_ascii_uppercase().as_str() {
        "DATE" => Some(format_date(arg.unwrap_or(default_date_format))),
        "CLIPBOARD" => Some(clipboard_text.to_string()),
        "VAR" => {
            let key = arg.unwrap_or("").trim();
            Some(
                context
                    .and_then(|ctx| ctx.get(key))
                    .cloned()
                    .unwrap_or_default(),
            )
        }
        "DATABASE" => Some(resolve_database(arg, databases, context)),
        _ => None,
    }
}

pub fn split_alternatives(value: &str) -> Vec<String> {
    value
        .split(['|', ','])
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

pub fn evaluate_condition(
    var: &str,
    op: &str,
    value: &str,
    context: Option<&HashMap<String, String>>,
) -> bool {
    let got = context
        .and_then(|ctx| ctx.get(var))
        .cloned()
        .unwrap_or_default();
    let mut op_norm = op.trim().to_ascii_lowercase();
    op_norm = op_norm.split_whitespace().collect::<Vec<_>>().join(" ");
    let optional = op_norm.ends_with('?');
    if optional {
        op_norm.pop();
        op_norm = op_norm.trim().to_string();
    }

    if op_norm == "never" {
        return false;
    }
    if optional && got.trim().is_empty() {
        return true;
    }

    match op_norm.as_str() {
        "=" | "==" | "eq" | "in" => {
            let alts = split_alternatives(value);
            let candidates = if alts.is_empty() {
                vec![value.trim().to_string()]
            } else {
                alts
            };
            let got_norm = got.trim().to_ascii_lowercase();
            candidates
                .iter()
                .any(|v| got_norm == v.to_ascii_lowercase())
        }
        "!=" | "<>" | "ne" | "not in" | "not_in" | "!in" => {
            let alts = split_alternatives(value);
            let candidates = if alts.is_empty() {
                vec![value.trim().to_string()]
            } else {
                alts
            };
            let got_norm = got.trim().to_ascii_lowercase();
            candidates
                .iter()
                .all(|v| got_norm != v.to_ascii_lowercase())
        }
        "contains" => got
            .to_ascii_lowercase()
            .contains(&value.trim().to_ascii_lowercase()),
        "startswith" => got
            .to_ascii_lowercase()
            .starts_with(&value.trim().to_ascii_lowercase()),
        "endswith" => got
            .to_ascii_lowercase()
            .ends_with(&value.trim().to_ascii_lowercase()),
        "matches" | "regex" => Regex::new(&format!("(?i)^(?:{})$", value))
            .map(|re| re.is_match(&got))
            .unwrap_or(false),
        _ => false,
    }
}

pub fn evaluate_match_rule(
    rule: Option<&MatchRule>,
    context: Option<&HashMap<String, String>>,
) -> bool {
    let Some(rule) = rule else {
        return true;
    };
    if rule.conditions.is_empty() {
        return true;
    }
    rule.conditions.iter().all(|cond: &MatchCondition| {
        let op = match cond.op {
            MatchOp::Eq => "=",
            MatchOp::Ne => "!=",
            MatchOp::Contains => "contains",
            MatchOp::Regex => "regex",
            MatchOp::In => "in",
            MatchOp::NotIn => "not in",
            MatchOp::Startswith => "startswith",
            MatchOp::Endswith => "endswith",
            MatchOp::Never => "never",
        };
        let op = if cond.optional {
            format!("{op}?")
        } else {
            op.to_string()
        };
        evaluate_condition(&cond.var, &op, &cond.value, context)
    })
}

pub fn expand_includes(text: &str, snippet_lookup: Option<&SnippetLookup<'_>>) -> String {
    expand_includes_depth(text, snippet_lookup, 0)
}

fn expand_includes_depth(
    text: &str,
    snippet_lookup: Option<&SnippetLookup<'_>>,
    depth: usize,
) -> String {
    if text.is_empty()
        || snippet_lookup.is_none()
        || !text.contains('{')
        || depth >= INCLUDE_MAX_DEPTH
    {
        return text.to_string();
    }
    let lookup = snippet_lookup.expect("checked above");
    INCLUDE_RE
        .replace_all(text, |caps: &regex::Captures<'_>| {
            let name = caps
                .name("name")
                .map(|m| m.as_str().trim())
                .unwrap_or_default();
            if name.is_empty() {
                return String::new();
            }
            let body = lookup(name).unwrap_or_default();
            expand_includes_depth(&body, snippet_lookup, depth + 1)
        })
        .to_string()
}

pub fn expand_conditionals(text: &str, context: Option<&HashMap<String, String>>) -> String {
    if text.is_empty() || !text.contains('{') {
        return text.to_string();
    }

    let mut out = String::new();
    let mut pos = 0usize;
    while pos < text.len() {
        let Some(m_if) = IF_OPEN_RE.find_at(text, pos) else {
            out.push_str(&text[pos..]);
            break;
        };
        out.push_str(&text[pos..m_if.start()]);
        let Some((branches, end_match)) = find_block_bounds(text, m_if.end()) else {
            out.push_str(&text[m_if.start()..]);
            break;
        };

        let mut branch_defs: Vec<BranchDef> = Vec::new();
        let if_caps = IF_OPEN_RE.captures(&text[m_if.start()..m_if.end()]);
        if let Some(caps) = if_caps {
            branch_defs.push(BranchDef {
                kind: BranchKind::If,
                condition: Some(ConditionDef {
                    var: caps
                        .name("var")
                        .map(|m| m.as_str().to_string())
                        .unwrap_or_default(),
                    op: caps
                        .name("op")
                        .map(|m| m.as_str().to_string())
                        .unwrap_or_default(),
                    value: caps
                        .name("value")
                        .map(|m| m.as_str().to_string())
                        .unwrap_or_default(),
                }),
                body_start: m_if.end(),
                marker_start: m_if.start(),
            });
        }
        for branch in branches {
            branch_defs.push(branch);
        }

        let mut chosen = String::new();
        for i in 0..branch_defs.len() {
            let branch = &branch_defs[i];
            let body_end = if i + 1 < branch_defs.len() {
                branch_defs[i + 1].marker_start
            } else {
                end_match.start()
            };
            if branch.kind == BranchKind::Else {
                chosen = text[branch.body_start..body_end].to_string();
                break;
            }
            if let Some(cond) = &branch.condition {
                if evaluate_condition(&cond.var, &cond.op, &cond.value, context) {
                    chosen = text[branch.body_start..body_end].to_string();
                    break;
                }
            } else if branch.kind == BranchKind::If {
                // A malformed IF marker should not crash parsing.
                chosen = text[branch.body_start..body_end].to_string();
                break;
            }
        }
        if !chosen.is_empty() {
            out.push_str(&expand_conditionals(&chosen, context));
        }
        pos = end_match.end();
    }
    out
}

fn find_block_bounds<'a>(
    text: &'a str,
    start: usize,
) -> Option<(Vec<BranchDef>, regex::Match<'a>)> {
    let mut depth = 1usize;
    let mut pos = start;
    let mut branches = Vec::new();

    while pos < text.len() {
        let m_if = IF_OPEN_RE.find_at(text, pos);
        let m_elsif = ELSIF_RE.find_at(text, pos);
        let m_else = ELSE_RE.find_at(text, pos);
        let m_end = END_RE.find_at(text, pos);

        let mut candidates: Vec<(usize, &str, regex::Match<'a>)> = Vec::new();
        if let Some(m) = m_if {
            candidates.push((m.start(), "if", m));
        }
        if let Some(m) = m_elsif {
            candidates.push((m.start(), "elsif", m));
        }
        if let Some(m) = m_else {
            candidates.push((m.start(), "else", m));
        }
        if let Some(m) = m_end {
            candidates.push((m.start(), "end", m));
        }
        if candidates.is_empty() {
            return None;
        }
        candidates.sort_by_key(|c| c.0);
        let (_, tag, next_match) = candidates[0];
        match tag {
            "if" => {
                depth += 1;
                pos = next_match.end();
            }
            "elsif" => {
                if depth == 1 {
                    if let Some(caps) =
                        ELSIF_RE.captures(&text[next_match.start()..next_match.end()])
                    {
                        branches.push(BranchDef {
                            kind: BranchKind::Elsif,
                            condition: Some(ConditionDef {
                                var: caps
                                    .name("var")
                                    .map(|m| m.as_str().to_string())
                                    .unwrap_or_default(),
                                op: caps
                                    .name("op")
                                    .map(|m| m.as_str().to_string())
                                    .unwrap_or_default(),
                                value: caps
                                    .name("value")
                                    .map(|m| m.as_str().to_string())
                                    .unwrap_or_default(),
                            }),
                            body_start: next_match.end(),
                            marker_start: next_match.start(),
                        });
                    }
                }
                pos = next_match.end();
            }
            "else" => {
                if depth == 1 {
                    branches.push(BranchDef {
                        kind: BranchKind::Else,
                        condition: None,
                        body_start: next_match.end(),
                        marker_start: next_match.start(),
                    });
                }
                pos = next_match.end();
            }
            "end" => {
                depth -= 1;
                if depth == 0 {
                    return Some((branches, next_match));
                }
                pos = next_match.end();
            }
            _ => return None,
        }
    }
    None
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum BranchKind {
    If,
    Elsif,
    Else,
}

#[derive(Debug, Clone)]
struct ConditionDef {
    var: String,
    op: String,
    value: String,
}

#[derive(Debug, Clone)]
struct BranchDef {
    kind: BranchKind,
    condition: Option<ConditionDef>,
    body_start: usize,
    marker_start: usize,
}

pub fn has_wait_or_key_tokens(text: &str) -> bool {
    TOKEN_RE.captures_iter(text).any(|caps| {
        let raw = caps.get(0).map(|m| m.as_str()).unwrap_or_default();
        if raw == "{{" || raw == "}}" {
            return false;
        }
        let name = caps.name("name").map(|m| m.as_str()).unwrap_or_default();
        if name.eq_ignore_ascii_case("WAIT") {
            return true;
        }
        parse_key_token(name).is_some()
    })
}

pub fn expand_for_clipboard(
    text: &str,
    default_date_format: &str,
    clipboard_text: &str,
    context: Option<&HashMap<String, String>>,
    databases: Option<&dyn DatabaseLookup>,
    snippet_lookup: Option<&SnippetLookup<'_>>,
) -> String {
    let prepared = expand_conditionals(&expand_includes(text, snippet_lookup), context);
    TOKEN_RE
        .replace_all(&prepared, |caps: &regex::Captures<'_>| {
            let raw = caps.get(0).map(|m| m.as_str()).unwrap_or_default();
            if raw == "{{" {
                return "{".to_string();
            }
            if raw == "}}" {
                return "}".to_string();
            }
            let name = caps.name("name").map(|m| m.as_str()).unwrap_or_default();
            let arg = caps.name("arg").map(|m| m.as_str());
            if name.eq_ignore_ascii_case("WAIT") || parse_key_token(name).is_some() {
                return String::new();
            }
            resolve_scalar_token(
                name,
                arg,
                default_date_format,
                clipboard_text,
                context,
                databases,
            )
            .unwrap_or_else(|| raw.to_string())
        })
        .to_string()
}

pub fn expand_for_clipboard_segments(
    text: &str,
    default_date_format: &str,
    clipboard_text: &str,
    context: Option<&HashMap<String, String>>,
    databases: Option<&dyn DatabaseLookup>,
    snippet_lookup: Option<&SnippetLookup<'_>>,
) -> Vec<Segment> {
    let prepared = expand_conditionals(&expand_includes(text, snippet_lookup), context);
    let mut segments = Vec::new();
    let mut buf = String::new();
    let mut pos = 0usize;

    let flush = |segments: &mut Vec<Segment>, buf: &mut String| {
        if !buf.is_empty() {
            segments.push(Segment::Text(buf.clone()));
            buf.clear();
        }
    };

    for caps in TOKEN_RE.captures_iter(&prepared) {
        let m = caps.get(0).expect("full match");
        buf.push_str(&prepared[pos..m.start()]);
        let raw = m.as_str();
        if raw == "{{" {
            buf.push('{');
        } else if raw == "}}" {
            buf.push('}');
        } else {
            let name = caps.name("name").map(|v| v.as_str()).unwrap_or_default();
            let arg = caps.name("arg").map(|v| v.as_str());
            if name.eq_ignore_ascii_case("WAIT") {
                if let Some(ms) = parse_wait_ms(arg) {
                    flush(&mut segments, &mut buf);
                    segments.push(Segment::Wait(ms));
                } else {
                    buf.push_str(raw);
                }
            } else if let Some(combo) = parse_key_token(name) {
                if let Some(repeats) = parse_repeat_count(arg) {
                    flush(&mut segments, &mut buf);
                    for _ in 0..repeats {
                        segments.push(Segment::Hotkey(combo.clone()));
                    }
                } else {
                    buf.push_str(raw);
                }
            } else if let Some(value) = resolve_scalar_token(
                name,
                arg,
                default_date_format,
                clipboard_text,
                context,
                databases,
            ) {
                buf.push_str(&value);
            } else {
                buf.push_str(raw);
            }
        }
        pos = m.end();
    }
    buf.push_str(&prepared[pos..]);
    flush(&mut segments, &mut buf);
    segments
}

pub fn expand_for_typing(
    text: &str,
    default_date_format: &str,
    clipboard_text: &str,
    context: Option<&HashMap<String, String>>,
    databases: Option<&dyn DatabaseLookup>,
    snippet_lookup: Option<&SnippetLookup<'_>>,
) -> Vec<Segment> {
    let mut out = Vec::new();
    for seg in expand_for_clipboard_segments(
        text,
        default_date_format,
        clipboard_text,
        context,
        databases,
        snippet_lookup,
    ) {
        match seg {
            Segment::Hotkey(combo) if combo == "tab" => out.push(Segment::Key("tab".to_string())),
            Segment::Hotkey(combo) if combo == "enter" => {
                out.push(Segment::Key("enter".to_string()))
            }
            Segment::Text(text) if text.contains('\n') => {
                let mut first = true;
                for part in text.split('\n') {
                    if !first {
                        out.push(Segment::Key("enter".to_string()));
                    }
                    first = false;
                    if !part.is_empty() {
                        out.push(Segment::Text(part.to_string()));
                    }
                }
            }
            other => out.push(other),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::collections::HashMap;

    struct TestDb {
        data: BTreeMap<(String, String, String), String>,
    }

    impl DatabaseLookup for TestDb {
        fn lookup(&self, db_name: &str, key: &str, column: Option<&str>) -> Option<String> {
            let column = column.unwrap_or("").to_ascii_lowercase();
            self.data
                .get(&(
                    db_name.to_ascii_lowercase(),
                    key.to_ascii_lowercase(),
                    column,
                ))
                .cloned()
        }
    }

    #[test]
    fn conditional_resolves_expected_branch() {
        let mut ctx = HashMap::new();
        ctx.insert("country".to_string(), "DE".to_string());
        let src = "{IF country in FR,BE}Bonjour{ELSIF country = DE}Hallo{ELSE}Hello{END}";
        assert_eq!(expand_conditionals(src, Some(&ctx)), "Hallo");
    }

    #[test]
    fn include_and_var_expand_for_clipboard() {
        let mut ctx = HashMap::new();
        ctx.insert("site".to_string(), "123".to_string());
        let out = expand_for_clipboard(
            "{INCLUDE=body}",
            "%Y-%m-%d",
            "clip",
            Some(&ctx),
            None,
            Some(&|name| {
                if name.eq_ignore_ascii_case("body") {
                    Some("Site {VAR=site}".to_string())
                } else {
                    None
                }
            }),
        );
        assert_eq!(out, "Site 123");
    }

    #[test]
    fn typing_segments_split_newlines() {
        let segments = expand_for_typing("A\nB", "%Y-%m-%d", "", None, None, None);
        assert_eq!(
            segments,
            vec![
                Segment::Text("A".to_string()),
                Segment::Key("enter".to_string()),
                Segment::Text("B".to_string()),
            ]
        );
    }

    #[test]
    fn database_and_context_substitution_resolve() {
        let mut ctx = HashMap::new();
        ctx.insert("region".to_string(), "123".to_string());
        ctx.insert("site".to_string(), "456".to_string());
        let mut data = BTreeMap::new();
        data.insert(
            (
                "sites".to_string(),
                "123-456".to_string(),
                "inet_solution".to_string(),
            ),
            "Fiber".to_string(),
        );
        let db = TestDb { data };
        let out = expand_for_clipboard(
            "{DATABASE=Sites,$region-$site,INET_SOLUTION}",
            "%Y-%m-%d",
            "",
            Some(&ctx),
            Some(&db),
            None,
        );
        assert_eq!(out, "Fiber");
    }
}
