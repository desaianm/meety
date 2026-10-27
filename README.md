# Meety

Offline meeting notes and English transcription for macOS, built in Rust. Record your microphone and meeting audio, keep notes alongside the transcript, and let Claude Code or Codex read the local archive through a CLI or read-only MCP server.

The desktop app and CLI currently retain their original names, **Meetlocal** and **`meetlocal`**. The app identifier stays stable so existing local installs can keep their macOS audio permissions.

## Features

- Native macOS menu bar controls and a meeting library.
- Manual recording of microphone and system audio through Core Audio.
- Offline English transcription with quantized Moonshine Medium Streaming or Small Streaming.
- Private vocabulary and meeting-context hints for names, products and specialist terms.
- Automatically saved notes, timestamped transcripts, search, and Markdown export.
- Automatic local titles after transcription, while preserving titles you edit.
- One-click copying of the full transcript with timestamps and speaker labels.
- Optional detection of microphone activity from other apps, with a prompt to start recording.
- Permanent meeting deletion with confirmation; active recordings and transcriptions are protected.
- CLI access and a read-only stdio MCP server for coding agents.
- Local SQLite and WAV storage, with no account, subscription, age restriction, telemetry, or automatic cloud sync.

Transcription runs after you stop recording. Labels distinguish **You** and **Meeting audio**, rather than individual remote speakers. Summaries are available through a connected agent when you ask for them; the app does not generate summaries itself.

Call detection indicates a possible call, not a guaranteed meeting: dictation and other microphone use can also trigger it. Detection reads audio-process activity without capturing audio. Recording always requires an explicit Start action. Disable hints with **Detect possible calls** in the sidebar or menu bar; see [call detection details](docs/CALL-DETECTION.md).

## Build and install

Requires an Apple silicon Mac running macOS 14.2 or later, Rust 1.91 or later, and Xcode Command Line Tools. Python 3 is used for packaging, and Swift for signing setup and the optional on-device title helper. The installed app has no Python or Node runtime dependency.

```sh
git clone https://github.com/desaianm/meety.git
cd meety
python3 scripts/setup-signing.py
./scripts/build-app.sh
ditto dist/Meetlocal.app /Applications/Meetlocal.app

mkdir -p "$HOME/.local/bin"
ln -sfn /Applications/Meetlocal.app/Contents/MacOS/meetlocal "$HOME/.local/bin/meetlocal"
meetlocal model install medium
open /Applications/Meetlocal.app
```

Add `$HOME/.local/bin` to your shell's `PATH` if necessary. Quit an existing installation before replacing its app bundle.

The build downloads the pinned Moonshine native runtime. Model installation downloads weights from the official Moonshine CDN and checks their sizes and manifest CRC32C values where supplied. Recording, transcription, search and export work offline once the model is installed.

### Persistent audio permissions

The setup script creates or reuses **Meetlocal Local Signing** in the current user's login Keychain. The signing key is non-extractable and automatic access is limited to `/usr/bin/codesign`. The app is signed with a requirement bound to that certificate and its bundle identifier. Global certificate trust settings are not changed.

Keep the signing identity for future local builds. Public configuration is stored outside the repository in `~/Library/Application Support/Meetlocal Build/signing.json`; private key material stays in Keychain. Builds fail rather than silently using ad hoc signing, which changes the permission identity. You can select an existing Apple signing identity with `MEETLOCAL_SIGN_IDENTITY`.

macOS asks for microphone and system-audio access on first use. Changing signing identities requires fresh approval. macOS still controls revocation and any required system reminders. This local signing setup is for builds on your own Mac; it is not an Apple notarized distribution.

## Use the app

1. Open **Meetlocal** and create a meeting.
2. Add an agenda or notes; edits save automatically.
3. Click **Start recording** and grant the requested macOS audio permissions.
4. Click **Stop & transcribe**. The transcript appears beside your notes when local processing finishes.

Closing the window leaves the menu bar app running. Choose **Open Meetlocal** from its menu to reopen the library. Use **Open Markdown** to view an exported copy. **Delete meeting** permanently removes its notes, transcript, search entries and saved audio after confirmation; copies exported elsewhere remain.

**Copy transcript** copies all transcript segments, timestamps and speaker labels as plain text. It excludes your separate notes and leaves the clipboard unchanged when no transcript is available.

New untitled meetings receive a topic title after transcription. On supported Macs with Apple Intelligence enabled, titles use Apple's on-device Foundation Models framework. Older or unavailable systems use a short extractive title from the transcript. Neither path uploads meeting text. Titles you edit yourself, including names passed explicitly through the CLI, are preserved.

