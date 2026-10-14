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
    helpers = []
    for version in ["100", "101"]:
        app = Path(temporary) / version / "Meetlocal.app"
        shutil.copytree(source, app)
        plist = app / "Contents/Info.plist"
        data = plistlib.loads(plist.read_bytes())
        data["CFBundleVersion"] = version
        plist.write_bytes(plistlib.dumps(data))
        helper = app / "Contents/Resources/title-generator"
        # Synthetic helper changes independently of the outer bundle version.
        # Never execute or replace the installed application's real helper.
        subprocess.run(["xcrun", "clang", "-x", "c", "-", "-o", str(helper)], input=f"int main(void) {{ return {int(version)}; }}", text=True, check=True, capture_output=True)
        subprocess.run([sys.executable, str(project / "scripts/sign-app.py"), str(app)], check=True, capture_output=True)
        displayed = subprocess.run(["codesign", "-d", "-r-", "--verbose=4", str(app)], check=True, capture_output=True, text=True)
        text = displayed.stdout + displayed.stderr
        requirement = re.search(r"designated => (.+)", text)[1]
        cdhash = re.search(r"^CDHash=(.+)$", text, re.MULTILINE)[1]
        builds.append((app, requirement, cdhash))
        displayed = subprocess.run(["codesign", "-d", "-r-", "--verbose=4", str(helper)], check=True, capture_output=True, text=True)
        text = displayed.stdout + displayed.stderr
        helpers.append((helper, re.search(r"designated => (.+)", text)[1], re.search(r"^CDHash=(.+)$", text, re.MULTILINE)[1]))
    assert builds[0][2] != builds[1][2], "The fixture must change the signed build hash"
    assert builds[0][1] == builds[1][1], "Changed builds lost their permission identity"
    assert "certificate" in builds[0][1], "Identity must be bound to a signing certificate"
    for current, expected in [(builds[0], builds[1]), (builds[1], builds[0])]:
        subprocess.run(["codesign", "--verify", "--strict", "-R", "=" + expected[1], str(current[0])], check=True, capture_output=True)
    assert helpers[0][2] != helpers[1][2], "Helper fixture did not change"
    assert helpers[0][1] == helpers[1][1], "Helper identity changed between builds"
    for current, expected in [(helpers[0], helpers[1]), (helpers[1], helpers[0])]:
        subprocess.run(["codesign", "--verify", "--strict", "-R", "=" + expected[1], str(current[0])], check=True, capture_output=True)
    print(json.dumps({"changed_hashes": True, "stable_designated_requirement": True, "cross_build_identity_verification": True, "nested_helper_identity_verification": True}))
