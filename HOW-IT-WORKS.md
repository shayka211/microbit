# micro:bit challenge–response authenticator — how it works

Two programs talk over a USB serial link:

| Side | Where | What it is |
|------|-------|------------|
| **firmware** | `src/12-authenticate/` | runs on the **micro:bit v2** (nRF52833), holds the secret key, verifies you physically press Button A |
| **host** | `host/microbit-authenticator/` | runs **on your PC**; the thing PAM calls when you log in to your desktop |

Both share one library, `authenticate` (`src/12-authenticate/src/lib.rs`), which
contains the *pure* logic that is identical on both sides: the wire protocol
codec and the HMAC math. Sharing this single crate is what guarantees both ends
speak exactly the same protocol.

```
┌─ PC ──────────────────────────────────┐        ┌─ micro:bit ────────────────────┐
│ PAM/loginctl ─► microbit-authenticator │        │ firmware (authenticate bin)     │
│              ╲                        │        │                                 │
│              random nonce ────────────┼──USB──►│  UARTE0 + DAPLink bridge         │
│                               │       │        │      │                          │
│                               ▼       │        │      ▼                          │
│   verify HMAC(key, nonce)  ◄─────── MAC ──────────── HMAC_SHA256(key, nonce)     │
│                    ▲                 │        │      ▲                            │
│   exit 0 = success  ┘                │        │      └── Button A pressed         │
└──────────────────────────────────────┘        └───────────────────────────────────┘
```

## The core idea (why it's secure)

Physical possession is the password.

1. A 32-byte secret is provisioned once into the micro:bit's UICR
   (one-time-writable memory region), then readback-protected via `APPROTECT`.
   After that, nobody can read the key off the board — not even you, by design.
2. To log in you must have the board *and* press Button A.
3. The host sends a fresh random **nonce** (challenge).
4. The board computes `HMAC-SHA256(key, nonce)` and sends it back as the
   **MAC** (response) — only after you press the button.
5. The host recomputes the same HMAC with its own copy of the key and compares
   constant-time. Match → exit 0 → PAM lets you in.

Fresh nonce every attempt → replays fail. Key never leaves the board or the
host, only the nonce and MAC travel over the wire.

## The wire protocol (AUTH/1.0)

One frame per line, ASCII, `\r\n` terminated, on the DAPLink UART bridge
(115200 8N1). Defined in `src/12-authenticate/src/protocol.rs`, used by *both*
sides (the host `cargo tree` pulls the same file via `authenticate`).

| Frame | Direction | Meaning |
|-------|-----------|---------|
| `HELLO AUTH/1.0 MAGIC:<0\|1>` | board → host | boot banner; 1 = provisioned |
| `PING` / `PONG` | both | reachability probe |
| `AUTH <64-hex nonce>` | host → board | challenge |
| `READY` | board → host | challenge accepted, await button |
| `AUTH_OK <64-hex mac>` | board → host | MAC after button press |
| `AUTH_TIMEOUT` / `AUTH_ERR busy\|unprovisioned` | board → host | rejected |
| `PROV <64-hex key>` / `PROV_OK` / `PROV_ERR ...` | host ⇄ board | provision a new key |
| `LOCK` / `LOCK_OK` | host ⇄ board | enable `APPROTECT`, then reset |
| `BTN_TEST` / `BTN_TEST_READY` / `BTN_PRESSED <ms>` / `BTN_TEST_TIMEOUT` | host ⇄ board | button self-test |

Full list: `enum Frame` in `protocol.rs`.

---

## Firmware side — `src/12-authenticate/`

Firmware = the `authenticate` crate's **binary** target
(`src/main.rs` + the `mod`s below). The **library** target (`lib.rs`) is the
shared protocol/crypto/state logic, compiled by both the firmware and the host.

| File | What it does |
|------|--------------|
| `src/main.rs` | Entry point & **state machine** (armed vs idle). Spawns the display task, owns serial, handles each incoming frame and the button-press window. |
| `src/lib.rs` | The shared library. `#![no_std]`, hardware-free so the host *and* the on-device tests can link it. |
| `src/protocol.rs` | The AUTH/1.0 **codec**: `Frame::encode`/`Frame::parse`, hex encode/decode, `ParseError`. Same code runs on host & board. |
| `src/crypto.rs` | `HMAC-SHA256` math: `sign(key, nonce)` and constant-time `verify`. Pure, testable on the host. |
| `src/state.rs` | Pure logic: the "armed session" (silent until press-or-timeout) and the button **debounce** filter. No hardware. |
| `src/constants.rs` | Protocol constants (`MAX_FRAME_LEN`, window times) **and** the UICR memory map (key address, magic, APPROTECT). |
| `src/serial.rs` | UARTE0 transport: read/write a protocol line, drop overlong lines. |
| `src/button.rs` | Debounced Button A via a `Debounce` state machine + embassy GPIO interrupt-free polling. |
| `src/display.rs` | 5×5 LED matrix task: shows Idle / Waiting (blink) / Success (tick) / Unprovisioned patterns. |
| `src/key.rs` | **UICR key store**: `load_key`, `provision` (write + readback verify), `enable_readback_protection` (`APPROTECT`), `reset_chip`. |
| `src/logger.rs` | defmt/RTT logger (`Debug/Info/Warn/Error` levels → probe-rs RTT pane or `probe-rs run`). |
| `tests/ondevice.rs` | `defmt-test` suites re-running the shared logic on the real board. |

### Firmware state machine (`main.rs`)

