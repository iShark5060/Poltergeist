use anyhow::Context;
use poltergeist_core::tokens::{expand_for_clipboard, DatabaseLookup, SnippetLookup};
use regex::Regex;
use reqwest::blocking::Client;
use serde::Deserialize;
use std::collections::HashMap;
use std::time::Duration;

#[derive(Debug, thiserror::Error)]
pub enum TranslationError {
    #[error("No API key configured")]
    MissingApiKey,
    #[error("Translation API request failed: {0}")]
    Request(String),
    #[error("Translation API returned no output")]
    EmptyResult,
}

#[derive(Debug, Clone)]
pub struct TranslationService {
    api_key: String,
    client: Client,
}

impl TranslationService {
    pub fn new(api_key: impl Into<String>) -> anyhow::Result<Self> {
        let client = Client::builder()
            .timeout(Duration::from_secs(20))
            .build()
            .context("failed to build HTTP client")?;
        Ok(Self {
            api_key: api_key.into().trim().to_string(),
            client,
        })
    }

    pub fn set_api_key(&mut self, api_key: impl Into<String>) {
        self.api_key = api_key.into().trim().to_string();
    }

    pub fn validate(&self) -> anyhow::Result<(bool, String)> {
        if self.api_key.is_empty() {
            return Ok((false, "No API key".to_string()));
        }
        let response = self
            .client
            .post("https://api-free.deepl.com/v2/usage")
            .form(&[("auth_key", self.api_key.as_str())])
            .send()
            .context("deepl usage request failed")?;
        if response.status().is_success() {
            let usage: UsageResponse = response.json().context("failed to parse usage response")?;
            let summary = if let (Some(count), Some(limit)) =
                (usage.character_count, usage.character_limit)
            {
                let pct = if limit > 0 {
                    (count as f64 / limit as f64) * 100.0
                } else {
                    0.0
                };
                format!("OK - {count} / {limit} chars ({pct:.1}%)")
            } else {
                "OK".to_string()
            };
            return Ok((true, summary));
        }
        if response.status().as_u16() == 403 {
            return Ok((false, "Invalid API key".to_string()));
        }
        Ok((
            false,
            format!("DeepL error: HTTP {}", response.status().as_u16()),
        ))
    }

    pub fn translate_plain_text(
        &self,
        text: &str,
        source_lang: Option<&str>,
        target_lang: &str,
    ) -> Result<String, TranslationError> {
        if self.api_key.is_empty() {
            return Err(TranslationError::MissingApiKey);
        }
        let mut form = vec![
            ("auth_key", self.api_key.as_str()),
            ("text", text),
            ("target_lang", target_lang),
        ];
        if let Some(source) = source_lang {
            form.push(("source_lang", source));
        }
        let response = self
            .client
            .post("https://api-free.deepl.com/v2/translate")
            .form(&form)
            .send()
            .map_err(|e| TranslationError::Request(e.to_string()))?;
        if !response.status().is_success() {
            return Err(TranslationError::Request(format!(
                "HTTP {}",
                response.status().as_u16()
            )));
        }
        let payload: TranslateResponse = response
            .json()
            .map_err(|e| TranslationError::Request(e.to_string()))?;
        let translated = payload
            .translations
            .first()
            .map(|v| v.text.clone())
            .ok_or(TranslationError::EmptyResult)?;
        Ok(translated)
    }

    pub fn text_has_translations(text: &str) -> bool {
        translation_regex().is_match(text)
    }

    pub fn translation_pairs_in_text(text: &str) -> Vec<(Option<String>, String)> {
        translation_regex()
            .captures_iter(text)
            .map(|caps| {
                let src = caps.name("src").map(|m| m.as_str().to_ascii_uppercase());
                let tgt = caps
                    .name("tgt")
                    .map(|m| m.as_str().to_ascii_uppercase())
                    .unwrap_or_default();
                (src, tgt)
            })
            .collect()
    }

