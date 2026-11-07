#!/usr/bin/env bash
# verify.sh — check that the micro:bit authenticator actually works.
#
# Run it from anywhere:
#     ./verify.sh                 # automatic checks (no button press needed)
#     ./verify.sh --press         # ... plus a final "press button A" check
#     ./verify.sh --no-hardware   # only the checks that need no board
#
# Checks marked [HARDWARE] need the micro:bit plugged in. Nothing here touches
# PAM or /etc. Prints PASS/FAIL per check, exits non-zero if anything failed.

set -u
cd "$(dirname "$0")" || exit 1
HOST=../../host/microbit-authenticator
LOGS="${TMPDIR:-/tmp}/verify-authenticate"
mkdir -p "$LOGS"

HW=1
PRESS=0
for a in "$@"; do
  case "$a" in
    --no-hardware) HW=0 ;;
    --press) PRESS=1 ;;
    *) echo "unknown option: $a" >&2; exit 2 ;;
  esac
done

pass=0
fail=0
step() { echo; echo "===== CHECK $1: $2 ====="; }
ok()   { echo "     PASS: $1"; pass=$((pass+1)); }
bad()  { echo "     FAIL: $1"; fail=$((fail+1)); }
run()  { # run <name> <cmd...>: log to $LOGS/<name>.log
  local name=$1; shift
  "$@" >"$LOGS/$name.log" 2>&1
}

# ---------------------------------------------------------------------------
step 1 "firmware compiles (debug + release)"
if run build-debug cargo build --target thumbv7em-none-eabihf \
   && run build-release cargo build --release --target thumbv7em-none-eabihf; then
  ok "debug and release firmware build"
else
  bad "see $LOGS/build-*.log"
fi

# ---------------------------------------------------------------------------
step 2 "shared logic unit tests (protocol, crypto, device rules, LED patterns)"
if run lib cargo test --lib --no-default-features; then
  ok "$(grep -E '^test result' "$LOGS/lib.log" | head -1)"
else
  bad "see $LOGS/lib.log"
fi

# ---------------------------------------------------------------------------
step 3 "host tool tests (incl. end-to-end against a fake board)"
if (cd "$HOST" && run host cargo test); then
  ok "host tests pass ($(grep -cE '^test .* ok$' "$LOGS/host.log") tests)"
else
  bad "see $LOGS/host.log"
fi

if [ "$HW" -eq 1 ]; then
  # -------------------------------------------------------------------------
  step 4 "on-device tests: logic suites on the REAL micro:bit [HARDWARE]"
  echo "     (temporarily flashes a test binary; check 5 puts the firmware back)"
  if run ondevice cargo test --target thumbv7em-none-eabihf --test ondevice; then
    ok "all on-device tests passed on the board"
  else
    bad "see $LOGS/ondevice.log"
  fi

  # -------------------------------------------------------------------------
  step 5 "flash the debug firmware [HARDWARE]"
  sleep 2 # the debug probe sometimes needs a moment after the test run
  if run flash cargo embed --target thumbv7em-none-eabihf \
     || { sleep 2; run flash cargo embed --target thumbv7em-none-eabihf; }; then
    ok "firmware flashed"
  else
    bad "see $LOGS/flash.log"
  fi

  # -------------------------------------------------------------------------
  step 6 "host <-> board over the real serial port [HARDWARE]"
  echo "     (ping, junk lines, auth window timeout — do NOT press button A, ~6 s)"
  (cd "$HOST" && cargo build -q 2>/dev/null)
  sleep 1 # let the board boot
  if (cd "$HOST" && run hardware cargo test --test hardware -- --ignored --test-threads=1); then
    ok "board answers, ignores junk, times out and recovers"
  else
    bad "see $LOGS/hardware.log"
  fi

  # -------------------------------------------------------------------------
  if [ "$PRESS" -eq 1 ]; then
    step 7 "real button press [HARDWARE, needs you]"
    echo "     >>> press button A when the middle row blinks (30 s) <<<"
    if "$HOST/target/debug/microbit-authenticator" btn-test; then
      ok "button A press seen by the host"
    else
      bad "no press detected"
    fi
  fi
fi

# ---------------------------------------------------------------------------
echo
echo "======================================"
echo "  PASSED: $pass   FAILED: $fail   (logs: $LOGS)"
echo "======================================"
[ "$fail" -eq 0 ]
