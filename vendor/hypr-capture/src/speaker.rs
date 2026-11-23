use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Poll, Waker};

use anyhow::{Context, Result};
use futures_util::Stream;

use ringbuf::{
    traits::{Consumer, Producer, Split},
    HeapCons, HeapProd, HeapRb,
};

use ca::aggregate_device_keys as agg_keys;
use cidre::{arc, av, cat, cf, core_audio as ca, ns, os};

pub struct SpeakerInput {
    tap: ca::TapGuard,
    agg_desc: arc::Retained<cf::DictionaryOf<cf::String, cf::Type>>,
}

struct WakerState {
    waker: Option<Waker>,
    has_data: bool,
}

pub struct SpeakerStream {
    consumer: HeapCons<f32>,
    _device: ca::hardware::StartedDevice<ca::AggregateDevice>,
    _ctx: Box<Ctx>,
    _tap: ca::TapGuard,
    waker_state: Arc<Mutex<WakerState>>,
    sample_rate: f64,
    expected_format: cat::AudioStreamBasicDesc,
    input_stream: ca::Stream,
}

impl SpeakerStream {
    pub fn sample_rate(&self) -> f64 {
        self.sample_rate
    }
    /// Check outside the realtime callback. Only a change to the actual input
    /// buffer format ends this epoch; queued samples keep their original rate.
    pub fn check_format(&self) -> Result<()> {
        let actual = self
            .input_stream
            .virtual_format()
            .context("System audio input format is unavailable")?;
        check_stream_format(&self.expected_format, &actual)
    }
}

struct Ctx {
    format: arc::R<av::AudioFormat>,
    producer: HeapProd<f32>,
    waker_state: Arc<Mutex<WakerState>>,
    consecutive_drops: Arc<AtomicU32>,
    should_terminate: Arc<AtomicBool>,
}

impl SpeakerInput {
    pub fn new() -> Result<Self> {
        let tap_desc = ca::TapDesc::with_mono_global_tap_excluding_processes(&ns::Array::new());
        let tap = tap_desc.create_process_tap()?;

        let tap_uid = tap
            .uid()
            .context("System audio tap identifier is unavailable")?;
        let sub_tap = cf::DictionaryOf::with_keys_values(
            &[
                ca::hardware::sub_tap_keys::uid(),
                ca::hardware::sub_tap_keys::drift_compensation(),
            ],
            &[
                tap_uid.as_type_ref(),
                cf::Boolean::value_true().as_type_ref(),
            ],
        );
        // A capture-only aggregate avoids borrowing a physical output device's
        // clock and input channels. Only our process tap belongs in this device.
        let agg_desc = cf::DictionaryOf::with_keys_values(
            &[
                agg_keys::is_private(),
                agg_keys::tap_auto_start(),
                agg_keys::name(),
                agg_keys::uid(),
                agg_keys::tap_list(),
            ],
            &[
                cf::Boolean::value_true().as_type_ref(),
                // Do not make AudioDeviceStart wait for another app's first
                // sound; microphone recording must also start in a quiet room.
                cf::Boolean::value_false(),
                cf::str!(c"meety-audio-tap"),
                &cf::Uuid::new().to_cf_string(),
                &cf::ArrayOf::from_slice(&[sub_tap.as_ref()]),
            ],
        );

        Ok(Self { tap, agg_desc })
    }

    fn start_device(
        &self,
        agg_device: ca::AggregateDevice,
        ctx: &mut Box<Ctx>,
    ) -> Result<ca::hardware::StartedDevice<ca::AggregateDevice>> {
        extern "C" fn proc(
            _device: ca::Device,
            _now: &cat::AudioTimeStamp,
            input_data: &cat::AudioBufList<1>,
            _input_time: &cat::AudioTimeStamp,
            _output_data: &mut cat::AudioBufList<1>,
            _output_time: &cat::AudioTimeStamp,
            ctx: Option<&mut Ctx>,
        ) -> os::Status {
            let ctx = ctx.unwrap();

            // Buffers are decoded using ctx.format, obtained from the aggregate
            // input stream's virtual ASBD. A physical device's nominal clock is not their
            // format: it can report zero during device transitions. Never use
            // that live property to relabel already-queued samples.
            if let Some(view) =
                av::AudioPcmBuf::with_buf_list_no_copy(&ctx.format, input_data, None)
            {
                if let Some(data) = view.data_f32_at(0) {
                    process_audio_data(ctx, data);
                }
            } else if ctx.format.common_format() == av::audio::CommonFormat::PcmF32 {
                let first_buffer = &input_data.buffers[0];
                let byte_count = first_buffer.data_bytes_size as usize;
                let float_count = byte_count / std::mem::size_of::<f32>();

                if float_count > 0 && !first_buffer.data.is_null() {
                    let data = unsafe {
                        std::slice::from_raw_parts(first_buffer.data as *const f32, float_count)
                    };
                    process_audio_data(ctx, data);
                }
            }

            os::Status::NO_ERR
        }

        let proc_id = agg_device.create_io_proc_id(proc, Some(ctx))?;
        let started_device = ca::device_start(agg_device, Some(proc_id))?;

        Ok(started_device)
    }

