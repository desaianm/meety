# Hyprnote capture attribution

Source: https://github.com/bahodirr/hyprnote at `1026f13c41a07c85c9695e34aa597f92003c809a`.

`src/mic.rs` comes from `crates/audio/src/mic.rs`; `src/speaker.rs` comes from `crates/audio/src/speaker/macos.rs`. Both are distributed under Hyprnote's GPL v3 license (included).

The extracted crate preserves its microphone/CPAL and system-audio/Core Audio process-tap implementations. Changes remove the microphone's unused growing sample history, bound the input queue, propagate startup failures, avoid teardown panics, and harden wake-up and overflow reporting. Default microphone selection avoids probing every installed input device. The extracted crate also exposes AVFoundation microphone permission state and an explicit asynchronous permission request before capture starts. A small local `AsyncSource` trait avoids linking the parent workspace's unused LLM dependencies. The parent source tree remains unchanged.
