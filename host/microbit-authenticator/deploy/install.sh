#!/usr/bin/env bash
# Host-side deployment for the micro:bit challenge-response login (CachyOS/Arch).
#
# Usage:
#   sudo host/microbit-authenticator/deploy/install.sh            # install/enable
#   sudo host/microbit-authenticator/deploy/install.sh --uninstall
#
# What this does NOT do: flash the firmware or provision the secret into the
# micro:bit. Those steps are printed at the end and live in the module README.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
HOST_DIR="$(dirname "$SCRIPT_DIR")"
REPO_ROOT="$(cd "$HOST_DIR/../.." && pwd)"

BIN="/usr/local/bin/microbit-authenticator"
KEY_DIR="/etc/microbit-auth"
KEY_FILE="$KEY_DIR/secret.key"
PAM_LINE='auth sufficient pam_exec.so quiet log=/var/log/microbit-auth.log /usr/local/bin/microbit-authenticator auth'
PAM_FILES=(/etc/pam.d/sddm /etc/pam.d/kde)
BACKUP_SUFFIX=".bak-microbit-auth"

if [[ $EUID -ne 0 ]]; then
  echo "error: run as root (sudo $0)" >&2
  exit 1
fi

if [[ "${1:-}" == "--uninstall" ]]; then
  echo "== removing PAM line + restoring backups =="
  for pam in "${PAM_FILES[@]}"; do
    [[ -f "$pam" ]] || continue
    grep -qvF "$PAM_LINE" "$pam" || sed -i "/${PAM_LINE}/d" "$pam"
    if [[ -f "${pam}${BACKUP_SUFFIX}" ]]; then
      cp -a "${pam}${BACKUP_SUFFIX}" "$pam"
      rm -f "${pam}${BACKUP_SUFFIX}"
    fi
    echo "restored $pam"
  done
  rm -f /etc/udev/rules.d/99-microbit-auth.rules /var/log/microbit-auth.log
  echo "udev rule removed (reload with: sudo udevadm control --reload-rules)"
  echo "binary + key dir left in place; remove manually if desired:"
  echo "  rm -f $BIN && rm -rf $KEY_DIR"
  exit 0
fi

echo "== build (release) =="
(cd "$REPO_ROOT" && cargo build -p microbit-authenticator --release)

echo "== install binary =="
install -m 0755 -o root -g root "$REPO_ROOT/target/release/microbit-authenticator" "$BIN"

echo "== key store =="
mkdir -p "$KEY_DIR"
chmod 700 "$KEY_DIR"
if [[ ! -f "$KEY_FILE" ]]; then
  "$BIN" keygen --out "$KEY_FILE"
  echo "generated root-only key: $KEY_FILE"
else
  echo "key file already exists (kept untouched): $KEY_FILE"
fi
chmod 600 "$KEY_FILE"
chown root:root "$KEY_FILE"

echo "== udev rule =="
install -m 0644 "$SCRIPT_DIR/99-microbit-auth.rules" /etc/udev/rules.d/99-microbit-auth.rules
udevadm control --reload-rules 2>/dev/null || true
udevadm trigger 2>/dev/null || true

echo "== PAM =="
for pam in "${PAM_FILES[@]}"; do
  [[ -f "$pam" ]] || { echo "skip (not present): $pam"; continue; }
  cp -a "$pam" "${pam}${BACKUP_SUFFIX}"
  if grep -qF "$PAM_LINE" "$pam"; then
    echo "PAM line already present: $pam"
  else
    sed -i "1i$PAM_LINE" "$pam"
    echo "inserted at top (auth runs first): $pam  [backup: ${pam}${BACKUP_SUFFIX}]"
  fi
done

cat <<EOF

== deployed ==
  binary : $BIN
  key    : $KEY_FILE (root:root 0600)
  udev   : /etc/udev/rules.d/99-microbit-auth.rules -> /dev/microbit-auth
  pam    : sufficient pam_exec.so at top of /etc/pam.d/sddm [and kde]

== next steps (board side) ==
  1) flash firmware:  cd $REPO_ROOT/src/12-authenticate
                      cargo embed --features v2 --target thumbv7em-none-eabihf
  2) provision key:   sudo $BIN provision          (uses $KEY_FILE)
  3) button check:    $BIN btn-test                (press Button A when LED waits)
  4) end-to-end:      loginctl lock-session        -> press A = instant unlock, ignore = password

== recovery ==
  sudo $BIN  provision again -> re-flash first (see module README for APPROTECT unlock)
EOF