    fn prepare_stream(
        &self,
    ) -> Result<(ca::AggregateDevice, ca::Stream, cat::AudioStreamBasicDesc)> {
        let tap_format = self.tap.asbd()?;
        let mut aggregate = ca::AggregateDevice::with_desc(&self.agg_desc)?;
        // This changes only our private, tap-only virtual device, never a user's
        // speaker/microphone configuration. Core Audio converts into this rate.
        aggregate.set_nominal_sample_rate(tap_sample_rate(tap_format.sample_rate)?)?;
        // HAL setters can complete asynchronously. Read the input virtual
        // format until the requested private rate is observed twice, rather
        // than taking an early baseline and mistaking settlement for a switch.
        let (input_stream, asbd) = wait_for_input_format(
            tap_format.sample_rate,
            std::time::Duration::from_secs(2),
            || {
                let mut inputs = Vec::new();
                for stream in aggregate.as_ref().streams()? {
                    if stream.direction()? == 1 {
                        inputs.push(stream);
                    }
                }
                anyhow::ensure!(
                    inputs.len() == 1,
                    "System audio aggregate must have exactly one input stream"
                );
                let stream = inputs.pop().unwrap();
                let format = stream.virtual_format()?;
                Ok((stream, format))
            },
        )?;
        // IOProc buffers use this VIRTUAL format, not a physical device clock.
        let format =
            av::AudioFormat::with_asbd(&asbd).context("Unsupported system audio input format")?;
        anyhow::ensure!(
            format.common_format() == av::audio::CommonFormat::PcmF32
                && asbd.channels_per_frame == 1,
            "System audio capture requires a mono float32 input stream"
        );

        tap_sample_rate(asbd.sample_rate)?;
        Ok((aggregate, input_stream, asbd))
    }

    pub fn stream(self) -> Result<SpeakerStream> {
        let (aggregate, input_stream, asbd) = self.prepare_stream()?;
        let format =
            av::AudioFormat::with_asbd(&asbd).context("Unsupported system audio input format")?;

        let buffer_size = 1024 * 128;
        let rb = HeapRb::<f32>::new(buffer_size);
        let (producer, consumer) = rb.split();

        let waker_state = Arc::new(Mutex::new(WakerState {
            waker: None,
            has_data: false,
        }));

        let sample_rate = tap_sample_rate(asbd.sample_rate)?;

        let mut ctx = Box::new(Ctx {
            format,
            producer,
            waker_state: waker_state.clone(),
            consecutive_drops: Arc::new(AtomicU32::new(0)),
            should_terminate: Arc::new(AtomicBool::new(false)),
        });

        let device = self.start_device(aggregate, &mut ctx)?;

        Ok(SpeakerStream {
            consumer,
            _device: device,
            _ctx: ctx,
            _tap: self.tap,
            waker_state,
            sample_rate,
            expected_format: asbd,
            input_stream,
        })
    }
}

fn process_audio_data(ctx: &mut Ctx, data: &[f32]) {
    let buffer_size = data.len();
    let pushed = ctx.producer.push_slice(data);

    if pushed < buffer_size {
        let consecutive = ctx.consecutive_drops.fetch_add(1, Ordering::AcqRel) + 1;

        let _ = consecutive;
        ctx.should_terminate.store(true, Ordering::Release);
    } else {
        ctx.consecutive_drops.store(0, Ordering::Release);
    }

    if pushed > 0 || ctx.should_terminate.load(Ordering::Acquire) {
        let should_wake = {
            let mut waker_state = ctx.waker_state.lock().unwrap();
            if !waker_state.has_data {
                waker_state.has_data = true;
                waker_state.waker.take()
            } else {
                None
            }
        };

        if let Some(waker) = should_wake {
            waker.wake();
        }
    }
}

impl Stream for SpeakerStream {
    type Item = f32;

    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<Option<Self::Item>> {
        if let Some(sample) = self.consumer.try_pop() {
            return Poll::Ready(Some(sample));
        }

        if self._ctx.should_terminate.load(Ordering::Acquire) {
            tracing::warn!("should_terminate");
            return match self.consumer.try_pop() {
                Some(sample) => Poll::Ready(Some(sample)),
                None => Poll::Ready(None),
            };
        }

        {
            let mut state = self.waker_state.lock().unwrap();
            state.has_data = false;
            state.waker = Some(cx.waker().clone());
        }

        // A callback may have arrived between the first pop and waker registration.
        if let Some(sample) = self.consumer.try_pop() {
            return Poll::Ready(Some(sample));
        }
        if self._ctx.should_terminate.load(Ordering::Acquire) {
            return Poll::Ready(None);
        }
        Poll::Pending
    }
}

impl Drop for SpeakerStream {
    fn drop(&mut self) {
        self._ctx.should_terminate.store(true, Ordering::Release);
    }
}

