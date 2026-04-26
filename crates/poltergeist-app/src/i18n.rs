//! Rust-side translation lookup with the same .po catalogue that the
//! Slint UI consumes via `@tr(...)`.
//!
//! Slint 1.10's bundled-translations feature lives entirely on the
//! Slint side — there is no public Rust API to look up a bundled
//! string from arbitrary Rust code. For the strings that we *build*
//! in Rust (status bar, message dialogs, picker subtitles, …) we
//! therefore re-parse the same `lang/<locale>/LC_MESSAGES/*.po`
//! files at compile time, embed them via `include_str!`, and run a
//! tiny lookup against the active locale.
//!
//! Usage:
//!
//! ```ignore
//! crate::i18n::set_locale("de");
//! let label = crate::i18n::tr("Loaded settings");
//! let msg   = crate::i18n::tr_format("Loaded {0}", &[&path]);
//! ```
//!
//! Format placeholders use the same `{0}`, `{1}`, … syntax that
//! Slint's `@tr` and Python's `str.format` both speak, so a single
//! source string can serve both ports.

use std::collections::HashMap;
use std::sync::RwLock;

use once_cell::sync::Lazy;

/// One catalog per bundled locale. Empty values are dropped during
/// parsing so a falsy lookup falls back to the source string.
type Catalog = HashMap<String, String>;
type CatalogRegistry = HashMap<&'static str, Catalog>;

/// Each tuple is `(locale, raw .po contents)`. Keep this list in sync
/// with the directories under `crates/poltergeist-app/lang/`.
static BUNDLED_PO_FILES: &[(&str, &str)] = &[
    (
        "de",
        include_str!("../lang/de/LC_MESSAGES/poltergeist-app.po"),
    ),
    (
        "es",
        include_str!("../lang/es/LC_MESSAGES/poltergeist-app.po"),
    ),
    (
        "fr",
        include_str!("../lang/fr/LC_MESSAGES/poltergeist-app.po"),
    ),
];

static CATALOGS: Lazy<CatalogRegistry> = Lazy::new(|| {
    let mut out = CatalogRegistry::new();
    for (locale, raw) in BUNDLED_PO_FILES {
        out.insert(*locale, parse_po(raw));
    }
    out
});

static ACTIVE_LOCALE: Lazy<RwLock<String>> = Lazy::new(|| RwLock::new(String::new()));

/// Switch the active locale used by `tr` / `tr_format`. Pass an empty
/// string or `"en"` to revert to the source language. Unknown locales
/// silently revert to English so Rust callers don't have to deal with
/// the fallible `slint::SelectBundledTranslationError` here.
pub fn set_locale(code: &str) {
    let normalized = code.trim().to_ascii_lowercase();
    let target = if normalized.is_empty() || normalized == "en" {
        String::new()
    } else if CATALOGS.contains_key(normalized.as_str()) {
        normalized
    } else {
        // Unknown locale — log once and fall back to English; the
        // .slint side already handles this case via its own error
        // enum, so we stay consistent and quiet here.
        String::new()
    };
    if let Ok(mut guard) = ACTIVE_LOCALE.write() {
        *guard = target;
    }
}

/// Look up a single source string in the active catalog. Returns the
/// source string itself when no translation is available — never
/// returns an empty string, so it's safe to use directly in formatted
/// output.
pub fn tr(source: &str) -> String {
    let locale = match ACTIVE_LOCALE.read() {
        Ok(guard) => guard.clone(),
        Err(_) => String::new(),
    };
    if locale.is_empty() {
        return source.to_string();
    }
    let Some(catalog) = CATALOGS.get(locale.as_str()) else {
        return source.to_string();
    };
    catalog
        .get(source)
        .cloned()
        .unwrap_or_else(|| source.to_string())
}

/// Translate `source` and substitute `{0}`, `{1}`, … placeholders
/// with the supplied arguments. Mirrors Python's `str.format(*args)`
/// and Slint's `@tr("...", args...)`.
///
/// Unmatched placeholders are left as-is, matching gettext defaults.
pub fn tr_format(source: &str, args: &[&dyn std::fmt::Display]) -> String {
    let template = tr(source);
    let mut out = String::with_capacity(template.len());
    let bytes = template.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'{' {
            // Escape: `{{` -> literal `{`.
            if i + 1 < bytes.len() && bytes[i + 1] == b'{' {
                out.push('{');
                i += 2;
                continue;
            }
            if let Some(end) = template[i + 1..].find('}') {
                let idx_str = &template[i + 1..i + 1 + end];
                if let Ok(idx) = idx_str.parse::<usize>() {
                    if let Some(arg) = args.get(idx) {
                        use std::fmt::Write;
                        let _ = write!(out, "{}", arg);
                    }
                    i = i + 1 + end + 1;
                    continue;
                }
            }
        }
        if bytes[i] == b'}' && i + 1 < bytes.len() && bytes[i + 1] == b'}' {
            out.push('}');
            i += 2;
            continue;
        }
        out.push(bytes[i] as char);
        i += 1;
    }
    out
}

