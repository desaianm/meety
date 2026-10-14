mod mic;
mod speaker;
pub use cpal;
pub use mic::*;
pub use speaker::*;

/// Read-only permission state; never opens an input device or requests access.
pub fn microphone_permission() -> &'static str {
    use cidre::av::{self, capture::AuthorizationStatus};
    match av::CaptureDevice::authorization_status_for_media_type(av::MediaType::audio()) {
        Ok(AuthorizationStatus::Authorized) => "authorized",
        Ok(AuthorizationStatus::NotDetermined) => "not_determined",
        Ok(AuthorizationStatus::Denied) => "denied",
        Ok(AuthorizationStatus::Restricted) => "restricted",
        Err(_) => "unavailable",
    }
}

pub async fn request_microphone_permission() -> anyhow::Result<()> {
    use cidre::av;
    if microphone_permission() == "authorized" {
        return Ok(());
    }
    let allowed = av::CaptureDevice::request_access_for_media_type(av::MediaType::audio())
        .await
        .map_err(|e| anyhow::anyhow!("Microphone permission request failed: {e:?}"))?;
    if !allowed {
        anyhow::bail!("Allow Meetlocal under System Settings → Privacy & Security → Microphone");
    }
    Ok(())
}

// Keep the original API's small trait without pulling Hyprnote's LLM stack.
pub trait AsyncSource {
    fn as_stream(&mut self) -> impl futures_util::Stream<Item = f32> + '_;
    fn sample_rate(&self) -> u32;
}
#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error("No microphone found")]
    NoInputDevice,
    #[error("Microphone configuration failed: {0}")]
    Configuration(#[from] cpal::DefaultStreamConfigError),
}
