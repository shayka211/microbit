# microbit-authenticator (host)

Host side of the micro:bit challenge–response login (PAM helper on CachyOS/Arch).

```
 ┌ SDDM greeter / KDE lock screen ─► pam_exec.so ─► microbit-authenticator ─► /dev/ttyACM0 ─► micro:bit
 │                                                                                              │
 │  exit 0 (sufficient)  login     nonce ──► HMAC-SHA256(key, nonce) ◄── button A press          │
 └───────────────────────────────────────────────────────────────────────────────────────────────┘
```

Shared framework with the firmware: both link the `authenticate` crate, so the
frame codec and HMAC math are identical on both ends by construction.

## Build / run

```text
cargo build -p microbit-authenticator --release
target/release/microbit-authenticator --help
```

## Subcommands and exit codes

| `auth` (default) | PAM entry point: send nonce, verify MAC → `0` success, `1` failure, `2` config error |
| `test`           | interactive HITL check (same as `auth`, more chatter) |
| `btn-test`       | `BTN_TEST` self-test: exits `0` only when the device writes after a real click |
| `ping`           | reachability probe (`PING`/`PONG`) |
| `keygen`         | print a fresh 64-hex 32-byte secret (`--out` writes it mode 0600) |
| `provision`      | flash the secret into UICR + enable readback protection (`--key` or the host key file) |

Options: `--port` (or `MICROBIT_PORT`), `--timeout-ms`, `-v/--verbose`.

The device is auto-discovered via `/dev/serial/by-id/*CMSIS-DAP*if01*` (fallback:
scan `/dev/ttyACM*`); `--port` overrides it.

## Host key store

Default `/etc/microbit-auth/secret.key` (root:root, 0600) or `MICROBIT_KEY_FILE`.

## Provisioning quick start

1. Flash the firmware (see `src/12-authenticate`).
2. `sudo install.sh` (see `deploy/`) → binary, key dir, udev rule, PAM drop-in.
3. `sudo microbit-authenticator keygen --out /etc/microbit-auth/secret.key`
   (already done by install.sh when the file was missing).
4. `sudo microbit-authenticator provision` — key is written to the micro:bit UICR,
   read back and verified, then `APPROTECT` enables readback lock and the board resets.
5. `microbit-authenticator btn-test` — press Button A when the LED blinks.
6. `loginctl lock-session` — press A = instant unlock; ignore = password prompt.

### Security notes

- The secret is symmetric: anyone with root on the host can impersonate the
  board. Key file is root-only and never logged or passed via argv.
- The board's key is not transferable off the device once `APPROTECT` is set;
  re-provisioning requires the full-chip erase described in the module README.
- Nonces are fresh per attempt, so replays fail; MAC comparison is constant-time.
- Physical possession of the micro:bit is possession of the key (that's the point).

## Tests

```text
cargo test -p microbit-authenticator      # host-side smoke tests
cargo test -p authenticate --lib --no-default-features   # shared logic suite (11 tests)
# on-device (board attached):
cargo test --manifest-path src/12-authenticate/Cargo.toml --target thumbv7em-none-eabihf --test ondevice
```