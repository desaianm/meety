#!/usr/bin/env python3
"""Sign all local builds with the same certificate-bound app identity."""
import json
import os
from pathlib import Path
import re
import subprocess
import sys

app = sys.argv[1]
override = os.environ.get("MEETLOCAL_SIGN_IDENTITY")
args = ["codesign", "--force", "--identifier", "local.meetlocal.app", "--timestamp=none"]
if override:
    if override == "-":
        raise SystemExit("Ad hoc signing changes the permission identity. Choose a certificate instead.")
    args += ["--sign", override]
else:
    config = Path(os.environ.get("MEETLOCAL_SIGNING_CONFIG", str(Path.home() / "Library/Application Support/Meetlocal Build/signing.json")))
    if not config.exists():
        raise SystemExit("Run python3 scripts/setup-signing.py once before building. A persistent identity is required to retain audio permissions.")
    identity = json.loads(config.read_text())
    fingerprint = identity["identity"]
    if not re.fullmatch(r"[0-9A-Fa-f]{40}", fingerprint):
        raise SystemExit("Invalid signing certificate fingerprint")
    requirement = f'designated => identifier "local.meetlocal.app" and certificate leaf = H"{fingerprint}"'
    args += ["--sign", fingerprint, "--keychain", identity["keychain"], "--requirements", "=" + requirement]
subprocess.run(args + [app], check=True)
subprocess.run(["codesign", "--verify", "--strict", app], check=True)