    pub fn uniform_expanded_translation_body_if_any(
        text: &str,
        default_date_format: &str,
        clipboard_text: &str,
        context: Option<&HashMap<String, String>>,
        databases: Option<&dyn DatabaseLookup>,
        snippet_lookup: Option<&SnippetLookup<'_>>,
    ) -> Option<String> {
        let regex = translation_regex();
        let mut bodies: Vec<String> = Vec::new();
        for caps in regex.captures_iter(text) {
            let body = caps.name("body").map(|m| m.as_str()).unwrap_or_default();
            let expanded = expand_for_clipboard(
                body,
                default_date_format,
                clipboard_text,
                context,
                databases,
                snippet_lookup,
            );
            bodies.push(expanded);
        }
        let first = bodies.first()?;
        if bodies.iter().all(|b| b == first) {
            Some(first.clone())
        } else {
            None
        }
    }

    pub fn expand_translation_sources(
        text: &str,
        default_date_format: &str,
        clipboard_text: &str,
        context: Option<&HashMap<String, String>>,
        databases: Option<&dyn DatabaseLookup>,
        snippet_lookup: Option<&SnippetLookup<'_>>,
    ) -> String {
        let regex = translation_regex();
        let mut out = String::new();
        let mut last = 0usize;
        for caps in regex.captures_iter(text) {
            let Some(full) = caps.get(0) else {
                continue;
            };
            out.push_str(&text[last..full.start()]);
            let body = caps.name("body").map(|m| m.as_str()).unwrap_or_default();
            let expanded = expand_for_clipboard(
                body,
                default_date_format,
                clipboard_text,
                context,
                databases,
                snippet_lookup,
            );
            out.push_str(&expanded);
            last = full.end();
        }
        out.push_str(&text[last..]);
        out
    }

    #[allow(clippy::too_many_arguments)]
    pub fn expand_translations(
        &self,
        text: &str,
        default_date_format: &str,
        clipboard_text: &str,
        context: Option<&HashMap<String, String>>,
        databases: Option<&dyn DatabaseLookup>,
        snippet_lookup: Option<&SnippetLookup<'_>>,
        body_override: Option<&str>,
    ) -> Result<String, TranslationError> {
        let regex = translation_regex();
        let mut matches = Vec::new();
        for caps in regex.captures_iter(text) {
            let Some(full) = caps.get(0) else {
                continue;
            };
            let src = caps.name("src").map(|m| m.as_str().to_ascii_uppercase());
            let tgt = caps
                .name("tgt")
                .map(|m| m.as_str().to_ascii_uppercase())
                .unwrap_or_default();
            let body = caps.name("body").map(|m| m.as_str()).unwrap_or_default();
            let expanded_body = body_override.map(ToOwned::to_owned).unwrap_or_else(|| {
                expand_for_clipboard(
                    body,
                    default_date_format,
                    clipboard_text,
                    context,
                    databases,
                    snippet_lookup,
                )
            });
            matches.push((full.start(), full.end(), src, tgt, expanded_body));
        }

        if matches.is_empty() {
            return Ok(text.to_string());
        }

        let mut translated_by_index = Vec::with_capacity(matches.len());
        for (_, _, src, tgt, body) in &matches {
            let translated = self.translate_plain_text(body, src.as_deref(), tgt)?;
            translated_by_index.push(translated);
        }

        let mut out = String::new();
        let mut last = 0usize;
        for (idx, (start, end, _, _, _)) in matches.iter().enumerate() {
            out.push_str(&text[last..*start]);
            out.push_str(&translated_by_index[idx]);
            last = *end;
        }
        out.push_str(&text[last..]);
        Ok(out)
    }
}

#[derive(Debug, Deserialize)]
struct UsageResponse {
    character_count: Option<i64>,
    character_limit: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct TranslateResponse {
    translations: Vec<TranslateItem>,
}

#[derive(Debug, Deserialize)]
struct TranslateItem {
    text: String,
}

fn translation_regex() -> &'static Regex {
    static TRANSLATION_RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    TRANSLATION_RE.get_or_init(|| {
        Regex::new(r"(?is)\{TRANSLATION\s*[:=]\s*(?:(?P<src>[A-Za-z]{2})>)?(?P<tgt>[A-Za-z]{2}(?:-[A-Za-z]{2})?)\}(?P<body>.*?)\{TRANSLATION_END\}")
            .expect("valid translation regex")
    })
}
