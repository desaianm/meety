# Hyprnote capture attribution

Source: https://github.com/bahodirr/hyprnote at `1026f13c41a07c85c9695e34aa597f92003c809a`.

`src/mic.rs` comes from `crates/audio/src/mic.rs`; `src/speaker.rs` comes from `crates/audio/src/speaker/macos.rs`. Both are distributed under Hyprnote's GPL v3 license (included).

The extracted crate preserves its microphone/CPAL and system-audio/Core Audio process-tap implementations. Changes remove the microphone's unused growing sample history, bound the input queue, propagate startup failures, avoid teardown panics, and harden wake-up and overflow reporting. Default microphone selection avoids probing every installed input device. The extracted crate also exposes AVFoundation microphone permission state and an explicit asynchronous permission request before capture starts. A small local `AsyncSource` trait avoids linking the parent workspace's unused LLM dependencies. The parent source tree remains unchanged.

Meety also adds a passive Core Audio process-input activity query for possible-call hints. This reads per-process metadata, excludes the caller, and opens no audio device or process tap.

System-audio sample rates now come from the tap buffer ASBD as an immutable
`f64`, rather than a mutable aggregate-device nominal-rate property. Invalid
nominal metadata cannot relabel queued samples. A non-realtime check before
capture and every 500 ms rejects actual tap-format or positive nominal-rate
changes, so the app can finalize and transcribe partial audio with a warning
instead of silently changing playback speed. New recording is required after
a genuine route-format change; automatic route reconstruction is not included.
