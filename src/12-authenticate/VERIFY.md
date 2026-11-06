# How to check everything works

## Quick answer

From `src/12-authenticate`:

```text
./verify.sh
```

That runs the full battery (build + unit tests + host tests + on-device
tests on your real micro:bit + a flash+boot check). Steps 4-5 need the board
plugged in.

## If you just want to see the firmware working live

```text
cargo embed --features v2 --target thumbv7em-none-eabihf
```

Run it from a **real terminal** (not here), keep it open, and you'll get a
live RTT console showing the defmt logs:

```
INFO  serial: UARTE0 ...
INFO  uicr: key present (magic ok)
DEBUG serial rx: PING
DEBUG display: pattern -> Waiting
...
```

Press the Host's button to arm a session, then press **Button A** on the
micro:bit and watch `button: debounced press` / `AUTH_OK`.

## What each thing does

| Thing | Where | What it proves |
|-------|-------|----------------|
| `cargo build --features v2 --target thumbv7em-none-eabihf` | firmware | compiles for the board |
| `cargo test --lib --no-default-features` | shared library | codec, HMAC vectors, debounce/state logic |
| `cargo test` (host dir) | host tool | host parses the same wire frames |
| `cargo test ... --test ondevice` | firmware tests | logic suites re-run on the REAL chip via SWD |
| `cargo embed`/`probe-rs run` | firmware | boots on the board, logs visible |

## Caveats

- A serial monitor/terminal holding `/dev/ttyACM0` (e.g. this env's `tabby`)
  blocks `probe-rs run`'s UART but not the on-device SWD tests — close other
  programs that touch the serial port before the live UART checks.
- `cargo embed` needs an interactive terminal for its RTT console.
