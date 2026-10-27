//! Private, user-owned speech hints. These bias recognition; they never rewrite
//! an existing transcript or send notes to a language-model service.
use crate::store::{atomic_write, Paths, Transcript};
use anyhow::{bail, Context, Result};
use fs2::FileExt;
use moonshine_rs::TranscriberOptions;
use serde::{Deserialize, Serialize};
use std::{fs, io::Read};

const MAX_INPUT: usize = 16_384;
const MAX_TERMS: usize = 64;
const MAX_CONTEXT: usize = 8_192;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Settings {
    pub terms: Vec<String>,
    pub use_meeting_context: bool,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            terms: vec![],
            use_meeting_context: true,
        }
    }
}

pub fn parse_terms(input: &str) -> Result<Vec<String>> {
    if input.len() > MAX_INPUT {
        bail!("Vocabulary is limited to 16 KB");
    }
    let mut terms = Vec::new();
    for raw in input.split([',', '\n', '\r']) {
        let term = raw.trim();
        if term.is_empty() {
            continue;
        }
        if term.chars().any(char::is_control) || term.chars().count() > 100 {
            bail!("Each vocabulary term must be at most 100 characters with no control characters");
        }
        let term = term.split_whitespace().collect::<Vec<_>>().join(" ");
        if !terms
            .iter()
            .any(|t: &String| t.to_lowercase() == term.to_lowercase())
        {
            terms.push(term);
        }
    }
    if terms.len() > MAX_TERMS {
        bail!("Use at most 64 vocabulary terms");
    }
    Ok(terms)
}

pub fn load(p: &Paths) -> Result<Settings> {
    let file = match fs::File::open(p.root.join("vocabulary.json")) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Settings::default()),
        Err(_) => bail!("Could not read local vocabulary settings"),
    };
    let mut bytes = Vec::new();
    file.take((MAX_INPUT * 2 + 1) as u64)
        .read_to_end(&mut bytes)?;
    if bytes.len() > MAX_INPUT * 2 {
        bail!("Local vocabulary settings are too large");
    }
    let mut settings: Settings = serde_json::from_slice(&bytes).map_err(|_| {
        anyhow::anyhow!("Invalid local vocabulary settings; save them again to repair")
    })?;
    settings.terms = parse_terms(&settings.terms.join("\n"))?;
    Ok(settings)
}

/// Partial CLI updates are serialized with full UI replacements. Supplying
/// both fields can repair an invalid file without having to read it first.
pub fn update(p: &Paths, terms: Option<&str>, use_context: Option<bool>) -> Result<Settings> {
    if terms.is_none() && use_context.is_none() {
        bail!("Provide --terms or --use-context");
    }
    let lock = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(p.root.join("vocabulary.lock"))?;
    lock.lock_exclusive()?;
    let mut settings = if terms.is_some() && use_context.is_some() {
        Settings::default()
    } else {
        load(p)?
    };
    if let Some(terms) = terms {
        settings.terms = parse_terms(terms)?;
    }
    if let Some(enabled) = use_context {
        settings.use_meeting_context = enabled;
    }
    atomic_write(
        &p.root.join("vocabulary.json"),
        &serde_json::to_vec_pretty(&settings)?,
    )
    .context("Could not save local vocabulary settings")?;
    Ok(settings)
}

fn bounded_excerpt(text: &str, budget: usize) -> String {
    if text.len() <= budget {
        return text.to_owned();
    }
    let mut first = budget / 2 - 3;
    let mut last = text.len() - (budget / 2 - 3);
    while !text.is_char_boundary(first) {
        first -= 1;
    }
    while !text.is_char_boundary(last) {
        last += 1;
    }
    format!("{}\n…\n{}", &text[..first], &text[last..])
}

