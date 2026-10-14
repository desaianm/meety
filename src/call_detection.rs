//! Passive microphone activity hints. This module never starts a recording.
//! Mic use also includes dictation and voice messages, so the UI says "possible".
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::{
    path::{Path, PathBuf},
    time::Duration,
};

const DEBOUNCE: Duration = Duration::from_secs(2);
const QUIET_RESET: Duration = Duration::from_secs(5);
const COOLDOWN: Duration = Duration::from_secs(60);

#[derive(Clone, Debug, Serialize)]
pub struct Status {
    pub enabled: bool,
    pub possible_call: bool,
    pub unavailable: bool,
}
impl Default for Status {
    fn default() -> Self {
        Self {
            enabled: true,
            possible_call: false,
            unavailable: false,
        }
    }
}
#[derive(Deserialize, Serialize)]
struct Settings {
    enabled: bool,
}

pub struct Monitor {
    settings_file: PathBuf,
    detector: Detector,
}
impl Monitor {
    pub fn new(root: &Path) -> Result<Self> {
        let settings_file = root.join("call-detection.json");
        let enabled = match std::fs::read(&settings_file) {
            Ok(bytes) => {
                serde_json::from_slice::<Settings>(&bytes)
                    .context("Could not read call detection settings")?
                    .enabled
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => true,
            Err(e) => return Err(e).context("Could not read call detection settings"),
        };
        Ok(Self {
            settings_file,
            detector: Detector::new(enabled),
        })
    }
    pub fn status(&self) -> Status {
        self.detector.status.clone()
    }
    pub fn set_enabled(&mut self, enabled: bool) -> Result<Status> {
        crate::store::atomic_write(
            &self.settings_file,
            &serde_json::to_vec(&Settings { enabled })?,
        )?;
        self.detector = Detector::new(enabled);
        Ok(self.status())
    }
    pub fn dismiss(&mut self) -> Status {
        self.detector.dismiss();
        self.status()
    }
    pub fn observe(&mut self, now: Duration, users: Result<Vec<u32>, ()>, busy: bool) -> Status {
        self.detector.observe(now, users, busy);
        self.status()
    }
}

/// One hint per continuous microphone-use session. A brief device mute / handoff
/// does not create another prompt. Ignore lasts until all users stop for 5s.
struct Detector {
    status: Status,
    active_since: Option<Duration>,
    quiet_since: Option<Duration>,
    dismissed: bool,
    last_prompt: Option<Duration>,
}
impl Detector {
    fn new(enabled: bool) -> Self {
        Self {
            status: Status {
                enabled,
                ..Status::default()
            },
            active_since: None,
            quiet_since: None,
            dismissed: false,
            last_prompt: None,
        }
    }
    fn dismiss(&mut self) {
        self.status.possible_call = false;
        self.dismissed = true;
    }
    fn observe(&mut self, now: Duration, users: Result<Vec<u32>, ()>, busy: bool) {
        if !self.status.enabled {
            return;
        }
        let Ok(users) = users else {
            self.status.unavailable = true;
            self.status.possible_call = false;
            self.active_since = None;
            self.quiet_since = None;
            return;
        };
        self.status.unavailable = false;
        if users.is_empty() {
            self.status.possible_call = false;
            let quiet = *self.quiet_since.get_or_insert(now);
            if now.saturating_sub(quiet) >= QUIET_RESET {
                self.active_since = None;
                self.dismissed = false;
            }
            return;
        }
        self.quiet_since = None;
        let active = *self.active_since.get_or_insert(now);
        // Starting by any route consumes this hint. Never re-prompt on the
        // same call once recording/transcription ends, even if the mic remains on.
        if busy {
            self.dismiss();
            return;
        }
        if self.dismissed {
            return;
        }
        let cooled = self
            .last_prompt
            .is_none_or(|last| now.saturating_sub(last) >= COOLDOWN);
        if !self.status.possible_call && now.saturating_sub(active) >= DEBOUNCE && cooled {
            self.status.possible_call = true;
            self.last_prompt = Some(now);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn tick(d: &mut Detector, seconds: u64, users: &[u32], busy: bool) {
        d.observe(Duration::from_secs(seconds), Ok(users.to_vec()), busy);
    }
    #[test]
    fn debounce_ignore_handoffs_and_new_sessions() {
        let mut d = Detector::new(true);
        tick(&mut d, 0, &[40], false);
        tick(&mut d, 1, &[40], false);
        assert!(!d.status.possible_call);
        tick(&mut d, 2, &[40], false);
        assert!(d.status.possible_call);
        d.dismiss();
        tick(&mut d, 70, &[41], false); // helper changes are still the same session
        assert!(!d.status.possible_call);
        tick(&mut d, 71, &[], false);
        tick(&mut d, 73, &[40], false); // brief mute cannot nag again
        assert!(!d.status.possible_call);
        tick(&mut d, 74, &[], false);
        tick(&mut d, 79, &[], false);
        tick(&mut d, 80, &[42], false);
        tick(&mut d, 82, &[42], false);
        assert!(d.status.possible_call);
    }
    #[test]
    fn busy_suppression_consumes_session_and_exit_clears_hint() {
        let mut d = Detector::new(true);
        tick(&mut d, 0, &[40], true);
        tick(&mut d, 5, &[40], false);
        assert!(!d.status.possible_call);
        tick(&mut d, 6, &[], false);
        tick(&mut d, 11, &[], false);
        tick(&mut d, 12, &[40], false);
        tick(&mut d, 14, &[40], false);
        assert!(d.status.possible_call);
        tick(&mut d, 15, &[], false);
        assert!(!d.status.possible_call);
    }
    #[test]
    fn errors_clear_hint_and_recovery_requires_debounce() {
        let mut d = Detector::new(true);
        tick(&mut d, 0, &[40], false);
        tick(&mut d, 2, &[40], false);
        d.observe(Duration::from_secs(3), Err(()), false);
        assert!(d.status.unavailable);
        assert!(!d.status.possible_call);
        tick(&mut d, 70, &[40], false);
        assert!(!d.status.unavailable);
        assert!(!d.status.possible_call);
        tick(&mut d, 72, &[40], false);
        assert!(d.status.possible_call);
    }
    #[test]
    fn settings_survive_restart_and_disable_stops_hints() {
        let root = tempfile::tempdir().unwrap();
        let mut monitor = Monitor::new(root.path()).unwrap();
        assert!(monitor.status().enabled);
        monitor.set_enabled(false).unwrap();
        let mut monitor = Monitor::new(root.path()).unwrap();
        assert!(!monitor.status().enabled);
        assert!(
            !monitor
                .observe(Duration::from_secs(20), Ok(vec![40]), false)
                .possible_call
        );
        monitor.set_enabled(true).unwrap();
        assert!(Monitor::new(root.path()).unwrap().status().enabled);
    }
    #[test]
    fn quick_sessions_are_rate_limited() {
        let mut d = Detector::new(true);
        tick(&mut d, 0, &[40], false);
        tick(&mut d, 2, &[40], false);
        tick(&mut d, 3, &[], false);
        tick(&mut d, 8, &[], false);
        tick(&mut d, 9, &[40], false);
        tick(&mut d, 11, &[40], false);
        assert!(!d.status.possible_call);
        tick(&mut d, 62, &[40], false);
        assert!(d.status.possible_call);
    }
    #[test]
    #[ignore = "read-only native sensor probe; no recording or permission request"]
    fn native_sensor_probe() {
        let before = hypr_capture::microphone_permission();
        let users = hypr_capture::other_microphone_users().unwrap();
        let after = hypr_capture::microphone_permission();
        assert_eq!(before, after);
        assert!(!users.contains(&std::process::id()));
        // Do not print process identities, app names, or user activity.
        println!(
            "Passive Core Audio input probe succeeded; own process excluded; permission unchanged"
        );
    }
}