// The rate belongs to the aggregate input stream's virtual format, not to
// output-device metadata. Keep it immutable for this epoch, including fractions.
fn tap_sample_rate(format_rate: f64) -> Result<f64> {
    anyhow::ensure!(
        format_rate.is_finite() && (8_000.0..=192_000.0).contains(&format_rate),
        "Unsupported system audio tap sample rate: {format_rate}"
    );
    Ok(format_rate)
}

fn wait_for_input_format<T>(
    requested: f64,
    timeout: std::time::Duration,
    mut read: impl FnMut() -> Result<(T, cat::AudioStreamBasicDesc)>,
) -> Result<(T, cat::AudioStreamBasicDesc)> {
    let deadline = std::time::Instant::now() + timeout;
    let mut previous = None;
    loop {
        match read() {
            Ok((stream, format))
                if format.sample_rate.is_finite()
                    && (format.sample_rate - requested).abs() <= 1.0 =>
            {
                if previous
                    .as_ref()
                    .is_some_and(|last| check_stream_format(last, &format).is_ok())
                {
                    return Ok((stream, format));
                }
                previous = Some(format);
            }
            _ => previous = None,
        }
        anyhow::ensure!(std::time::Instant::now() < deadline,
            "System audio input format did not settle before capture. Check your audio device and try again.");
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

fn check_stream_format(
    expected: &cat::AudioStreamBasicDesc,
    actual: &cat::AudioStreamBasicDesc,
) -> Result<()> {
    // Reserved bytes are not audio format. Ignore them when comparing ASBDs.
    let mut expected = *expected;
    let mut actual = *actual;
    expected.reserved = 0;
    actual.reserved = 0;
    anyhow::ensure!(expected == actual,
        "System audio input format changed. Recording stopped to preserve audio timing; start a new recording for the new device.");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    #[ignore = "Native metadata only: creates a private tap/aggregate but never registers an IOProc or starts audio"]
    fn native_tap_format_metadata_probe() -> Result<()> {
        let output = ca::System::default_output_device()?;
        let before = output.nominal_sample_rate()?;
        let source = SpeakerInput::new()?;
        let (aggregate, stream, asbd) = source.prepare_stream()?;
        assert!(check_stream_format(&asbd, &stream.virtual_format()?).is_ok());
        assert!((aggregate.as_ref().nominal_sample_rate()? - asbd.sample_rate).abs() <= 1.0);
        assert_eq!(
            output.nominal_sample_rate()?,
            before,
            "Private aggregate configuration changed a physical device"
        );
        println!("Metadata-only probe passed: private input {} Hz, {} channel; physical output clock {} Hz unchanged. No IOProc registered, audio device never started.", asbd.sample_rate, asbd.channels_per_frame, before);
        Ok(())
    }

    #[test]
    fn input_format_waits_for_delayed_settlement_and_times_out() -> Result<()> {
        let rates = [44_100.0, 0.0, 48_000.0, 48_000.0];
        let mut reads = 0;
        let (_, format) =
            wait_for_input_format(48_000.0, std::time::Duration::from_secs(1), || {
                let format = cat::AudioStreamBasicDesc {
                    sample_rate: rates[reads],
                    ..Default::default()
                };
                reads += 1;
                Ok(((), format))
            })?;
        assert_eq!(reads, 4);
        assert_eq!(format.sample_rate, 48_000.0);
        let result = wait_for_input_format(48_000.0, std::time::Duration::ZERO, || {
            Ok((
                (),
                cat::AudioStreamBasicDesc {
                    sample_rate: 44_100.0,
                    ..Default::default()
                },
            ))
        });
        assert!(result.unwrap_err().to_string().contains("did not settle"));
        Ok(())
    }

    #[test]
    fn unchanged_stream_does_not_fail_for_different_device_clock() {
        let actual = cat::AudioStreamBasicDesc {
            sample_rate: 48_000.0,
            ..Default::default()
        };
        let unrelated_device_clock = 44_100.0;
        assert_ne!(actual.sample_rate, unrelated_device_clock);
        check_stream_format(&actual, &actual)
            .expect("An unchanged buffer format must not be rejected because a separate device clock differs");
    }
    #[test]
    fn tap_buffers_keep_their_format_when_device_nominal_rate_disappears() {
        // Nominal-device metadata no longer enters this API or the callback.
        // Both the format decoder and queue consumer share the original ASBD.
        assert_eq!(tap_sample_rate(48_000.0).unwrap(), 48_000.0);
        assert_eq!(tap_sample_rate(44_100.5).unwrap(), 44_100.5);
        for invalid in [0.0, f64::NAN, f64::INFINITY, 7_999.0, 192_001.0] {
            assert!(tap_sample_rate(invalid).is_err());
        }
        let expected = cat::AudioStreamBasicDesc {
            sample_rate: 48_000.0,
            ..Default::default()
        };
        assert!(check_stream_format(&expected, &expected).is_ok());
        let padding_only = cat::AudioStreamBasicDesc {
            reserved: 123,
            ..expected
        };
        assert!(check_stream_format(&expected, &padding_only).is_ok());
        let changed = cat::AudioStreamBasicDesc {
            sample_rate: 24_000.0,
            ..expected
        };
        assert!(check_stream_format(&expected, &changed).is_err());
    }
}
