# microbit-authenticator (host)

Host side of the micro:bit challenge–response check: `sudo` accepts button A
on the micro:bit **or** your password (PAM helper on CachyOS/Arch).

```
 ┌ sudo ─────────────────────────► pam_exec.so ─► microbit-authenticator ─► /dev/ttyACM0 ─► micro:bit
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

## Setting up sudo (button A *or* password)

Everything goes through `deploy/install.sh`; each step is its own command and
only `enable-sudo` touches `/etc/pam.d/sudo`.

1. Flash the release firmware: `cd src/12-authenticate && cargo embed-rel`.
2. `cargo build --release` (as your user), then `sudo deploy/install.sh install`
   → binary in `/usr/local/bin`, root-only key in `/etc/microbit-auth`, udev
   rule, and a button-only **test** PAM service `/etc/pam.d/microbit-test`.
3. `sudo microbit-authenticator provision --no-lock` — the board stores the
   system key (stays re-provisionable; lock later with plain `provision`).
4. Try the test service: `sudo pamtester microbit-test $USER authenticate`
   (AUR `pamtester`). Press A → success; wait → failure.
5. Open a second terminal with `sudo -i` as a safety net, then
   `sudo deploy/install.sh enable-sudo`, and check with `sudo -k; sudo true`.
6. Undo any time: `sudo deploy/install.sh disable-sudo` (removes exactly the
   two added lines). `status` shows what's installed; `uninstall` removes all
   but the key.

What it feels like: `sudo …` → "micro:bit: press button A within 5 s" and the
middle row blinks → press = root. No press, board unplugged or wrong key → the
normal password prompt. Diagnostics: `/var/log/microbit-auth.log`.

### Security notes

- The secret is symmetric: anyone with root on the host can impersonate the
  board. Key file is root-only and never logged or passed via argv.
- The board's key is not transferable off the device once `APPROTECT` is set;
  re-provisioning requires the full-chip erase described in the module README.
- Nonces are fresh per attempt, so replays fail; MAC comparison is constant-time.
- Physical possession of the micro:bit is possession of the key (that's the point).
- The board can't show *which* command asks. Any program running as you could
  start `sudo` and wait for a press — only press A right after you typed sudo
  yourself (same rule as a USB security key).

## Tests

See `src/12-authenticate/VERIFY.md` (or just run `src/12-authenticate/verify.sh`).

```text
cargo test                                                  # fake-board end-to-end + shared protocol
cargo test --test hardware -- --ignored --test-threads=1    # real board, no press needed
```