- **boot**: init embassy, UARTE0, display task, Button A, load key from UICR,
  send `HELLO`.
- **idle**: read a line → `PING`→`PONG`, `AUTH`→arm a session + `READY`,
  `BTN_TEST`→arm self-test, `PROV`/`LOCK`→key.rs operations.
- **armed**: block on *either* Button A debounced press *or* the window
  `Timer`. Press → `Session::step` emits `ArmedEvent::Press(p)` → send
  `AUTH_OK` (or `BTN_PRESSED`). Timeout → `AuthTimeout`/`BtnTestTimeout`.

---

## Host side — `host/microbit-authenticator/`

A std binary, invoked by `pam_exec` (or by hand). Exit codes are the PAM
contract: **0** allowed, **1** denied, **2** config/IO error.

| Piece | What it does |
|-------|--------------|
| `main()` | Parse CLI (`auth`/`test`/`btn-test`/`ping`/`keygen`/`provision`), dispatch, `process::exit(code)`. |
| `open_port()` / `discover_port()` | Find the board: `/dev/serial/by-id/*CMSIS-DAP*if01*`, else scan `/dev/ttyACM*`, else `--port`/`MICROBIT_PORT`. |
| `read_line`/`write_line`/`send_frame` | Raw byte plumbing; CR/LF line framing, timeout-scaled reads. |
| `load_host_key()` | Read the 32-byte secret from `/etc/microbit-auth/secret.key` (or `MICROBIT_KEY_FILE`). |
| `run_auth()` | The real flow: load key → open serial → send random `AUTH` nonce → parse replies → verify MAC → exit 0/1. Tests the human + board + key all at once. |
| `run_btn_test()` | `BTN_TEST` self-test: only a real click makes the device answer with `BTN_PRESSED`. |
| `run_ping()` | Connectivity probe (`PING`/`PONG`). |
| `run_keygen()` | Generate a random 32-byte secret, print it, optionally write mode-0600. |
| `run_provision()` | Two-phase flash: `PROV` key + magic (waits for `PROV_OK`) then `LOCK` (waits for `LOCK_OK`), enabling `APPROTECT`. |
| `tests/` | Host smoke tests + a shared-protocol test linking the `authenticate` lib. |

---

## Logging tour

Both sides now log what they are doing.

### Firmware — defmt over RTT

Levels `Debug`/`Info`/`Warn`/`Error` (added `Debug` in this change) via
`logger::flush(...)` in `src/12-authenticate/src/logger.rs`. Messages you'll
now see (example `probe-rs run` / VS Code RTT pane output):

```
INFO  serial: UARTE0 ...                 (main.rs banner)
INFO  uicr: key present (magic ok)       (key.rs)
DEBUG serial tx: HELLO AUTH/1.0 MAGIC:1  (serial.rs)
DEBUG serial rx: PING                    (serial.rs)
INFO  proto: PING -> PONG                (main.rs)
DEBUG serial tx: PONG
DEBUG serial rx: AUTH <nonce>            (serial.rs)
DEBUG display: pattern -> Waiting        (display.rs)
INFO  button: waiting for release...     (button.rs)
INFO  button: debounced press after ~340ms
INFO  proto: auth: button A pressed... -> AUTH_OK sent
DEBUG serial tx: AUTH_OK <mac>
```

Key places logging was added:
- `serial.rs` — every TX/RX line + overlong-line drops (the wire-level trace).
- `button.rs` — debounce transitions and the confirmed press with timing.
- `display.rs` — each display-pattern change.
- `key.rs` — UICR load/provision/readback/APPROTECT/reset.
- `logger.rs` — new `Debug` level.

To see these on your machine: flash/run with `probe-rs run --chip nRF52833_XXAA`
(alias in `.cargo/config.toml`), or open the probe-rs RTT pane in VS Code.
Note `Embed.toml` disables the `cargo embed` RTT decode step, so use
`probe-rs run`/the VS Code pane instead.

### Host — stderr, two tiers

In `host/microbit-authenticator/src/main.rs`:

- `info(...)` — **always printed** to stderr (lifecycle: start, key loaded, port
  opened, frames sent, and the final result). Never touches stdout, so it can't
  corrupt the PAM contract or the `keygen` output.
- `trace(cli, ...)` (was `log`) — **only with `-v/--verbose`**; adds timestamps
  and the juicy detail (nonce/MAC hex, expected-vs-received, parse noise).

Example success (default `auth`):

```
start: Auth (verbose=false)
key: reading /etc/microbit-auth/secret.key
key: loaded 32-byte host secret
opening /dev/ttyACM0 @ 115200 8N1
serial port open
tx: challenge AUTH sent
result: OK (MAC verified)          ← exit 0, PAM grants login
```

With `-v` you additionally get per-frame lines, nonce/MAC hex and the
constant-time comparison, e.g. `[1699999999.123] tx: AUTH <nonce>` and
`verify: expected=... received=... match=TRUE (constant-time)`.

---

## Typical workflows

**Provision a fresh board**

```text
sudo host/microbit-authenticator/target/release/microbit-authenticator keygen --out /etc/microbit-auth/secret.key
sudo host/microbit-authenticator/target/release/microbit-authenticator provision
```

**Check the link before trusting it**

```text
host/microbit-authenticator/target/release/microbit-authenticator ping
host/microbit-authenticator/target/release/microbit-authenticator btn-test
```

**Watch the whole exchange on the device**

```text
probe-rs run --chip nRF52833_XXAA --chip-description-path ...   # or VS Code RTT pane
```