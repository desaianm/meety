use crate::store::Segment;
use anyhow::{bail, Context, Result};
use std::{
    io::Write,
    process::{Command, Stdio},
};

/// Plain text only: personal notes and meeting metadata never reach the clipboard.
pub fn format_transcript(segments: &[Segment]) -> Result<String> {
    let mut lines = Vec::new();
    for segment in segments {
        let text = segment.text.trim();
        if text.is_empty() {
            continue;
        }
        let seconds = segment.start_ms.max(0) / 1000;
        lines.push(format!(
            "[{:02}:{:02}:{:02}] {}\n{}",
            seconds / 3600,
            (seconds / 60) % 60,
            seconds % 60,
            segment.source,
            text
        ));
    }
    if lines.is_empty() {
        bail!("No transcript to copy yet");
    }
    Ok(lines.join("\n\n"))
}

pub fn copy_transcript(segments: &[Segment]) -> Result<()> {
    let text = format_transcript(segments)?;
    // Pass meeting text through stdin, never a shell command or command arguments.
    write_to_command(
        Command::new("/usr/bin/pbcopy").env("LC_ALL", "en_US.UTF-8"),
        &text,
    )
}

fn write_to_command(command: &mut Command, text: &str) -> Result<()> {
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .context("Could not open the macOS clipboard")?;
    let written = child
        .stdin
        .take()
        .context("Clipboard input is unavailable")?
        .write_all(text.as_bytes());
    let status = child
        .wait()
        .context("Could not finish copying transcript")?;
    written.context("Could not write transcript to the clipboard")?;
    if !status.success() {
        bail!("macOS could not copy the transcript. Try again.");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn segment(start_ms: i64, source: &str, text: &str) -> Segment {
        Segment {
            start_ms,
            end_ms: start_ms + 1000,
            source: source.into(),
            text: text.into(),
        }
    }

    #[test]
    fn full_transcript_keeps_timestamps_speakers_unicode_and_literal_text() -> Result<()> {
        let text = format_transcript(&[
            segment(-5, "You", "  Hello café.  "),
            segment(
                3_661_999,
                "Meeting audio",
                "Keep $(example) and `literal` text.\nNext line.",
            ),
        ])?;
        assert_eq!(text, "[00:00:00] You\nHello café.\n\n[01:01:01] Meeting audio\nKeep $(example) and `literal` text.\nNext line.");
        Ok(())
    }

    #[test]
    fn empty_transcript_does_not_replace_clipboard() {
        assert!(format_transcript(&[]).is_err());
        assert!(format_transcript(&[segment(0, "You", " \n ")]).is_err());
    }

    #[test]
    fn native_copy_reports_unavailable_command_without_touching_clipboard() {
        assert!(write_to_command(
            &mut Command::new("/missing-meety-clipboard-command"),
            "fixture"
        )
        .is_err());
    }
}
