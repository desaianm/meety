use cpal::{
    traits::{DeviceTrait, HostTrait, StreamTrait},
    SizedSample,
};
use dasp::sample::ToSample;
use futures_channel::mpsc;
use futures_util::{Stream, StreamExt};
use std::pin::Pin;

use crate::AsyncSource;

pub struct MicInput {
    #[allow(dead_code)]
    host: cpal::Host,
    device: cpal::Device,
    config: cpal::SupportedStreamConfig,
}

impl MicInput {
    pub fn device_name(&self) -> String {
        self.device
            .name()
            .unwrap_or("Unknown Microphone".to_string())
    }

    pub fn list_devices() -> Vec<String> {
        cpal::default_host()
            .input_devices()
            .unwrap()
            .map(|d| d.name().unwrap_or("Unknown Microphone".to_string()))
            .collect()
    }

    pub fn new(device_name: Option<String>) -> Result<Self, crate::Error> {
        let host = cpal::default_host();

        let device = match device_name {
            // Probing supported formats on every virtual/Bluetooth device can
            // block in HAL before the actual default microphone is opened.
            None => host
                .default_input_device()
                .ok_or(crate::Error::NoInputDevice)?,
            Some(name) => host
                .devices()
                .ok()
                .and_then(|mut devices| devices.find(|d| d.name().unwrap_or_default() == name))
                .or_else(|| host.default_input_device())
                .ok_or(crate::Error::NoInputDevice)?,
        };

        let config = device.default_input_config()?;
        tracing::info!(sample_rate = ?config.sample_rate());

        Ok(Self {
            host,
            device,
            config,
        })
    }
}

impl MicInput {
    pub fn stream(&self) -> anyhow::Result<MicStream> {
        let (tx, rx) = mpsc::channel::<Vec<f32>>(64);

        let errors = std::sync::Arc::new(std::sync::Mutex::new(None));
        let callback_errors = errors.clone();
        let (ready, wait) = std::sync::mpsc::channel();
        let config = self.config.clone();
        let device = self.device.clone();
        let (drop_tx, drop_rx) = std::sync::mpsc::channel();

        std::thread::spawn(move || {
            fn build_stream<S: ToSample<f32> + SizedSample>(
                device: &cpal::Device,
                config: &cpal::SupportedStreamConfig,
                mut tx: mpsc::Sender<Vec<f32>>,
                errors: std::sync::Arc<std::sync::Mutex<Option<String>>>,
            ) -> Result<cpal::Stream, cpal::BuildStreamError> {
                let channels = config.channels() as usize;
                let stream_errors = errors.clone();
                device.build_input_stream::<S, _, _>(
                    &config.config(),
                    move |data: &[S], _input_callback_info: &_| {
                        if tx
                            .try_send(
                                data.iter()
                                    .step_by(channels)
                                    .map(|&x| x.to_sample())
                                    .collect(),
                            )
                            .is_err()
                        {
                            *errors.lock().unwrap() = Some(
                                "Microphone input queue overflowed; recording is incomplete".into(),
                            );
                        }
                    },
                    move |err| {
                        *stream_errors.lock().unwrap() =
                            Some(format!("Microphone capture failed: {err}"));
                    },
                    None,
                )
            }

            let start_stream = || {
                let stream = match config.sample_format() {
                    cpal::SampleFormat::I8 => {
                        build_stream::<i8>(&device, &config, tx, callback_errors.clone())
                    }
                    cpal::SampleFormat::I16 => {
                        build_stream::<i16>(&device, &config, tx, callback_errors.clone())
                    }
                    cpal::SampleFormat::I32 => {
                        build_stream::<i32>(&device, &config, tx, callback_errors.clone())
                    }
                    cpal::SampleFormat::F32 => {
                        build_stream::<f32>(&device, &config, tx, callback_errors.clone())
                    }
                    sample_format => {
                        tracing::error!(sample_format = ?sample_format, "unsupported");
                        return None;
                    }
                };

                let stream = match stream {
                    Ok(stream) => stream,
                    Err(err) => {
                        tracing::error!("Error starting stream: {}", err);
                        return None;
                    }
                };

                if let Err(err) = stream.play() {
                    *callback_errors.lock().unwrap() =
                        Some(format!("Microphone startup failed: {err}"));
                    return None;
                }

                Some(stream)
            };

            let stream = match start_stream() {
                Some(stream) => stream,
                None => {
                    let _ = ready.send(Err(
                        "Microphone startup failed. Allow Microphone in macOS Privacy & Security",
                    ));
                    return;
                }
            };

            let _ = ready.send(Ok(()));
            // Wait for the stream to be dropped
            let _ = drop_rx.recv();

            // Then drop the stream
            drop(stream);
        });

        let receiver = rx.map(futures_util::stream::iter).flatten();
        wait.recv()
            .map_err(|_| anyhow::anyhow!("Microphone thread stopped"))?
            .map_err(|e| anyhow::anyhow!(e))?;
        Ok(MicStream {
            drop_tx,
            config: self.config.clone(),
            receiver: Box::pin(receiver),
            errors,
        })
    }
}

pub struct MicStream {
    drop_tx: std::sync::mpsc::Sender<()>,
    config: cpal::SupportedStreamConfig,
    receiver: Pin<Box<dyn Stream<Item = f32> + Send + Sync>>,
    errors: std::sync::Arc<std::sync::Mutex<Option<String>>>,
}

impl Drop for MicStream {
    fn drop(&mut self) {
        let _ = self.drop_tx.send(());
    }
}

impl Stream for MicStream {
    type Item = f32;

    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        match self.receiver.as_mut().poll_next_unpin(cx) {
            std::task::Poll::Ready(Some(data_chunk)) => std::task::Poll::Ready(Some(data_chunk)),
            std::task::Poll::Ready(None) => std::task::Poll::Ready(None),
            std::task::Poll::Pending => std::task::Poll::Pending,
        }
    }
}

impl AsyncSource for MicStream {
    fn as_stream(&mut self) -> impl Stream<Item = f32> + '_ {
        self
    }

    fn sample_rate(&self) -> u32 {
        self.config.sample_rate().0
    }
}

impl MicStream {
    pub fn errors(&self) -> std::sync::Arc<std::sync::Mutex<Option<String>>> {
        self.errors.clone()
    }
}
