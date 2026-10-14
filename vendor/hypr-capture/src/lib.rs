mod mic;
mod speaker;
pub use cpal;
pub use mic::*;
pub use speaker::*;

/// Inspect Core Audio's per-process input activity without opening a device,
/// capturing samples, or asking for microphone / system-audio permission.
/// Process identities are returned only to distinguish this app from other users
/// of the microphone; callers must not persist or log this metadata.
pub fn other_microphone_users() -> anyhow::Result<Vec<u32>> {
    use cidre::core_audio::Process;
    let own_pid = std::process::id();
    let processes =
        Process::list().map_err(|_| anyhow::anyhow!("Microphone activity is unavailable"))?;
    let mut users = Vec::new();
    let mut readable = 0;
    let mut other_objects = 0;
    for process in &processes {
        // Audio process objects can disappear between enumeration and a read.
        let Ok(pid) = process.pid() else {
            other_objects += 1;
            continue;
        };
        if pid as u32 == own_pid {
            continue;
        }
        other_objects += 1;
        if pid <= 0 {
            continue;
        }
        if let Ok(active) = process.is_running_input() {
            readable += 1;
            if active {
                users.push(pid as u32);
            }
        }
    }
    if other_objects > 0 && readable == 0 {
        anyhow::bail!("Microphone activity is unavailable");
    }
    users.sort_unstable();
    users.dedup();
    Ok(users)
}

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
