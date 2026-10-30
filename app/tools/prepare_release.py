#!/usr/bin/env python3
"""Prepare a release build of the desktop app (run from the repo root, after
`cargo build --release -p aetherdv`).

* copies aetherdv and aetherdv-cli into app/src-tauri/binaries/ with the
  target-triple suffix Tauri's externalBin expects;
* writes app/src-tauri/tauri.release.conf.json (merged with --config): the
  bundled engine, updater artifacts, and the update public key;
* with --tag, refuses to continue if the tag doesn't match the app version.

Usage: prepare_release.py --target TRIPLE [--pubkey-file F | env UPDATER_PUBKEY]
                          [--tag vX.Y.Z] [--require-key]
"""
import argparse, json, os, pathlib, shutil, sys

ROOT = pathlib.Path(__file__).resolve().parents[2]
TAURI = ROOT / "app" / "src-tauri"

def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--target", required=True)
    ap.add_argument("--pubkey-file")
    ap.add_argument("--tag")
    ap.add_argument("--require-key", action="store_true",
                    help="fail if no public key / signing key (release tags)")
    a = ap.parse_args()

    version = json.loads((TAURI / "tauri.conf.json").read_text())["version"]
    if a.tag and a.tag.lstrip("v") != version:
        sys.exit(f"tag {a.tag} does not match the app version {version} (app/src-tauri/tauri.conf.json)")

    exe = ".exe" if "windows" in a.target else ""
    out = TAURI / "binaries"
    out.mkdir(exist_ok=True)
    for name in ("aetherdv", "aetherdv-cli"):
        src = ROOT / "target" / "release" / f"{name}{exe}"
        if not src.is_file():
            sys.exit(f"{src} missing: run cargo build --release -p aetherdv first")
        shutil.copy2(src, out / f"{name}-{a.target}{exe}")

    pubkey = ""
    if a.pubkey_file:
        pubkey = pathlib.Path(a.pubkey_file).read_text().strip()
    elif os.environ.get("UPDATER_PUBKEY"):
        pubkey = os.environ["UPDATER_PUBKEY"].strip()
    signing = bool(os.environ.get("TAURI_SIGNING_PRIVATE_KEY"))
    if a.require_key and not (pubkey and signing):
        sys.exit("release builds need the UPDATER_PUBKEY variable and the TAURI_SIGNING_PRIVATE_KEY secret (docs/releasing.md)")
    if pubkey and not signing:
        print("warning: public key set but no signing key; building without updater artifacts", file=sys.stderr)

    conf = {"bundle": {"externalBin": ["binaries/aetherdv", "binaries/aetherdv-cli"],
                       "createUpdaterArtifacts": bool(pubkey and signing)}}
    if pubkey:
        conf["plugins"] = {"updater": {"pubkey": pubkey}}
    (TAURI / "tauri.release.conf.json").write_text(json.dumps(conf, indent=2) + "\n")
    print(f"prepared {version} for {a.target}: updater artifacts {'on' if conf['bundle']['createUpdaterArtifacts'] else 'off'}")

if __name__ == "__main__":
    main()
