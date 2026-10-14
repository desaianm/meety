#!/usr/bin/env python3
"""Regression: changed local builds must satisfy each other's identity."""
import json
from pathlib import Path
import plistlib
import re
import shutil
import subprocess
import sys
import tempfile

project = Path(__file__).resolve().parent.parent
source = Path(sys.argv[1]) if len(sys.argv) > 1 else project / "dist/Meetlocal.app"
with tempfile.TemporaryDirectory(prefix="meetlocal-signing-check-") as temporary:
    builds = []
    for version in ["100", "101"]:
        app = Path(temporary) / version / "Meetlocal.app"
        shutil.copytree(source, app)
        plist = app / "Contents/Info.plist"
        data = plistlib.loads(plist.read_bytes())
        data["CFBundleVersion"] = version
        plist.write_bytes(plistlib.dumps(data))
        subprocess.run([sys.executable, str(project / "scripts/sign-app.py"), str(app)], check=True, capture_output=True)
        displayed = subprocess.run(["codesign", "-d", "-r-", "--verbose=4", str(app)], check=True, capture_output=True, text=True)
        text = displayed.stdout + displayed.stderr
        requirement = re.search(r"designated => (.+)", text)[1]
        cdhash = re.search(r"^CDHash=(.+)$", text, re.MULTILINE)[1]
        builds.append((app, requirement, cdhash))
    assert builds[0][2] != builds[1][2], "The fixture must change the signed build hash"
    assert builds[0][1] == builds[1][1], "Changed builds lost their permission identity"
    assert "certificate" in builds[0][1], "Identity must be bound to a signing certificate"
    for current, expected in [(builds[0], builds[1]), (builds[1], builds[0])]:
        subprocess.run(["codesign", "--verify", "--strict", "-R", "=" + expected[1], str(current[0])], check=True, capture_output=True)
    print(json.dumps({"changed_hashes": True, "stable_designated_requirement": True, "cross_build_identity_verification": True}))
