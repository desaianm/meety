# Possible call detection

Call detection is enabled by default. The app checks Core Audio's per-process
[`kAudioProcessPropertyIsRunningInput`](https://developer.apple.com/documentation/coreaudio/kaudioprocesspropertyisrunninginput)
metadata every two seconds, excluding its own process. It does not open an audio
device, start a process tap, read audio samples, request a permission, or upload
anything. Process IDs are temporary in-memory inputs and never enter the archive
or logs. Audio playback alone is not an input signal.

After two seconds of sustained input use, the menu bar displays **Call?** and its
menu offers **Possible call detected · Start recording** and **Ignore this call**.
The library window also shows an actionable banner. The app does not steal focus,
ask for notification access, or start recording automatically. Clicking Start
recording follows the ordinary explicit recording flow and its existing macOS
audio permissions.

**Ignore** suppresses the current continuous microphone session, including brief
mutes and process handoffs. All other microphone users must stop for at least
five seconds to begin a new session, and prompts have a 60-second cooldown.
Recording, audio preparation and transcription suppress and consume hints for
the same ongoing session. A sensor error clears the hint and shows an unavailable
state; manual recording remains available.

Uncheck **Detect possible calls** in the library sidebar or menu bar to disable
polling and hints. The preference is stored as a boolean in `call-detection.json`
under the existing private app-data directory, separate from meeting records.

Microphone activity is a hint rather than proof of a meeting. Dictation, a voice
message, or an app keeping an input stream open can also trigger it. System-muted
calls may remain detectable while the call app keeps its input stream running;
apps that completely stop their input stream cannot be detected during that time.

## Checks

`cargo test --locked call_detection` exercises debounce, dismissal, cooldown,
busy suppression, input exit, sensor-error recovery and persistence using
synthetic input snapshots. `node scripts/test-call-detection-ui.cjs` checks the
actual shipped controls, including that a hint cannot start a recording.

The optional `cargo test --locked native_sensor_probe -- --ignored --nocapture`
reads native activity metadata once, checks the current process is excluded and
confirms the microphone permission state is unchanged. It never records audio
and does not print process identities or activity counts.
