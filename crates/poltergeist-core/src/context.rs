use regex::Regex;
use std::collections::HashMap;

pub fn parse(text: &str, patterns: &[String]) -> HashMap<String, String> {
    let mut ctx = HashMap::new();
    ctx.insert("_full".to_string(), text.to_string());

    for pattern in patterns {
        if pattern.trim().is_empty() {
            continue;
        }
        let Ok(compiled) = Regex::new(pattern) else {
            continue;
        };
        let Some(caps) = compiled.captures(text) else {
            continue;
        };
        for name in compiled.capture_names().flatten() {
            let value = caps
                .name(name)
                .map(|m| m.as_str().to_string())
                .unwrap_or_default();
            ctx.insert(name.to_string(), value);
        }
        if let Some(raw) = caps.get(0) {
            ctx.insert("_raw".to_string(), raw.as_str().to_string());
        }
        return ctx;
    }
    ctx
}

pub fn validate(pattern: &str) -> Option<String> {
    Regex::new(pattern).err().map(|e| e.to_string())
}

pub fn merge(contexts: &[HashMap<String, String>]) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for ctx in contexts {
        for (k, v) in ctx {
            out.insert(k.clone(), v.clone());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_first_match_and_named_groups() {
        let patterns = vec![
            r"(?P<country>[A-Z]{2})-(?P<site>\d+)".to_string(),
            r"(?P<foo>.*)".to_string(),
        ];
        let ctx = parse("DE-123", &patterns);
        assert_eq!(ctx.get("country").map(String::as_str), Some("DE"));
        assert_eq!(ctx.get("site").map(String::as_str), Some("123"));
        assert_eq!(ctx.get("_raw").map(String::as_str), Some("DE-123"));
    }
}
