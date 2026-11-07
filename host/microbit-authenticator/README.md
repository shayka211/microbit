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
cd host/microbit-authenticator
cargo build --release
target/release/microbit-authenticator --help
```

## Code layout

| File | What |
|---|---|
| `src/link.rs` | protocol client over any `Read + Write` (auth, ping, btn-test, provision, lock) |
| `src/port.rs` | finding / opening the serial port |
| `src/keyfile.rs` | loading / saving the host key |
| `src/main.rs` | CLI + exit codes only |
| `tests/fake_device.rs` | end-to-end tests against an in-memory board running the firmware's `authenticate::device` logic |
| `tests/hardware.rs` | the same against the real board (`--ignored`) |

## Subcommands and exit codes

| Command | |
|---|---|
| `auth` (default) | PAM entry point: send nonce, verify MAC → `0` success, `1` failure, `2` config error |
| `test`           | same as `auth`, with prompts for a human |
| `btn-test`       | exits `0` only after a real press of button A |
| `ping`           | reachability probe (`PING`/`PONG`) |
| `monitor`        | print everything the board sends (frames + firmware debug logs) |
| `keygen`         | print a fresh 64-hex 32-byte secret (`--out` creates the file with mode 0600) |
| `provision`      | write the secret into UICR, then lock readback (`--key`, or the host key file; `--no-lock` to stay re-provisionable) |

Options: `--port` (or `MICROBIT_PORT`), `--timeout-ms`, `-v/--verbose` (show
every line from the board).

Default timeouts leave 2 s on top of the board's own window (auth 7 s,
btn-test 32 s, others 3 s), so the board's verdict always arrives before the
host gives up.

Port: `--port`, else `/dev/microbit-auth` (udev rule), else the DAPLink
`/dev/serial/by-id/*micro:bit*if01*`. There is no "any /dev/ttyACM*" fallback,
so challenges never go to an unrelated serial device.

## Host key store

Default `/etc/microbit-auth/secret.key` (root:root, 0600) or `MICROBIT_KEY_FILE`.

## Provisioning quick start

1. Flash the firmware (see `src/12-authenticate`).
2. `cargo build --release`, then `sudo deploy/install.sh --no-pam` → binary, key dir, udev rule
   (drop `--no-pam` to also add the PAM lines).
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

See `src/12-authenticate/VERIFY.md` (or just run `src/12-authenticate/verify.sh`).

```text
cargo test                                                  # fake-board end-to-end + shared protocol
cargo test --test hardware -- --ignored --test-threads=1    # real board, no press needed
```
