//! Local automatic titles. Apple Intelligence is optional; the fallback is an
//! extract from the transcript, not an AI summary. No model is downloaded here.
use crate::store::{Paths, Segment};
use anyhow::Result;
use serde::Deserialize;
use std::{
    collections::HashMap,
    io::Write,
    path::PathBuf,
    process::{Command, Stdio},
    time::{Duration, Instant},
};

pub struct GeneratedTitle {
    pub text: String,
    pub origin: &'static str,
}

/// Persist only against the captured ownership revision. A concurrent rename
/// wins even when the user chooses the exact same text as the placeholder.
pub fn after_transcription(p: &Paths, id: &str, segments: &[Segment]) -> Result<()> {
    generate_and_apply(p, id, segments, generate)
}

fn generate_and_apply(
    p: &Paths,
    id: &str,
    segments: &[Segment],
    generate: impl FnOnce(&[Segment]) -> Option<GeneratedTitle>,
) -> Result<()> {
    let meeting = p.get(id)?.meeting;
    if meeting.title_origin != "automatic" {
        return Ok(());
    }
    if let Some(title) = generate(segments) {
        p.apply_automatic_title(id, meeting.title_revision, &title.text, title.origin)?;
    }
    Ok(())
}

fn generate(segments: &[Segment]) -> Option<GeneratedTitle> {
    generate_with(segments, on_device_title)
}

fn generate_with(
    segments: &[Segment],
    local_model: impl FnOnce(&str) -> Option<String>,
) -> Option<GeneratedTitle> {
    // Requiring a meaningful extract first also avoids titling silence or a
    // greeting-only recording with an invented topic from the language model.
    let fallback = transcript_excerpt(segments)?;
    if let Some(text) = local_model(&excerpts(segments)) {
        Some(GeneratedTitle {
            text,
            origin: "on_device_model",
        })
    } else {
        Some(GeneratedTitle {
            text: fallback,
            origin: "transcript_excerpt",
        })
    }
}

fn helper_path() -> Option<PathBuf> {
    let executable = std::env::current_exe().ok()?;
    let directory = executable.parent()?;
    // Signed app bundle; sibling location supports a separately built CLI.
    [
        directory.join("../Resources/title-generator"),
        directory.join("title-generator"),
    ]
    .into_iter()
    .find(|path| path.is_file())
}

#[derive(Deserialize)]
struct HelperReply {
    title: Option<String>,
    status: String,
}

fn on_device_title(text: &str) -> Option<String> {
    run_helper(&helper_path()?, text, Duration::from_secs(12))
}

