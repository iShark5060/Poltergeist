use regex::Regex;
use std::collections::HashMap;
use std::fs;
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IconStyle {
    Solid,
    Regular,
    Brands,
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct IconSpec {
    pub key: String,
    pub label: String,
    pub style: IconStyle,
    pub unicode_hex: String,
}

impl IconSpec {
    pub fn glyph(&self) -> Option<char> {
        u32::from_str_radix(self.unicode_hex.trim(), 16)
            .ok()
            .and_then(char::from_u32)
    }
}

#[derive(Debug, Default, Clone)]
pub struct IconRegistry {
    icons: HashMap<String, IconSpec>,
}

impl IconRegistry {
    pub fn from_substitution_file(path: &Path) -> anyhow::Result<Self> {
        let body = fs::read_to_string(path)?;
        Ok(parse_substitution_text(&body))
    }

    pub fn get(&self, key: &str) -> Option<&IconSpec> {
        self.icons.get(&key.to_ascii_lowercase())
    }

    pub fn len(&self) -> usize {
        self.icons.len()
    }

    /// Returns the configured glyph for `key`, or `fallback` if missing/unmappable.
    #[allow(dead_code)]
    pub fn glyph_or(&self, key: &str, fallback: char) -> char {
        self.get(key).and_then(|spec| spec.glyph()).unwrap_or(fallback)
    }

    /// Returns a single-character `String` carrying the glyph for `key` (empty if missing).
    #[allow(dead_code)]
    pub fn glyph_string(&self, key: &str) -> String {
        self.get(key)
            .and_then(|spec| spec.glyph())
            .map(|c| c.to_string())
            .unwrap_or_default()
    }

    /// Returns whether the icon belongs to the FontAwesome Brands family
    /// (so the UI knows which family to render with).
    #[allow(dead_code)]
    pub fn is_brands(&self, key: &str) -> bool {
        matches!(self.get(key).map(|spec| spec.style), Some(IconStyle::Brands))
    }
}

fn parse_substitution_text(body: &str) -> IconRegistry {
    let line_re = Regex::new(
        r"(?i)^\s*(?P<key>[A-Za-z0-9_\-\.]+)\.png\s*->\s*(?P<label>.*?)\s*->\s*(?P<hex>[A-Fa-f0-9]{4,6})\s*\((?P<style>[^)]*)\)\s*$",
    )
    .expect("valid regex");
    let mut icons = HashMap::new();
    for line in body.lines() {
        let Some(caps) = line_re.captures(line) else {
            continue;
        };
        let key = caps
            .name("key")
            .map(|m| m.as_str().trim().to_ascii_lowercase())
            .unwrap_or_default();
        if key.is_empty() {
            continue;
        }
        let style_raw = caps
            .name("style")
            .map(|m| m.as_str().to_ascii_lowercase())
            .unwrap_or_default();
        let style = if style_raw.contains("brand") {
            IconStyle::Brands
        } else if style_raw.contains("regular") {
            IconStyle::Regular
        } else {
            IconStyle::Solid
        };
        let spec = IconSpec {
            key: key.clone(),
            label: caps
                .name("label")
                .map(|m| m.as_str().trim().to_string())
                .unwrap_or_default(),
            style,
            unicode_hex: caps
                .name("hex")
                .map(|m| m.as_str().trim().to_ascii_lowercase())
                .unwrap_or_default(),
        };
        icons.insert(key, spec);
    }
    IconRegistry { icons }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_substitution_rows() {
        let source = "add.png -> Add Snippet -> f15b (Classic Regular)";
        let registry = parse_substitution_text(source);
        let add = registry.get("add").expect("add icon");
        assert_eq!(add.unicode_hex, "f15b");
        assert!(add.glyph().is_some());
    }
}
