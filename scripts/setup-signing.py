#!/usr/bin/env python3
"""Create/reuse a local signing identity without changing certificate trust."""
import hashlib
import json
import os
from pathlib import Path
import re
import secrets
import subprocess
import tempfile

os.umask(0o077)
name = "Meetlocal Local Signing"
config = Path(os.environ.get("MEETLOCAL_SIGNING_CONFIG", str(Path.home() / "Library/Application Support/Meetlocal Build/signing.json")))


def command(args, input=None):
    result = subprocess.run(args, input=input, capture_output=True, text=True)
    if result.returncode:
        raise SystemExit(f"{args[0]} failed: {result.stderr.strip()}")
    return result.stdout


old = json.loads(config.read_text()) if config.exists() else None
keychain = old["keychain"] if old else command(["security", "default-keychain", "-d", "user"]).strip().strip('"')
existing = subprocess.run(["security", "find-certificate", "-c", name, "-p", keychain], capture_output=True, text=True)
if existing.returncode:
    if old:
        raise SystemExit("The saved signing certificate is missing. Restore it in Keychain Access; no new identity was generated.")
    with tempfile.TemporaryDirectory(prefix="meetlocal-signing-") as temporary:
        temporary = Path(temporary)
        settings = temporary / "openssl.cnf"
        settings.write_text("""[req]
distinguished_name=subject
x509_extensions=extensions
prompt=no
[subject]
CN=Meetlocal Local Signing
[extensions]
basicConstraints=critical,CA:FALSE
keyUsage=critical,digitalSignature
extendedKeyUsage=critical,codeSigning
subjectKeyIdentifier=hash
""")
        key = temporary / "key.pem"
        certificate = temporary / "certificate.pem"
        package = temporary / "identity.p12"
        command(["/usr/bin/openssl", "req", "-new", "-x509", "-newkey", "rsa:3072", "-nodes", "-sha256", "-days", "3650", "-config", str(settings), "-keyout", str(key), "-out", str(certificate)])
        # Pass the one-use wrapping secret through stdin, never argv. Import
        # makes the key non-extractable and grants automatic access to codesign.
        password = secrets.token_urlsafe(32) + "\n"
        command(["/usr/bin/openssl", "pkcs12", "-export", "-inkey", str(key), "-in", str(certificate), "-name", name, "-out", str(package), "-passout", "stdin"], input=password)
        helper = Path(__file__).with_name("import-signing.swift")
        command(["xcrun", "swift", str(helper), str(package), keychain], input=password)
existing_pem = command(["security", "find-certificate", "-c", name, "-p", keychain])
der = subprocess.run(["/usr/bin/openssl", "x509", "-outform", "DER"], input=existing_pem.encode(), capture_output=True, check=True).stdout
fingerprint = hashlib.sha1(der).hexdigest().upper()
identities = command(["security", "find-identity", "-p", "codesigning", keychain])
if not re.search(r"\b" + fingerprint + r"\b", identities, re.IGNORECASE):
    raise SystemExit("Certificate exists but its signing key is missing. Restore the identity in Keychain Access before building; no replacement was generated.")
if old:
    if old["identity"] != fingerprint:
        raise SystemExit("Existing signing configuration points to a different identity. Restore that certificate; changing it would reset permissions.")
config.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
temporary_config = config.with_suffix(".tmp")
temporary_config.write_text(json.dumps({"identity": fingerprint, "keychain": keychain, "name": name}, indent=2) + "\n")
temporary_config.replace(config)
print(f"Persistent signing identity ready: {name}. Configuration: {config}")
