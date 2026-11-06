#!/usr/bin/env bash
# verify.sh — check that the micro:bit authenticator actually works.
#
# Run it from this folder (src/12-authenticate):
#     ./verify.sh
#
# It runs 5 checks in order. Checks marked [HARDWARE] need the micro:bit
# plugged in. The rest just need cargo.
#
# Pass/Fail: the script prints PASS/FAIL per check and exits non-zero
# if anything failed, so you can also use it in CI.

set -u
cd "$(dirname "$0")" || exit 1

pass=0
fail=0
step() { echo; echo "===== CHECK $1: $2 ====="; }
ok()   { echo "     PASS: $1"; pass=$((pass+1)); }
bad()  { echo "     FAIL: $1"; fail=$((fail+1)); }

# ---------------------------------------------------------------------------
step 1 "firmware compiles for the board"
echo "     (this is the same build \"cargo embed\" uses)"
if cargo build --features v2 --target thumbv7em-none-eabihf >/tmp/verify-build.log 2>&1; then
  ok "cargo build --features v2 --target thumbv7em-none-eabihf"
else
  bad "see /tmp/verify-build.log"
fi

# ---------------------------------------------------------------------------
step 2 "shared logic unit tests (pure Rust, no hardware)"
if cargo test --lib --no-default-features >/tmp/verify-lib.log 2>&1; then
  ok "lib tests pass (protocol codec, HMAC vectors, debounce/state machines)"
else
  bad "see /tmp/verify-lib.log"
fi

# ---------------------------------------------------------------------------
step 3 "host tool tests (protocol frames parse on both sides)"
if (cd ../../host/microbit-authenticator && cargo test >/tmp/verify-host.log 2>&1); then
  ok "host tests pass (same wire codec used by firmware & host)"
else
  bad "see /tmp/verify-host.log"
fi

# ---------------------------------------------------------------------------
step 4 "on-device tests: run the logic suites on the REAL micro:bit [HARDWARE]"
echo "     (flashes a temporary test binary via SWD; closes its app afterwards)"
if cargo test --features v2 --target thumbv7em-none-eabihf --test ondevice >/tmp/verify-ondevice.log 2>&1; then
  ok "all on-device tests passed on the board"
else
  bad "see /tmp/verify-ondevice.log"
fi

# ---------------------------------------------------------------------------
step 5 "flash the real firmware [HARDWARE]"
echo "     (probe-rs runs the app for a few seconds; expected to be interrupted)"
if cargo build --features v2 --target thumbv7em-none-eabihf >/dev/null 2>&1; then
  timeout 20 probe-rs run --chip nRF52833_xxAA --no-catch-reset \
    ../../target/thumbv7em-none-eabihf/debug/authenticate >/tmp/verify-flash.log 2>&1
  # probe-rs run never exits on its own; timeout(124) + no crash message == booted fine
  code=$?
  if [ "$code" -eq 124 ] \
     && ! grep -qiE "HardFault|Failed to parse defmt|Firmware exited unexpectedly|cannot open" /tmp/verify-flash.log; then
    ok "firmware flashed and booted (no HardFault)"
  else
    bad "see /tmp/verify-flash.log (exit $code)"
  fi
else
  bad "firmware build failed — run the build step again to see the error"
fi

# ---------------------------------------------------------------------------
echo
echo "======================================"
echo "  PASSED: $pass   FAILED: $fail"
echo "======================================"
[ "$fail" -eq 0 ]