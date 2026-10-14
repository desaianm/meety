#!/bin/bash
set -euo pipefail
project_root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$project_root"
cargo build --release --locked
target_root="$(cargo metadata --no-deps --format-version=1 | python3 -c 'import sys,json; print(json.load(sys.stdin)["target_directory"])')"
app="$project_root/dist/Meetlocal.app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp "$target_root/release/meetlocal" "$app/Contents/MacOS/meetlocal"
cp packaging/Info.plist "$app/Contents/Info.plist"
cp packaging/Meetlocal.icns "$app/Contents/Resources/Meetlocal.icns"
cp LICENSE "$app/Contents/Resources/LICENSE"
cp README.md "$app/Contents/Resources/README.md"
# FoundationModels is an optional OS-provided on-device language model. The
# helper weak-links it and checks macOS/model availability at runtime. Rust
# uses a transcript excerpt if this helper or Apple Intelligence is unavailable.
if xcrun swiftc -parse-as-library -O -target "$(uname -m)-apple-macos14.2" \
    scripts/title-generator.swift -o "$app/Contents/Resources/title-generator"; then
    echo "Built optional on-device title helper"
else
    rm -f "$app/Contents/Resources/title-generator"
    echo "FoundationModels helper unavailable; automatic titles use transcript excerpts" >&2
fi
python3 scripts/sign-app.py "$app"
echo "$app"
