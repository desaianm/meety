# Attribution

Meety includes code and artwork from the GPL v3 Hyprnote repository at <https://github.com/bahodirr/hyprnote>, commit `1026f13c41a07c85c9695e34aa597f92003c809a`.

- `vendor/hypr-capture/src/mic.rs`: adapted from `crates/audio/src/mic.rs`.
- `vendor/hypr-capture/src/speaker.rs`: adapted from `crates/audio/src/speaker/macos.rs`.
- `packaging/Meetlocal.icns`: the upstream `apps/desktop/src-tauri/icons/stable/icon.icns` asset.

The original GPL v3 license is included both at the repository root and within the vendored crate. Changes to the capture code are described in `vendor/hypr-capture/UPSTREAM.md`. The icon is included as source artwork so the app can be built without a separate Hyprnote checkout.

The app also depends on Moonshine and other third-party Rust libraries. Their source locations and pinned versions are recorded in `Cargo.toml` and `Cargo.lock`.
