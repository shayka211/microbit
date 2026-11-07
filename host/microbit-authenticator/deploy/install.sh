#!/usr/bin/env bash
# Install the micro:bit authenticator so `sudo` accepts EITHER button A on the
# micro:bit OR your password.
#
# Each step is a separate command, so nothing touches sudo until you ask:
#
#   (cd host/microbit-authenticator && cargo build --release)   # as your user
#   sudo deploy/install.sh install        # binary + key + udev + button-only TEST service
#   sudo deploy/install.sh status         # what is installed right now
#   sudo deploy/install.sh enable-sudo    # add the button line to /etc/pam.d/sudo
#   sudo deploy/install.sh disable-sudo   # remove it again (exactly our 2 lines)
#   sudo deploy/install.sh uninstall      # remove everything except the key
#
# Safety:
# - The sudo line is `sufficient`: if the button check fails for ANY reason
#   (no press, board unplugged, wrong key, crash) sudo asks for the password
#   as before. Only our two lines are ever added/removed; the file is
#   rewritten atomically and checked to be otherwise unchanged.
# - `install` never touches /etc/pam.d/sudo. It adds a separate
#   /etc/pam.d/microbit-test service (button only, no password) to try first.
#
# Testing this script itself: MICROBIT_INSTALL_ROOT=/some/dir runs everything
# against <dir>/etc, <dir>/usr/... instead of the real system (no root needed).
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
HOST_DIR="$(dirname "$SCRIPT_DIR")"
R="${MICROBIT_INSTALL_ROOT:-}" # empty = the real system

BIN_PATH="/usr/local/bin/microbit-authenticator" # path as seen by PAM
BIN="$R$BIN_PATH"
KEY_DIR="$R/etc/microbit-auth"
KEY_FILE="$KEY_DIR/secret.key"
UDEV_RULE="$R/etc/udev/rules.d/99-microbit-auth.rules"
SUDO_PAM="$R/etc/pam.d/sudo"
TEST_PAM="$R/etc/pam.d/microbit-test"
LOG_PATH="/var/log/microbit-auth.log"
BACKUP="$SUDO_PAM.bak-microbit-auth"

# pam_exec options:
#   quiet    no "pam_exec failed" noise when the button is not pressed
#   stdout   show the helper's "press button A" line in your terminal
#   seteuid  run the helper as root (sudo's effective user), so it can read
#            the root-only key file — without this it runs as you and fails
#   log=     the helper's diagnostics (stderr) go here
PAM_LINE="auth       sufficient   pam_exec.so quiet stdout seteuid log=$LOG_PATH $BIN_PATH auth"
MARK="# microbit-authenticator: button A or password (managed by install.sh)"

die() { echo "error: $*" >&2; exit 1; }
real_system() { [[ -z "$R" ]]; }

if real_system && [[ $EUID -ne 0 ]]; then
  die "run as root: sudo $0 ${1:-install}"
fi

# ── /etc/pam.d/sudo editing ───────────────────────────────────────────────

# Write $2 (temp file) over $1 atomically, keeping $1's owner and mode.
replace_file() {
  local file=$1 tmp=$2
  chmod --reference="$file" "$tmp"
  real_system && chown --reference="$file" "$tmp"
  mv -f "$tmp" "$file"
}

# Our lines removed from $1, to stdout.
without_our_lines() {
  grep -vxF -e "$PAM_LINE" -e "$MARK" "$1" || true
}

sudo_has_line() { grep -qxF "$PAM_LINE" "$SUDO_PAM"; }

sudo_add() {
  [[ -f "$SUDO_PAM" ]] || die "$SUDO_PAM not found"
  if sudo_has_line; then
    echo "already enabled in $SUDO_PAM"
    return
  fi
  [[ -e "$BACKUP" ]] || cp -a "$SUDO_PAM" "$BACKUP" # first original only

  local tmp
  tmp="$(mktemp "$SUDO_PAM.XXXXXX")"
  # Insert our two lines right before the first `auth` line.
  if ! awk -v mark="$MARK" -v line="$PAM_LINE" '
      !done && /^[[:space:]]*auth[[:space:]]/ { print mark; print line; done = 1 }
      { print }
      END { if (!done) exit 3 }' "$SUDO_PAM" >"$tmp"; then
    rm -f "$tmp"
    die "no 'auth' line in $SUDO_PAM — not touching it"
  fi
  # Paranoia: apart from our lines, the new file must equal the old one.
  if ! diff -q <(without_our_lines "$tmp") "$SUDO_PAM" >/dev/null; then
    rm -f "$tmp"
    die "sanity check failed — $SUDO_PAM left unchanged"
  fi
  replace_file "$SUDO_PAM" "$tmp"
  echo "enabled: added 2 lines to $SUDO_PAM (original saved as $BACKUP)"
}

sudo_remove() {
  [[ -f "$SUDO_PAM" ]] || return 0
  if ! sudo_has_line && ! grep -qxF "$MARK" "$SUDO_PAM"; then
    echo "not enabled in $SUDO_PAM (nothing to remove)"
    return
  fi
  local tmp
  tmp="$(mktemp "$SUDO_PAM.XXXXXX")"
  without_our_lines "$SUDO_PAM" >"$tmp"
  if ! grep -qE '^[[:space:]]*auth[[:space:]]' "$tmp"; then
    rm -f "$tmp"
    die "result would have no auth line — $SUDO_PAM left unchanged"
  fi
  replace_file "$SUDO_PAM" "$tmp"
  echo "disabled: removed our lines from $SUDO_PAM"
}

