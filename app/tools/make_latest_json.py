#!/usr/bin/env python3
"""Build the updater feed (latest.json) from signed release bundles.

Usage: make_latest_json.py DIR VERSION BASE_URL NOTES_FILE OUT

DIR holds the bundles and their .sig files (searched recursively). BASE_URL
is where the release assets will be downloadable (…/releases/download/vX).
Every signature must carry `version:VERSION` in its trusted comment, because
the app requires it (requireSignedVersion; prevents downgrades).
LATEST_REQUIRE (comma-separated platform keys) overrides which platforms
must be present (default: linux-x86_64, darwin-aarch64, windows-x86_64).
"""
import base64, datetime, json, os, pathlib, sys

# (suffix, platform keys) — keys follow the updater's {os}-{arch}[-{installer}]
KINDS = [
    (".AppImage", ["linux-x86_64-appimage", "linux-x86_64"]),
    (".deb", ["linux-x86_64-deb"]),
    (".rpm", ["linux-x86_64-rpm"]),
    (".app.tar.gz", ["darwin-aarch64-app", "darwin-aarch64"]),
    ("-setup.exe", ["windows-x86_64-nsis", "windows-x86_64"]),
    (".msi", ["windows-x86_64-msi"]),
]

def trusted_comment(sig_b64: str) -> str:
    text = base64.b64decode(sig_b64).decode()
    for line in text.splitlines():
        if line.startswith("trusted comment:"):
            return line[len("trusted comment:"):].strip()
    return ""

def main():
    if len(sys.argv) != 6:
        sys.exit(__doc__)
    d, version, base, notes_file, out = sys.argv[1:]
    d = pathlib.Path(d)
    platforms = {}
    for sig in sorted(d.rglob("*.sig")):
        bundle = sig.with_suffix("")
        name = bundle.name
        if " " in name:
            sys.exit(f"{name}: rename assets without spaces first (GitHub rewrites them on upload)")
        kind = next((k for k in KINDS if name.endswith(k[0])), None)
        if kind is None:
            continue
        signature = sig.read_text().strip()
        fields = dict(f.split(":", 1) for f in trusted_comment(signature).split("\t") if ":" in f)
        if fields.get("version", "").lstrip("v") != version:
            sys.exit(f"{sig.name}: signature is for version {fields.get('version')!r}, not {version}")
        for key in kind[1]:
            platforms.setdefault(key, {"signature": signature, "url": f"{base.rstrip('/')}/{name}"})
    required = os.environ.get("LATEST_REQUIRE", "linux-x86_64,darwin-aarch64,windows-x86_64").split(",")
    missing = [k for k in required if k not in platforms]
    if missing:
        sys.exit(f"no signed bundle for {missing}")
    feed = {
        "version": version,
        "notes": pathlib.Path(notes_file).read_text().strip(),
        "pub_date": datetime.datetime.now(datetime.timezone.utc).replace(microsecond=0).isoformat().replace("+00:00", "Z"),
        "platforms": platforms,
    }
    pathlib.Path(out).write_text(json.dumps(feed, indent=2) + "\n")
    print(f"latest.json: {version}, {len(platforms)} platform entries")

if __name__ == "__main__":
    main()
