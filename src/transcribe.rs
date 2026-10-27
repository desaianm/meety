use crate::{
    audio::{Resampler, RATE},
    model,
    store::{Paths, Segment},
};
use anyhow::{bail, Context, Result};
use fs2::FileExt;
use moonshine_rs::Transcriber;
use std::{collections::BTreeMap, fs, path::Path};

pub fn run(p: &Paths, id: &str) -> Result<()> {
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(p.root.join("transcription.lock"))?;
    lock.try_lock_exclusive()
        .context("Another transcription is running; retry after it finishes")?;
    let transcript = p.get(id)?;
    if transcript.meeting.status == "recording" {
        bail!("Stop recording before transcribing");
    }
    // Snapshot hints once, so both tracks use the same vocabulary and notes.
    let hints = crate::vocabulary::load(p)?;
    let context = crate::vocabulary::context(&hints, &transcript);
    let id = transcript.meeting.id;
    let choice = model::selected(p);
    if !model::ready(p, choice) {
        bail!("English model missing. Run meetlocal model install");
    }
    p.set_status(&id, "transcribing", None)?;
    let result = (|| -> Result<()> {
        let options = crate::vocabulary::options(&hints, &context);
        let engine = Transcriber::from_files(model::path(p, choice), choice.arch(), Some(&options))
            .context("Load local English model")?;
        let directory = p.meeting_dir(&id);
        let mut segments = Vec::new();
        let mut duration: f64 = 0.0;
        let mut found = false;
        for (file, source) in [
            ("microphone.wav", "You"),
            ("system.wav", "Meeting audio"),
            ("import.wav", "Imported audio"),
        ] {
            let file = directory.join(file);
            if !file.exists() {
                continue;
            }
            found = true;
            let (mut track, seconds) = transcribe_track(&engine, &file, source)?;
            duration = duration.max(seconds);
            segments.append(&mut track);
        }
        if !found {
            bail!("No retained audio found for this meeting");
        }
        segments.sort_by_key(|s| s.start_ms);
        p.save_transcript(&id, &segments, duration)?;
        // Release the speech model before optional on-device title generation.
        drop(engine);
        // Titling must never turn a successfully saved transcript into a failure.
        // Ownership/revision checks inside this operation protect concurrent edits.
        let _ = crate::titles::after_transcription(p, &id, &segments);
        Ok(())
    })();
    if let Err(e) = &result {
        p.set_status(&id, "failed", Some(&format!("{e:#}")))?;
    }
    result
}
fn transcribe_track(
    engine: &Transcriber,
    path: &Path,
    source: &str,
) -> Result<(Vec<Segment>, f64)> {
    let mut reader = hound::WavReader::open(path)?;
    let spec = reader.spec();
    if spec.channels != 1
        || spec.sample_rate != RATE
        || spec.sample_format != hound::SampleFormat::Float
        || spec.bits_per_sample != 32
    {
        bail!("Internal audio must be 16kHz mono float32 WAV");
    }
    let seconds = reader.duration() as f64 / RATE as f64;
    if seconds == 0.0 {
        return Ok((vec![], 0.0));
    }
    let mut stream = engine.create_stream()?;
    let mut lines = BTreeMap::new();
    let mut samples = reader.samples::<f32>();
    loop {
        let chunk = samples
            .by_ref()
            .take(RATE as usize)
            .collect::<std::result::Result<Vec<_>, _>>()?;
        if chunk.is_empty() {
            break;
        }
        stream.add_audio(&chunk, RATE)?;
        for line in stream.poll(false)?.lines {
            lines.insert(line.id, line);
        }
    }
    for line in stream.finalize()?.lines {
        lines.insert(line.id, line);
    }
    let segments = lines
        .into_values()
        .filter(|l| !l.text.trim().is_empty())
        .map(|l| Segment {
            start_ms: (l.start_time as f64 * 1000.0).round() as i64,
            end_ms: ((l.start_time + l.duration) as f64 * 1000.0).round() as i64,
            source: source.into(),
            text: l.text.trim().into(),
        })
        .collect();
    Ok((segments, seconds))
}
pub fn import(p: &Paths, input: &Path, title: &str) -> Result<String> {
    let reader = hound::WavReader::open(input).context("Import accepts PCM WAV files")?;
    let spec = reader.spec();
    if spec.channels == 0
        || spec.channels > 32
        || !(8000..=192000).contains(&spec.sample_rate)
        || !matches!(
            (spec.sample_format, spec.bits_per_sample),
            (hound::SampleFormat::Float, 32) | (hound::SampleFormat::Int, 8 | 16 | 24 | 32)
        )
    {
        bail!("Unsupported WAV format");
    }
    let id = p.create(title, "ready")?;
    let result = (|| -> Result<()> {
        let mut writer = hound::WavWriter::create(
            p.meeting_dir(&id).join("import.wav"),
            hound::WavSpec {
                channels: 1,
                sample_rate: RATE,
                bits_per_sample: 32,
                sample_format: hound::SampleFormat::Float,
            },
        )?;
        let channels = spec.channels as usize;
        let mut r = Resampler::default();
        let samples: Box<dyn Iterator<Item = Result<f32>>> = match spec.sample_format {
            hound::SampleFormat::Float => Box::new(reader.into_samples::<f32>().map(|v| Ok(v?))),
            hound::SampleFormat::Int => Box::new(
                reader
                    .into_samples::<i32>()
                    .map(move |v| Ok(v? as f32 / 2f32.powi(spec.bits_per_sample as i32 - 1))),
            ),
        };
        let mut samples = samples;
        loop {
            let interleaved = samples
                .by_ref()
                .take(48000 * channels)
                .collect::<Result<Vec<_>>>()?;
            if interleaved.is_empty() {
                break;
            }
            if interleaved.len() % channels != 0 {
                bail!("Truncated WAV frame");
            }
            let mono: Vec<f32> = interleaved
                .chunks_exact(channels)
                .map(|c| c.iter().sum::<f32>() / channels as f32)
                .collect();
            for v in r.process(&mono, spec.sample_rate as f64) {
                writer.write_sample(v)?;
            }
        }
        writer.finalize()?;
        run(p, &id)
    })();
    if let Err(e) = result {
        p.set_status(&id, "failed", Some(&format!("{e:#}")))?;
        bail!("Import {id} retained for retry: {e:#}");
    }
    Ok(id)
}