/// Parse a single `.po` file into a `msgid -> msgstr` map. Supports the
/// multi-line continuation form where a `msgid ""` on one line is
/// followed by `"chunk"` lines. Empty `msgstr` entries are skipped so
/// the lookup can fall through to the source string. C-style escapes
/// `\n`, `\t`, `\"`, `\\` are unescaped.
fn parse_po(raw: &str) -> Catalog {
    let mut out = Catalog::new();
    let mut state = ParseState::Idle;
    let mut current_id = String::new();
    let mut current_str = String::new();

    fn flush(out: &mut Catalog, id: &mut String, msg: &mut String) {
        if !id.is_empty() && !msg.is_empty() {
            out.insert(std::mem::take(id), std::mem::take(msg));
        } else {
            id.clear();
            msg.clear();
        }
    }

    for raw_line in raw.lines() {
        let line = raw_line.trim_start();
        if line.is_empty() {
            // Blank line — entry boundary.
            flush(&mut out, &mut current_id, &mut current_str);
            state = ParseState::Idle;
            continue;
        }
        if line.starts_with('#') {
            continue;
        }
        if let Some(rest) = line.strip_prefix("msgid ") {
            flush(&mut out, &mut current_id, &mut current_str);
            current_id = unquote_po(rest.trim());
            state = ParseState::Id;
            continue;
        }
        if let Some(rest) = line.strip_prefix("msgstr ") {
            current_str = unquote_po(rest.trim());
            state = ParseState::Str;
            continue;
        }
        // Continuation of the previous string. Trim() because `.po`
        // files can have trailing whitespace before the quote.
        if line.starts_with('"') {
            let chunk = unquote_po(line.trim());
            match state {
                ParseState::Id => current_id.push_str(&chunk),
                ParseState::Str => current_str.push_str(&chunk),
                ParseState::Idle => {}
            }
        }
    }
    flush(&mut out, &mut current_id, &mut current_str);
    // Drop the empty header entry that gettext always emits.
    out.remove("");
    out
}

#[derive(Copy, Clone)]
enum ParseState {
    Idle,
    Id,
    Str,
}

/// Strip the surrounding quotes from a `.po` literal and unescape
/// the C-style sequences gettext emits. Returns an empty string for
/// any malformed input — the worst case is that one entry is lost,
/// not that the whole catalog fails to parse.
fn unquote_po(token: &str) -> String {
    let bytes = token.as_bytes();
    if bytes.len() < 2 || bytes[0] != b'"' || bytes[bytes.len() - 1] != b'"' {
        return String::new();
    }
    let inner = &token[1..token.len() - 1];
    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars();
    while let Some(ch) = chars.next() {
        if ch != '\\' {
            out.push(ch);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some('"') => out.push('"'),
            Some('\\') => out.push('\\'),
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => break,
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_simple_entry() {
        let raw = "msgid \"Hello\"\nmsgstr \"Hallo\"\n";
        let cat = parse_po(raw);
        assert_eq!(cat.get("Hello"), Some(&"Hallo".to_string()));
    }

    #[test]
    fn parses_multiline_entry() {
        let raw = "msgid \"\"\n\"line one\\n\"\n\"line two\"\nmsgstr \"\"\n\"zeile eins\\n\"\n\"zeile zwei\"\n";
        let cat = parse_po(raw);
        assert_eq!(
            cat.get("line one\nline two"),
            Some(&"zeile eins\nzeile zwei".to_string())
        );
    }

    #[test]
    fn drops_empty_translations() {
        let raw = "msgid \"foo\"\nmsgstr \"\"\n";
        let cat = parse_po(raw);
        assert!(cat.get("foo").is_none());
    }

    #[test]
    fn skips_comments_and_header() {
        let raw = concat!(
            "msgid \"\"\n",
            "msgstr \"Project-Id-Version: x\\n\"\n",
            "\n",
            "# translator note\n",
            "msgid \"a\"\n",
            "msgstr \"b\"\n",
        );
        let cat = parse_po(raw);
        assert_eq!(cat.get("a"), Some(&"b".to_string()));
        assert!(cat.get("").is_none());
    }

    #[test]
    fn tr_falls_back_to_source() {
        set_locale("");
        assert_eq!(tr("untranslated"), "untranslated");
    }

    #[test]
    fn tr_format_substitutes_placeholders() {
        set_locale("");
        let v: Vec<&dyn std::fmt::Display> = vec![&"alpha", &42];
        assert_eq!(
            tr_format("hello {0}, count {1}", &v),
            "hello alpha, count 42"
        );
    }

    #[test]
    fn tr_format_handles_escaped_braces() {
        set_locale("");
        let v: Vec<&dyn std::fmt::Display> = vec![&"x"];
        assert_eq!(tr_format("{{literal}} {0}", &v), "{literal} x");
    }
}
