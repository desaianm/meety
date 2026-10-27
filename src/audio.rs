use anyhow::{bail, Context, Result};
use futures_util::StreamExt;
use hypr_capture::{AsyncSource, MicInput, SpeakerInput};
use std::{
    fs::File,
    io::BufWriter,
    path::Path,
    sync::{Arc, Mutex},
    time::Instant,
};

pub const RATE: u32 = 16000;
type Writer = hound::WavWriter<BufWriter<File>>;

/// Packet-independent weighted averaging for fractional input sample rates.
#[derive(Default)]
pub struct Resampler {
    phase: f64,
    sum: f64,
}
impl Resampler {
    pub fn process(&mut self, samples: &[f32], rate: f64) -> Vec<f32> {
        let ratio = rate / RATE as f64;
        let mut out = Vec::with_capacity((samples.len() as f64 / ratio).ceil() as usize);
        for &value in samples {
            let mut left = 1.0f64;
            while left > 1e-9 {
                let take = left.min(ratio - self.phase);
                self.sum += value as f64 * take;
                self.phase += take;
                left -= take;
                if self.phase >= ratio - 1e-9 {
                    out.push((self.sum / ratio) as f32);
                    self.sum = 0.0;
                    self.phase = 0.0;
                }
            }
        }
        out
    }
}
struct Track {
    writer: Option<Writer>,
    resampler: Resampler,
    written: u64,
    packets: u64,
    rate: Option<f64>,
    last_flush: Instant,
}
impl Track {
    fn create(path: &Path) -> Result<Self> {
        let writer = hound::WavWriter::create(
            path,
            hound::WavSpec {
                channels: 1,
                sample_rate: RATE,
                bits_per_sample: 32,
                sample_format: hound::SampleFormat::Float,
            },
        )?;
        Ok(Self {
            writer: Some(writer),
            resampler: Resampler::default(),
            written: 0,
            packets: 0,
            rate: None,
            last_flush: Instant::now(),
        })
    }
    fn append(&mut self, samples: &[f32], rate: f64, position: f64) -> Result<()> {
        if !rate.is_finite() || !(8000.0..=192000.0).contains(&rate) {
            bail!("Unsupported capture sample rate: {rate}");
        }
        if self.rate.is_some_and(|r| (r - rate).abs() > 1.0) {
            self.resampler = Resampler::default();
        }
        self.rate = Some(rate);
        let writer = self.writer.as_mut().context("Audio track already closed")?;
        let target = (position.max(0.0) * RATE as f64) as u64;
        // Align both sources to the host clock; ignore jitter below 5ms.
        if target > self.written + 80 {
            let gap = target - self.written;
            if gap > RATE as u64 * 60 * 60 * 8 {
                bail!("Capture timestamp outside eight-hour limit");
            }
            for _ in 0..gap {
                writer.write_sample(0.0f32)?;
            }
            self.written = target;
        }
        for v in self.resampler.process(samples, rate) {
            writer.write_sample(if v.is_finite() {
                v.clamp(-1.0, 1.0)
            } else {
                0.0
            })?;
            self.written += 1;
        }
        self.packets += 1;
        if self.last_flush.elapsed().as_secs() >= 1 {
            writer.flush()?;
            self.last_flush = Instant::now();
        }
        Ok(())
    }
    fn finish(&mut self) -> Result<()> {
        if let Some(w) = self.writer.take() {
            w.finalize()?;
        }
        Ok(())
    }
}
struct Tracks {
    microphone: Track,
    system: Track,
    error: Option<String>,
}
pub struct Capture {
    stop: tokio::sync::watch::Sender<bool>,
    thread: Option<std::thread::JoinHandle<Result<()>>>,
    tracks: Arc<Mutex<Tracks>>,
    pub started: Instant,
}
impl Capture {
    pub fn start(directory: &Path) -> Result<Self> {
        let tracks = Arc::new(Mutex::new(Tracks {
            microphone: Track::create(&directory.join("microphone.wav"))?,
            system: Track::create(&directory.join("system.wav"))?,
            error: None,
        }));
        let (stop, mut stopped) = tokio::sync::watch::channel(false);
        let (ready, wait) = std::sync::mpsc::channel();
        let shared = tracks.clone();
        let thread = std::thread::spawn(move || {
            let result = (|| -> Result<()> {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()?;
                runtime.block_on(async {
                    tokio::select! {
                        result=hypr_capture::request_microphone_permission()=>result?,
                        _=stopped.changed()=>bail!("Recording startup cancelled"),
                    }
                    if *stopped.borrow(){bail!("Recording startup cancelled");}
                    // Finish permission prompts before either source produces
                    // samples, avoiding queue overflow during first-use setup.
                    let speaker=SpeakerInput::new().context("Allow Meetlocal to capture system audio in macOS Privacy & Security")?;
                    if *stopped.borrow(){bail!("Recording startup cancelled");}
                    let mic=MicInput::new(None)?;
                    if *stopped.borrow(){bail!("Recording startup cancelled");}
                    let system=speaker.stream()?;
                    system.check_format()?;
                    if *stopped.borrow(){bail!("Recording startup cancelled");}
                    // Aggregate-device startup can wait on a first-use system
                    // audio prompt. Start the microphone only after that wait,
                    // so its bounded input queue cannot fill during setup.
                    let microphone=mic.stream()?;
                    let mic_rate=microphone.sample_rate();
                    let mic_errors=microphone.errors();
                    let mut microphone=microphone.ready_chunks(480);
                    let mut system=system.ready_chunks(480);
                    let started=Instant::now();
                    let mut format_check=tokio::time::interval(std::time::Duration::from_millis(500));
                    let _=ready.send(Ok(started));
                    loop {
                        tokio::select! {
                            biased;
                            _=stopped.changed()=>break,
                            _=format_check.tick()=>system.get_ref().check_format()?,
                            data=microphone.next()=>{
                                let samples=data.context("Microphone stream ended")?;
                                let mut t=shared.lock().unwrap();
                                let position=if t.microphone.packets==0 { (started.elapsed().as_secs_f64()-samples.len() as f64/mic_rate as f64).max(0.0) }else{t.microphone.written as f64/RATE as f64};
                                t.microphone.append(&samples,mic_rate as f64,position).context("Microphone capture failed")?;
                                if let Some(error)=mic_errors.lock().unwrap().clone(){bail!("{error}");}
                            },
                            data=system.next()=>{
                                let samples=data.context("System audio stream ended; audio may have overflowed")?;
                                let rate=system.get_ref().sample_rate();
                                let mut t=shared.lock().unwrap();
                                let position=if t.system.packets==0 { (started.elapsed().as_secs_f64()-samples.len() as f64/rate).max(0.0) }else{t.system.written as f64/RATE as f64};
                                t.system.append(&samples,rate,position).context("System audio capture failed")?;
                            },
                        }
                        if started.elapsed().as_secs()>=8*3600 {bail!("Eight-hour recording limit reached. Audio retained.");}
                    }
                    Ok(())
                })
            })();
            if let Err(e) = &result {
                let message = format!("{e:#}");
                let _ = ready.send(Err(message.clone()));
                shared.lock().unwrap().error = Some(message);
            }
            let mut t = shared.lock().unwrap();
            let mic = t.microphone.finish();
            let system = t.system.finish();
            result?;
            mic?;
            system?;
            Ok(())
        });
        let startup = match wait.recv_timeout(std::time::Duration::from_secs(45)) {
            Ok(result) => result,
            Err(error) => {
                let _ = stop.send(true);
                bail!("Audio startup did not finish: {error}. Check permissions and your default microphone, then retry.");
            }
        };
        match startup {
            Ok(started) => Ok(Self {
                stop,
                thread: Some(thread),
                tracks,
                started,
            }),
            Err(error) => {
                let _ = thread.join();
                bail!("{error}");
            }
        }
    }
    pub fn metrics(&self) -> serde_json::Value {
        let t = self.tracks.lock().unwrap();
        serde_json::json!({"microphone_packets":t.microphone.packets,"system_packets":t.system.packets,"elapsed_seconds":self.started.elapsed().as_secs(),"error":t.error})
    }
    pub fn error(&self) -> Option<String> {
        self.tracks.lock().ok().and_then(|t| t.error.clone())
    }
    pub fn stop(mut self) -> Result<()> {
        let _ = self.stop.send(true);
        if let Some(thread) = self.thread.take() {
            thread
                .join()
                .map_err(|_| anyhow::anyhow!("Capture thread failed"))??;
        }
        let t = self.tracks.lock().unwrap();
        if t.microphone.packets == 0 {
            bail!("No microphone audio received. Grant Microphone access in macOS Privacy & Security. Audio retained.");
        }
        Ok(())
    }
}
impl Drop for Capture {
    fn drop(&mut self) {
        let _ = self.stop.send(true);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fractional_resampling_is_packet_independent() {
        let input = vec![0.25f32; 44100];
        let whole = Resampler::default().process(&input, 44100.0);
        let mut r = Resampler::default();
        let mut parts = Vec::new();
        for chunk in input.chunks(137) {
            parts.extend(r.process(chunk, 44100.0));
        }
        assert_eq!(whole.len(), 16000);
        assert_eq!(whole, parts);
        assert!(whole.iter().all(|v| (*v - 0.25).abs() < 1e-6));
    }
    #[test]
    fn source_silence_preserves_timeline() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let file = temp.path().join("track.wav");
        let mut track = Track::create(&file)?;
        track.append(&vec![0.5; 480], 48000.0, 1.0)?;
        track.finish()?;
        let r = hound::WavReader::open(file)?;
        assert_eq!(r.duration(), 16160);
        Ok(())
    }
}