pub fn context(settings: &Settings, transcript: &Transcript) -> String {
    if !settings.use_meeting_context {
        return String::new();
    }
    let mut text = String::new();
    // Avoid feeding an earlier machine-generated title back into recognition.
    if transcript.meeting.title_origin == "user" && transcript.meeting.title != "Untitled meeting" {
        text.push_str(&transcript.meeting.title);
        text.push('\n');
    }
    text.push_str(transcript.notes.trim());
    // Native options are C strings. Pasted control bytes must not prevent an
    // otherwise valid meeting from transcribing.
    let text = text.replace(|c: char| c.is_control() && c != '\n' && c != '\t', " ");
    bounded_excerpt(&text, MAX_CONTEXT)
}

pub fn options(settings: &Settings, context: &str) -> TranscriberOptions {
    let mut options = TranscriberOptions::new().with_identify_speakers(false);
    if !settings.terms.is_empty() {
        options = options
            .with_keyterms(settings.terms.join(","))
            .with_keyterm_boost(2.0);
    }
    if !context.is_empty() {
        options = options.with_context(context).with_context_max_terms(32);
    }
    options
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn normalize_validate_and_bound_terms() -> Result<()> {
        assert_eq!(
            parse_terms(" SampleName,\nsamplename\nÉlodie  Martin\r\n")?,
            vec!["SampleName", "Élodie Martin"]
        );
        assert!(parse_terms("secret\0value").is_err());
        assert!(parse_terms(&"x".repeat(101)).is_err());
        assert!(parse_terms(
            &(0..65)
                .map(|n| format!("Term{n}"))
                .collect::<Vec<_>>()
                .join(",")
        )
        .is_err());
        assert!(parse_terms(&"x".repeat(MAX_INPUT + 1)).is_err());
        Ok(())
    }
    #[test]
    fn settings_persist_partial_updates_and_repair() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let p = Paths::new(Some(dir.path().into()))?;
        assert_eq!(load(&p)?, Settings::default());
        update(&p, Some("SampleName,Example Product"), None)?;
        update(&p, None, Some(false))?;
        let s = load(&p)?;
        assert_eq!(s.terms, vec!["SampleName", "Example Product"]);
        assert!(!s.use_meeting_context);
        fs::write(p.root.join("vocabulary.json"), "invalid private text")?;
        let error = load(&p).unwrap_err().to_string();
        assert!(!error.contains("private text"));
        assert!(update(&p, Some("bad\0term"), Some(true)).is_err());
        update(&p, Some(""), Some(true))?;
        assert_eq!(load(&p)?, Settings::default());
        Ok(())
    }
    #[test]
    fn notes_and_manual_title_are_context_but_prior_transcript_is_not() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let p = Paths::new(Some(dir.path().into()))?;
        let id = p.create("Product planning", "draft")?;
        p.save_notes(&id, "Product planning", "Discuss Example Product")?;
        let mut t = p.get(&id)?;
        let settings = Settings::default();
        assert_eq!(
            context(&settings, &t),
            "Product planning\nDiscuss Example Product"
        );
        t.meeting.title_origin = "on_device_model".into();
        assert_eq!(context(&settings, &t), "Discuss Example Product");
        let off = Settings {
            use_meeting_context: false,
            ..settings
        };
        assert_eq!(context(&off, &t), "");
        assert!(options(&off, "").get("context").is_none());
        let s = Settings {
            terms: vec!["Example Product".into()],
            ..off
        };
        let opts = options(&s, "");
        assert_eq!(opts.get("keyterms"), Some("Example Product"));
        assert_eq!(opts.get("keyterm_boost"), Some("2"));
        let opts = options(&Settings::default(), "Example agenda");
        assert_eq!(opts.get("context"), Some("Example agenda"));
        assert_eq!(opts.get("context_max_terms"), Some("32"));
        t.notes = "Pasted\0agenda".into();
        assert_eq!(context(&Settings::default(), &t), "Pasted agenda");
        t.notes = format!("Opening topic {} Closing topic", "😀".repeat(20_000));
        let excerpt = context(&Settings::default(), &t);
        assert!(excerpt.len() <= MAX_CONTEXT);
        assert!(excerpt.starts_with("Opening topic") && excerpt.ends_with("Closing topic"));
        Ok(())
    }
}