# ── commands ──────────────────────────────────────────────────────────────

cmd_install() {
  local built="$HOST_DIR/target/release/microbit-authenticator"
  [[ -x "$built" ]] || die "$built not found — build it first, as your user:
  (cd $HOST_DIR && cargo build --release)"

  echo "== binary =="
  mkdir -p "$(dirname "$BIN")"
  if real_system; then
    install -m 0755 -o root -g root "$built" "$BIN"
  else
    install -m 0755 "$built" "$BIN"
  fi
  echo "installed $BIN"

  echo "== key =="
  mkdir -p "$KEY_DIR"
  chmod 700 "$KEY_DIR"
  if [[ -f "$KEY_FILE" ]]; then
    echo "kept existing $KEY_FILE"
  else
    "$BIN" keygen --out "$KEY_FILE" >/dev/null 2>&1
    echo "generated new key $KEY_FILE"
  fi
  chmod 600 "$KEY_FILE"
  real_system && chown root:root "$KEY_FILE" "$KEY_DIR"

  echo "== udev rule (stable /dev/microbit-auth name) =="
  mkdir -p "$(dirname "$UDEV_RULE")"
  install -m 0644 "$SCRIPT_DIR/99-microbit-auth.rules" "$UDEV_RULE"
  if real_system; then
    udevadm control --reload-rules 2>/dev/null || true
    udevadm trigger --subsystem-match=tty 2>/dev/null || true
  fi
  echo "installed $UDEV_RULE"

  echo "== test PAM service (button only, sudo untouched) =="
  mkdir -p "$(dirname "$TEST_PAM")"
  cat >"$TEST_PAM" <<EOF
#%PAM-1.0
# Button-only test service for microbit-authenticator. Nothing uses it except
# you, testing:   sudo pamtester microbit-test \$USER authenticate
$PAM_LINE
auth       required     pam_deny.so
account    required     pam_permit.so
EOF
  chmod 644 "$TEST_PAM"
  echo "installed $TEST_PAM"

  cat <<EOF

Done. /etc/pam.d/sudo was NOT touched. Next:
  1) put the key on the board:  sudo $BIN_PATH provision --no-lock
  2) try the test service:      sudo pamtester microbit-test \$USER authenticate
                                (pamtester is in the AUR; press A -> "successfully authenticated")
  3) when happy:                sudo $0 enable-sudo
EOF
}

cmd_enable_sudo() {
  [[ -x "$BIN" ]] || die "run '$0 install' first"
  [[ -f "$KEY_FILE" ]] || die "no key file $KEY_FILE — run '$0 install' first"
  real_system && { [[ -f /usr/lib/security/pam_exec.so ]] || die "pam_exec.so not found"; }

  if [[ "${2:-}" != "--yes" ]]; then
    cat <<EOF
This adds 2 lines to $SUDO_PAM:
  $MARK
  $PAM_LINE

Before continuing, open a SECOND terminal and run 'sudo -i' there, and keep
it open until you've checked sudo still works ('sudo -k; sudo true').
Undo at any time: sudo $0 disable-sudo   (or copy back $BACKUP)

EOF
    read -r -p "Type 'yes' to continue: " answer
    [[ "$answer" == "yes" ]] || die "aborted, nothing changed"
  fi
  sudo_add
  cat <<EOF

Test now (in a normal terminal):
  sudo -k; sudo true       -> board blinks: press A = root, wait 5 s = password
EOF
}

cmd_status() {
  local ok="yes" no="no"
  echo "binary        : $([[ -x "$BIN" ]] && echo "$ok ($BIN)" || echo $no)"
  echo "key file      : $([[ -f "$KEY_FILE" ]] && echo "$ok ($(stat -c '%U:%G %a' "$KEY_FILE"))" || echo $no)"
  echo "udev rule     : $([[ -f "$UDEV_RULE" ]] && echo $ok || echo $no)"
  echo "board symlink : $([[ -e "$R/dev/microbit-auth" ]] && echo "$ok (/dev/microbit-auth)" || echo "$no (plugged in? udev rule installed?)")"
  echo "test service  : $([[ -f "$TEST_PAM" ]] && echo $ok || echo $no)"
  echo "sudo enabled  : $([[ -f "$SUDO_PAM" ]] && sudo_has_line && echo $ok || echo $no)"
}

cmd_uninstall() {
  sudo_remove
  rm -f "$TEST_PAM" "$UDEV_RULE" "$BIN" "$R$LOG_PATH"
  real_system && udevadm control --reload-rules 2>/dev/null || true
  echo "removed: test service, udev rule, binary, log"
  echo "kept:    $KEY_DIR (delete with: sudo rm -rf $KEY_DIR)"
  [[ -e "$BACKUP" ]] && echo "kept:    $BACKUP (original sudo file, for reference)"
  return 0
}

case "${1:-install}" in
  install) cmd_install ;;
  enable-sudo) cmd_enable_sudo "$@" ;;
  disable-sudo) sudo_remove ;;
  status) cmd_status ;;
  uninstall) cmd_uninstall ;;
  *) die "unknown command '${1}' (install | status | enable-sudo | disable-sudo | uninstall)" ;;
esac
