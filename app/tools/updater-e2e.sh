#!/usr/bin/env bash
# End-to-end updater test on Linux (AppImage), with a throwaway key:
#   1. build version A with the test public key and a local feed URL;
#   2. build version B (A with the patch number +1), signed;
#   3. publish B's latest.json on a local HTTP server;
#   4. run A under Xvfb with the selftest feature: it must find B, verify
#      it, install it over itself and restart as B.
# Usage: app/tools/updater-e2e.sh WORKDIR     (needs Xvfb, cargo-tauri)
#   SKIP_BUILD=1  reuse the builds in WORKDIR
#   TAMPER=artifact  serve a corrupted package: the update must be refused
#   TAMPER=version   announce B+1 with B's genuine package (downgrade/replay
#                    attempt): refused because the signature names B
set -euo pipefail
W=$(realpath -m "${1:?workdir}"); mkdir -p "$W"
ROOT=$(cd "$(dirname "$0")/../.." && pwd); T=$ROOT/app/src-tauri
CONF=$T/tauri.conf.json; cp "$CONF" "$W/tauri.conf.json.orig"
restore() { cp "$W/tauri.conf.json.orig" "$CONF"; rm -f "$T/tauri.release.conf.json" "$W/e2e.conf.json"; }
trap restore EXIT
A=$(python3 -c "import json;print(json.load(open('$CONF'))['version'])")
B=$(python3 -c "v='$A'.split('.');v[2]=str(int(v[2])+1);print('.'.join(v))")
PORT=18090
[ -f "$W/test.key" ] || cargo tauri signer generate -w "$W/test.key" -p e2e --ci -f >/dev/null
export TAURI_SIGNING_PRIVATE_KEY="$(cat "$W/test.key")" TAURI_SIGNING_PRIVATE_KEY_PASSWORD=e2e
TRIPLE=$(rustc -vV | sed -n 's/^host: //p')
(cd "$ROOT" && cargo build -q --release -p aetherd)
build() { # version outdir
  python3 - "$CONF" "$1" <<'PY'
import json,sys; p,v=sys.argv[1:]; d=json.load(open(p)); d['version']=v; open(p,'w').write(json.dumps(d,indent=2)+"\n")
PY
  (cd "$ROOT" && python3 app/tools/prepare_release.py --target "$TRIPLE" --pubkey-file "$W/test.key.pub")
  # Test-only overrides: local feed over plain HTTP.
  python3 - "$T/tauri.release.conf.json" "$PORT" <<'PY'
import json,sys; p,port=sys.argv[1:]; d=json.load(open(p))
d['plugins']['updater'].update({"endpoints":[f"http://127.0.0.1:{port}/latest.json"],"dangerousInsecureTransportProtocol":True})
open(p,'w').write(json.dumps(d,indent=2))
PY
  (cd "$T" && cargo tauri build --features selftest --bundles appimage --config tauri.release.conf.json >"$W/build-$1.log" 2>&1) \
    || { tail -30 "$W/build-$1.log"; exit 1; }
  mkdir -p "$2"
  for f in "$ROOT"/target/release/bundle/appimage/*_"$1"_*.AppImage*; do n=$(basename "$f"); cp "$f" "$2/${n// /-}"; done
}
if [ -z "${SKIP_BUILD:-}" ]; then build "$A" "$W/a"; build "$B" "$W/b"; fi
echo "Release notes for $B (end-to-end test)." >"$W/notes.txt"
LATEST_REQUIRE=linux-x86_64 python3 "$ROOT/app/tools/make_latest_json.py" "$W/b" "$B" "http://127.0.0.1:$PORT" "$W/notes.txt" "$W/b/latest.json"
SERVE=$W/b
case "${TAMPER:-}" in
  artifact)
    SERVE=$W/serve; rm -rf "$SERVE"; cp -r "$W/b" "$SERVE"
    python3 - "$SERVE"/*.AppImage <<'PY'
import sys; p=sys.argv[1]; b=bytearray(open(p,'rb').read()); b[len(b)//2]^=0xFF; open(p,'wb').write(b)
PY
    ;;
  version)
    SERVE=$W/serve; rm -rf "$SERVE"; cp -r "$W/b" "$SERVE"
    C=$(python3 -c "v='$B'.split('.');v[2]=str(int(v[2])+1);print('.'.join(v))")
    python3 - "$SERVE/latest.json" "$C" <<'PY'
import json,sys; p,v=sys.argv[1:]; d=json.load(open(p)); d['version']=v; open(p,'w').write(json.dumps(d))
PY
    ;;
esac
(cd "$SERVE" && exec python3 -m http.server $PORT --bind 127.0.0.1 >"$W/http.log" 2>&1) & HTTP=$!
trap 'kill $HTTP 2>/dev/null; restore' EXIT
sleep 1
APP=$(ls "$W"/a/*.AppImage); cp "$APP" "$W/installed.AppImage"; chmod +x "$W/installed.AppImage"
rm -f "$W/selftest.log"
export AETHER_SELFTEST_OUT="$W/selftest.log" AETHER_SELFTEST_FROM="$A" APPIMAGE_EXTRACT_AND_RUN=1
# Own X server: the app restarts itself, so the display must outlive it.
Xvfb :87 -screen 0 1280x800x24 >"$W/xvfb.log" 2>&1 & XVFB=$!
trap 'kill $HTTP $XVFB 2>/dev/null; restore' EXIT
sleep 1
DISPLAY=:87 timeout 120 "$W/installed.AppImage" || true
# the restarted process (B) exits by itself once it has logged its version
for _ in $(seq 60); do grep -q "running $B" "$W/selftest.log" 2>/dev/null && break; sleep 1; done
cat "$W/selftest.log"
if [ -n "${TAMPER:-}" ]; then
  if grep -q "install failed" "$W/selftest.log" && cmp -s "$W/installed.AppImage" "$APP"; then
    echo "PASS: tampered ($TAMPER) update refused; $A still installed"
  else
    echo "FAIL: tampered update was not refused"; exit 1
  fi
else
  grep -q "running $B" "$W/selftest.log" && echo "PASS: updated $A -> $B" || { echo "FAIL"; exit 1; }
fi