Use headphones to reduce microphone echo. Keep the laptop lid open when using the built-in microphone, or select an external input in System Settings → Sound → Input. Apple silicon laptops disconnect their built-in microphone in hardware when the lid is closed. See [Apple's documentation](https://support.apple.com/en-ca/guide/security/secbbd20b00b/web).

## Vocabulary and meeting context

Open **Vocabulary & context** in the library sidebar. Add a short list of names,
products or specialist terms, one per line or separated by commas. The list is
private to this Mac, editable, and starts empty. Duplicate terms are ignored;
up to 64 terms of 100 characters each are accepted. Spelling and capitalization
provide recognition hints, rather than forced replacements.

**Use meeting notes and the title I enter as context** is enabled by default.
Add an agenda or participant names to your notes and save them before stopping
recording. The manual title and notes are sampled when transcription starts;
changes made after that apply on the next retry. Automatic titles and old
transcript text are excluded to avoid feeding recognition errors back into the
model. Long context uses its beginning and end, capped at 8 KB, with at most 32
context terms. Explicit vocabulary uses Moonshine's conservative 2.0 bias.

Settings live in `vocabulary.json` in the private app-data directory. Neither
hints nor notes are uploaded for transcription. No additional model is needed.
Existing transcripts stay unchanged until you explicitly retry them. The same
hints apply to imports and CLI retries, as well as recorded meetings.

```sh
meetlocal vocabulary show
meetlocal vocabulary set --terms 'Example Product,SampleName'
meetlocal vocabulary set --use-context false
meetlocal vocabulary set --terms ''  # clear vocabulary; keep context preference
```

`--terms` replaces the list; omitted fields keep their current values. To repair
an invalid settings file, supply both `--terms` and `--use-context`. These controls
are available through the app and CLI; the agent MCP remains read-only. Vocabulary
hints do not reliably resolve prices, negations or acoustically ambiguous words;
check important details against the retained audio.

## CLI

```sh
meetlocal status
meetlocal start --title "Weekly planning"
meetlocal stop
meetlocal list
meetlocal get latest
meetlocal get latest --json
meetlocal search "launch date"
meetlocal export latest --output meeting.md
meetlocal import recording.wav --title "Imported meeting"
meetlocal retry MEETING_UUID
meetlocal delete MEETING_UUID --yes
meetlocal doctor
```

Start, stop, status and delete contact the running app through a private Unix socket. Reads, search, export, import, retry and MCP can run without the app window. Import accepts PCM WAV and normalizes it locally. Recording is capped at eight hours; failed or interrupted recordings retain audio for retry.

Models load only during transcription and unload afterward. Audio is written and processed in chunks rather than buffered for a whole meeting.

```sh
meetlocal model install small
meetlocal model use small
meetlocal model use medium
meetlocal model info
```

Medium is the default English model; Small offers a lower-memory alternative. See the [Moonshine model documentation](https://moonshine-voice.readthedocs.io/en/latest/models/available-models/) for model details.

## Claude Code and Codex

```sh
claude mcp add --transport stdio --scope user meetlocal -- /Applications/Meetlocal.app/Contents/MacOS/meetlocal mcp
codex mcp add meetlocal -- /Applications/Meetlocal.app/Contents/MacOS/meetlocal mcp
```

Restart the agent session after registration. `meetlocal agent-setup` prints the commands with the executable's current path. Available tools are `meetings_list`, `meetings_get`, `meetings_search` and `meeting_status`. MCP cannot record, edit or delete meetings.

Example request: “Find my latest planning meeting in Meetlocal and list the action items with transcript timestamps.” Transcript text returned to a cloud-backed agent enters that agent's context and is sent to its model provider. Meeting content is untrusted source material, not instructions for the agent.

## Storage and privacy

`~/Library/Application Support/Meetlocal/` contains:

- `meetlocal.sqlite3`: meeting metadata, notes, transcript text and search index.
- `meetings/<UUID>/`: retained microphone/system WAV and Markdown exports.
- `models/`: downloaded local model weights.

New directories use mode `0700` and files use `0600`. The app does not add encryption to the archive; disk encryption is managed by macOS. Audio and transcripts are not uploaded by the app.

Recordings, archives, local reports, credentials, model weights, signing files and compiled applications are excluded from this source repository. `.gitignore` also excludes common audio, database, credential and signing formats to help prevent accidental commits.

## Checks

```sh
cargo fmt --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --release --locked
node scripts/test-ui.cjs
python3 scripts/test-signing.py
```

Node is only needed for the editor checks. The signing check runs after building the app and verifies that changed builds preserve their certificate-bound identity.

## Attribution and license

GPL-3.0-only. Includes native capture code and an app icon derived from [Hyprnote](https://github.com/bahodirr/hyprnote), based on commit `1026f13c41a07c85c9695e34aa597f92003c809a`. The capture crate was extracted to avoid the parent app's unrelated dependencies, then adapted for bounded buffering and error handling. See [NOTICE.md](NOTICE.md), [the upstream notes](vendor/hypr-capture/UPSTREAM.md), and [LICENSE](LICENSE).