fn run_helper(path: &std::path::Path, text: &str, timeout: Duration) -> Option<String> {
    let mut child = Command::new(path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    // Input is at most 6 KB, below a pipe's capacity: writing cannot deadlock
    // against a helper producing its small single JSON reply.
    let written = child.stdin.take()?.write_all(text.as_bytes());
    if written.is_err() {
        let _ = child.kill();
        let _ = child.wait();
        return None;
    }
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                if !status.success() {
                    return None;
                }
                let output = child.wait_with_output().ok()?;
                if output.stdout.len() > 4096 {
                    return None;
                }
                let response: HelperReply = serde_json::from_slice(&output.stdout).ok()?;
                return (response.status == "generated")
                    .then_some(response.title)
                    .flatten()
                    .and_then(|s| clean_title(&s));
            }
            Ok(None) if start.elapsed() < timeout => {
                std::thread::sleep(Duration::from_millis(25));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
}

fn clean_title(text: &str) -> Option<String> {
    let text = text.trim().trim_matches(['"', '\'', '“', '”']);
    if text.contains(['\n', '\r']) || text.chars().any(char::is_control) {
        return None;
    }
    let words = text.split_whitespace().collect::<Vec<_>>();
    if !(2..=12).contains(&words.len()) || text.chars().count() > 100 {
        return None;
    }
    let title = words.join(" ");
    let lower = title.to_lowercase();
    if lower.starts_with("title:")
        || lower.starts_with("here is")
        || lower.starts_with("i cannot")
        || lower.starts_with("i can't")
        || lower.starts_with("sorry")
    {
        return None;
    }
    Some(title.trim_end_matches(['.', ':', ';']).to_owned())
}

fn excerpts(segments: &[Segment]) -> String {
    let lines = segments
        .iter()
        .filter(|s| useful_text(&s.text))
        .map(|s| s.text.trim())
        .collect::<Vec<_>>();
    if lines.is_empty() {
        return String::new();
    }
    // Spread excerpts across the entire conversation, rather than just its
    // opening greetings. Bound by bytes as well as characters for pipe safety.
    let count = lines.len().min(12);
    let mut text = String::new();
    for i in 0..count {
        let index = if count == 1 {
            0
        } else {
            i * (lines.len() - 1) / (count - 1)
        };
        let mut piece = String::new();
        for c in lines[index].chars() {
            if piece.len() + c.len_utf8() > 450 {
                break;
            }
            piece.push(c);
        }
        text.push_str(&piece);
        text.push('\n');
    }
    text
}

fn useful_text(text: &str) -> bool {
    let text = text.trim();
    !text.is_empty()
        && !text.starts_with('[')
        && !text.starts_with('(')
        && text.chars().any(char::is_alphabetic)
}

fn content_word(word: &str) -> bool {
    word.chars().count() >= 3
        && !STOP_WORDS.contains(&word.to_lowercase().as_str())
        && word.chars().any(char::is_alphabetic)
}

const STOP_WORDS: &[&str] = &[
    "the",
    "and",
    "for",
    "that",
    "this",
    "with",
    "have",
    "has",
    "had",
    "was",
    "were",
    "are",
    "our",
    "you",
    "your",
    "they",
    "their",
    "them",
    "about",
    "from",
    "will",
    "would",
    "could",
    "should",
    "can",
    "not",
    "but",
    "it's",
    "its",
    "let",
    "let's",
    "lets",
    "just",
    "need",
    "want",
    "going",
    "today",
    "we're",
    "we'll",
    "really",
    "okay",
    "yeah",
    "yes",
    "hello",
    "thanks",
    "thank",
    "everyone",
    "joining",
    "welcome",
    "meeting",
    "call",
    "good",
    "morning",
    "afternoon",
    "evening",
    "bye",
    "goodbye",
    "well",
    "then",
    "now",
    "here",
    "there",
    "think",
    "discuss",
    "discussing",
    "talk",
    "talking",
    "look",
    "looking",
    "into",
    "all",
];

/// Pick a meaningful, short passage using content words and repetition. This
/// fallback extracts existing words; it does not claim to summarize with AI.
fn transcript_excerpt(segments: &[Segment]) -> Option<String> {
    let candidates = segments
        .iter()
        .filter(|s| useful_text(&s.text))
        .flat_map(|s| s.text.split(['.', '?', '!', '\n']))
        .map(|s| {
            s.split_whitespace()
                .map(|w| w.trim_matches(|c: char| !c.is_alphanumeric() && c != '\''))
                .filter(|w| !w.is_empty())
                .collect::<Vec<_>>()
        })
        .filter(|words| words.iter().filter(|w| content_word(w)).count() >= 3)
        .collect::<Vec<_>>();
    let mut frequency = HashMap::<String, usize>::new();
    for words in &candidates {
        for word in words.iter().filter(|w| content_word(w)) {
            *frequency.entry(word.to_lowercase()).or_default() += 1;
        }
    }
    let words = candidates
        .iter()
        .enumerate()
        .max_by_key(|(i, words)| {
            let score: usize = words
                .iter()
                .take(16)
                .filter(|w| content_word(w))
                .map(|w| {
                    2 + frequency
                        .get(&w.to_lowercase())
                        .copied()
                        .unwrap_or(0)
                        .min(5)
                })
                .sum();
            (score, std::cmp::Reverse(*i))
        })?
        .1;
    // Trim conversational lead-ins while retaining words actually spoken.
    let start = words.iter().position(|w| content_word(w))?;
    let mut title = String::new();
    for word in words.iter().skip(start).take(9) {
        if title.chars().count() + word.chars().count() + 1 > 96 {
            break;
        }
        if !title.is_empty() {
            title.push(' ');
        }
        title.push_str(word);
    }
    if title.split_whitespace().count() < 3 {
        return None;
    }
    let mut chars = title.chars();
    Some(chars.next()?.to_uppercase().collect::<String>() + chars.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn speech(text: &str) -> Vec<Segment> {
        vec![Segment {
            start_ms: 0,
            end_ms: 1000,
            source: "Meeting audio".into(),
            text: text.into(),
        }]
    }
    #[test]
    fn fallback_skips_greetings_and_extracts_a_real_topic() {
        let mut segments = speech("Hello everyone, thanks for joining. Good morning.");
        segments.extend(speech(
            "Today we're discussing customer onboarding workflow improvements.",
        ));
        assert_eq!(
            transcript_excerpt(&segments).as_deref(),
            Some("Customer onboarding workflow improvements")
        );
        assert!(generate(&speech("[BLANK_AUDIO]")).is_none());
        assert!(generate(&speech("Hello everyone, thanks for joining.")).is_none());
        assert!(generate(&[]).is_none());
    }
    #[test]
    fn generated_titles_are_validated_and_input_is_bounded() {
        assert_eq!(
            clean_title(" \"Launch Planning\" ").as_deref(),
            Some("Launch Planning")
        );
        assert!(clean_title("Here is your title: Launch").is_none());
        assert!(clean_title("Title\nInstructions").is_none());
        assert!(clean_title(&"a ".repeat(30)).is_none());
        let segments = (0..100)
            .map(|_| speech(&"界".repeat(2000)).remove(0))
            .collect::<Vec<_>>();
        assert!(excerpts(&segments).len() < 6000);
    }
    #[test]
    fn title_edit_during_generation_wins_even_when_text_is_unchanged() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let p = Paths::new(Some(temp.path().into()))?;
        for title in ["Customer Planning", "Untitled meeting"] {
            let id = p.create_untitled("Untitled meeting", "complete")?;
            generate_and_apply(
                &p,
                &id,
                &speech("Plan customer onboarding workflow"),
                |_| {
                    p.save_notes_with_title_edit(&id, title, "Retain these notes", true)
                        .unwrap();
                    Some(GeneratedTitle {
                        text: "Generated Title".into(),
                        origin: "on_device_model",
                    })
                },
            )?;
            let saved = p.get(&id)?;
            assert_eq!(saved.meeting.title, title);
            assert_eq!(saved.meeting.title_origin, "user");
            assert_eq!(saved.notes, "Retain these notes");
        }
        Ok(())
    }
    #[test]
    fn explicit_titles_are_protected_and_failure_leaves_placeholder() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let p = Paths::new(Some(temp.path().into()))?;
        let id = p.create("Untitled meeting", "complete")?;
        generate_and_apply(&p, &id, &[], |_| {
            panic!("User title must not invoke generation")
        })?;
        let id = p.create_untitled("Untitled meeting", "complete")?;
        generate_and_apply(&p, &id, &[], |_| None)?;
        assert_eq!(p.get(&id)?.meeting.title, "Untitled meeting");
        assert_eq!(p.get(&id)?.meeting.status, "complete");
        Ok(())
    }
    #[test]
    fn unavailable_or_failing_model_falls_back_to_honest_excerpt() {
        let title = generate_with(&speech("Customer onboarding workflow improvements"), |_| {
            None
        })
        .unwrap();
        assert_eq!(title.text, "Customer onboarding workflow improvements");
        assert_eq!(title.origin, "transcript_excerpt");
    }
    #[test]
    fn helper_failure_and_timeout_are_bounded() -> Result<()> {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir()?;
        let path = temp.path().join("helper");
        for script in [
            "#!/bin/sh\nexit 1\n",
            "#!/bin/sh\necho invalid-json\n",
            "#!/bin/sh\nexec /bin/sleep 2\n",
        ] {
            std::fs::write(&path, script)?;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700))?;
            let started = Instant::now();
            assert!(
                run_helper(&path, "Synthetic meeting text", Duration::from_millis(50)).is_none()
            );
            assert!(started.elapsed() < Duration::from_secs(1));
        }
        std::fs::write(&path, "#!/bin/sh\necho '{\"status\":\"generated\",\"title\":\"Customer Onboarding Review\"}'\n")?;
        assert_eq!(
            run_helper(&path, "Synthetic meeting text", Duration::from_secs(1)).as_deref(),
            Some("Customer Onboarding Review")
        );
        Ok(())
    }
}
