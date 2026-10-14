#!/usr/bin/env python3
"""Sign all local builds with the same certificate-bound app identity."""
import json
import os
from pathlib import Path
import re
import subprocess
import sys

app = Path(sys.argv[1])
override = os.environ.get("MEETLOCAL_SIGN_IDENTITY")
if override:
    if override == "-":
        raise SystemExit("Ad hoc signing changes the permission identity. Choose a certificate instead.")
    signing = ["--sign", override]
    fingerprint = None
else:
    config = Path(os.environ.get("MEETLOCAL_SIGNING_CONFIG", str(Path.home() / "Library/Application Support/Meetlocal Build/signing.json")))
    if not config.exists():
        raise SystemExit("Run python3 scripts/setup-signing.py once before building. A persistent identity is required to retain audio permissions.")
    identity = json.loads(config.read_text())
    fingerprint = identity["identity"]
    if not re.fullmatch(r"[0-9A-Fa-f]{40}", fingerprint):
        raise SystemExit("Invalid signing certificate fingerprint")
    signing = ["--sign", fingerprint, "--keychain", identity["keychain"]]

def sign(path, identifier):
    args = ["codesign", "--force", "--identifier", identifier, "--timestamp=none"] + signing
    if fingerprint:
        requirement = f'designated => identifier "{identifier}" and certificate leaf = H"{fingerprint}"'
        args += ["--requirements", "=" + requirement]
    subprocess.run(args + [str(path)], check=True)
    subprocess.run(["codesign", "--verify", "--strict", str(path)], check=True)

# Seal nested executables before signing the containing app. Each helper keeps
# its own identifier but reuses the same persistent local signing certificate.
helper = app / "Contents/Resources/title-generator"
if helper.is_file():
    sign(helper, "local.meetlocal.app.title-generator")
sign(app, "local.meetlocal.app")
