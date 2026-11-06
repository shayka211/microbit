# Module 12 — micro:bit Cryptographic Challenge–Response Login (PLAN)

> Status: **IMPLEMENTED — milestones M0–M6 below.** The plan remains the design
> contract. Each of the three parts describes the *what*, *where*, *how*, the indicative logs
> at interesting points, and acceptance criteria.
>
> Target: micro:bit v2 (nRF52833), Embassy 0.5.x stack, host = CachyOS (Arch) + KDE Plasma/SDDM.

## Implementation status (M0–M6)

| Milestone | Deliverable | Status |
|---|---|---|
| M0 | Baseline builds | ✅ `cargo embed --features v2 --target thumbv7em-none-eabihf` builds; `microbit-authenticator --help` runs |
| M1 | lib/bin split + protocol core + supervisor | ✅ `src/lib.rs` (protocol/crypto/const/state) + `src/main.rs`; `cargo test --lib` green |
| M2 | display + button + §1.10 self-test (BTN_TEST) | ✅ `display.rs`, `button.rs`, `BTN_TEST` mode in supervisor; debounce/session unit-tested |
| M3 | crypto both layers + RFC 4231 vectors | ✅ vectors in lib tests, on-device `tests/ondevice.rs`, and host `tests/shared_protocol.rs` |
| M4 | UICR provision + APPROTECT + recovery runbook | ✅ `key.rs` (erase/provision/verify/APPROTECT/reset); runbook §8 – NOTE: run on-target tests BEFORE `LOCK` |
| M5 | Host CLI + PAM/udev deploy | ✅ `host/microbit-authenticator` (auth/test/btn-test/ping/keygen/provision) + `deploy/install.sh` |
| M6 | Hardening & docs | ✅ READMEs, security notes §9, idempotent installer, backups, `--uninstall` |

On-target suite: compile-verified for `thumbv7em-none-eabihf`; **run it while the board is
still unlocked**: `cargo test --target thumbv7em-none-eabihf --test ondevice`.

---

## 0. Outcome

"Either password **OR** micro:bit button" login on SDDM:

1. User locks the screen (`loginctl lock-session`) or reaches the SDDM greeter.
2. PAM runs our authenticator; the micro:bit turns its LED matrix on ("waiting" pattern).
3. Pressing **Button A** within the window answers a cryptographic challenge
   (HMAC-SHA256 over a fresh 32-byte nonce, keyed by a secret stored in the micro:bit's
   UICR and locked against readback). The host verifies and logs the user in instantly.
4. Ignoring the micro:bit falls through to the normal KDE password field.

---

## 1. Architecture overview

```
 ┌─────────────┐  115200 8N1, no flow control   ┌────────────────────────┐
 │  CachyOS    │ ─────────── USB /dev/ttyACM0 ─► │ interface MCU (KL27 or │
 │  SDDM/PAM    │      (CDC-ACM bridge)          │ nRF52820, DAPLink)     │
 │  ┌────────┐  │                                └───────────┬────────────┘
 │  │ pam_exec│  │                                            │ UART0 (P0.06 TX /
 │  └────┬───┘  │                                            │ P1.08 RX @ 115200)
 │  microbit-   │                                            ▼
 │  authenticator│                          ┌────────────────────────┐
 │  (host CLI)   │                          │ nRF52833 target MCU    │
 └──────┬───────┘                          │  firmware (Embassy)     │
        │ exit 0 = sufficient ──────────────│  UARTE0 + HMAC-SHA256   │
        └── /etc/pam.d/sddm + /etc/pam.d/kde│  UICR secret + Buttons  │
                                           └────────────────────────┘
```

**Three deliverables**

| Part | Component | Where it lives | Language |
|---|---|---|---|
| **P1** | Embedded firmware (serial challenge–response on the micro:bit) | `src/12-authenticate/` (workspace member `authenticate`) | Rust `no_std` + Embassy |
| **P2** | Host authenticator CLI | `host/microbit-authenticator/` (new workspace member) | Rust `std` |
| **P3** | PAM integration (SDDM + KDE lock screen) | `/etc/pam.d/*`, udev rule | Config |

### 1.1 Hardware reality check (read this first)

The original spec asked for `usb-device` + `usbd-serial` on the micro:bit so it would
enumerate as a USB CDC-ACM device. **This is not possible on a stock micro:bit v2:**
the nRF52833's USB pins are *not* wired to the USB connector. The connector is owned by a
second "interface MCU" (KL27Z on v2.00, nRF52820 on v2.2x) running DAPLink. DAPLink
exposes four USB functions — MSC (drag-and-drop), **CDC** (serial pass-through from the
*target* MCU's UART to the PC), HID/CMSIS-DAP, WebUSB.

Consequences used throughout this plan:

- From the host's point of view the micro:bit **already is** `/dev/ttyACM0` / a USB serial
  device. The firmware does not enumerate USB — it speaks the protocol on `UARTE0`
  (TX `P0.06`, RX `P1.08`, 115200 8N1), which DAPLink bridges to the CDC port.
  The user-visible behavior is identical to the spec's "USB Serial device".
- DAPLink connects TX/RX only → **no flow control**. At 115200 the micro:bit docs warn
  bytes *can be dropped*. Mitigations baked into the protocol (§3): short frames (≤ 96 B),
  line framing, host timeout/retry, and host-side input drain before each exchange.
- `usb-device`/`usbd-serial` **must not** be added to the firmware `Cargo.toml`.
  (They'd only be usable with a debugger wired to the nRF52833's USB pins — out of scope.)

### 1.2 Readback lock (UICR + APPROTECT)

- Secret key: 32 bytes stored in the **UICR** customer region at `0x10001080`
  (magic word `0xA17EE2B5` at `0x10001100`, flashed with the key).
- Firmware reads the key at runtime via volatile reads (the CPU can read UICR; only
  *external debug access* is what we block).
- **Readback protection**: set `UICR.APPROTECT` (`0x10001208`) to `0x00000000`
  ("hard" protection). The exact trigger value must be re-verified against the
  nRF52833 Product Specification §Debug before flashing (see Risk R3).
- What APPROTECT does: blocks SWD AHB-AP debug access → DAPLink drag-and-drop and
  `probe-rs` can no longer read/steal the key, and (after reset) refuse flashing until unlocked.
- **Unlock / recovery** (documented runbook in §8): full-chip erase via CTRL-AP
  (`probe-rs erase --chip nRF52833_xxAA`, `pyocd erase --chip`). Destroys key + firmware =
  "brick to plain micro:bit" by design, then re-provision. The CDC/UART bridge keeps working
  while protected (DAPLink interface MCU is unaffected).

---

## 2. Build & flashing (per user's setup)

Firmware — always from inside the chapter:

```text
cd ~/checkouts/microbit/src/12-authenticate
cargo embed --features v2 --target thumbv7em-none-eabihf
```

Logic tests — same directory, **normal `#[test]`s**, no board attached:

```text
cargo test --lib        # runs host unit tests (protocol, crypto, state machine, ...)
```

(`--lib` is required: `main.rs` is `#![no_main]`/`#![no_std]`, so the *binary* can never be
host-tested — only the library target can, and `cargo test` without `--lib` would try to
compile the bin into a test harness and fail. See §1.11.)

On-target tests — same directory, runs **on the micro:bit** via probe-rs + defmt-test
(§1.11b); board must be attached; replaces the flashed firmware until `cargo embed` again:

```text
cargo test --target thumbv7em-none-eabihf --test ondevice
```

(The long `--test ondevice` is deliberate: it compiles only the defmt-test target, so the
host-only bin and lib harness are never touched on this target.)

Host tool (from repo root, `--release`); the host crate is a std member, so no embedded target:

```text
cargo build -p microbit-authenticator --release
```

(Details + install steps in §6 and §7.)

---

## 3. Serial protocol v1 (shared contract)

Line-based ASCII, `<CR><LF>` terminator, max 96 bytes/line. No binary escapes.
All fields lowercase-hex where hex. Firmware ignores and logs any malformed line.

| Direction | Frame | When / Meaning |
|---|---|---|
| dev → host | `HELLO AUTH/1.0 MAGIC:<xx>` | On boot & after `LOCK`+reset. `xx` = `01` if key present, `00` if unprovisioned |
| host → dev | `PING` | Keep-alive / presence probe |
| dev → host | `PONG` | Reply to `PING` |
| host → dev | `AUTH <64-hex nonce>` | New challenge (only accepted while idle) |
| dev → host | `READY` | Challenge accepted, LED "waiting", arming button |
| dev → host | `AUTH_OK <64-hex mac>` | Button A pressed; MAC = HMAC-SHA256(key, nonce) |
| dev → host | `AUTH_TIMEOUT` | Auth window (default 5 s) elapsed, no press |
| dev → host | `AUTH_ERR <reason>` | e.g. `busy`, `unprovisioned` |
| host → dev | `PROV <64-hex key>` | Provisioning: store key (only while unprovisioned) |
| dev → host | `PROV_OK` / `PROV_ERR <reason>` | Result of key write + readback verification |
| host → dev | `LOCK` | Enable APPROTECT, `LOCK_OK`, reset |
| dev → host | `LOCK_OK` | Then a hard reset → fresh `HELLO` |
| host → dev | `BTN_TEST` | Arm the button self-test (accepted only while `Idle`) |
| dev → host | `BTN_TEST_READY` | Test armed; **silence until a press** (only a click makes the device write) |
| dev → host | `BTN_PRESSED <elapsed_ms>` | Debounced Button A press, milliseconds since arming |
| dev → host | `BTN_TEST_TIMEOUT` | Self-test window elapsed, no press |
| either | `ERR <reason>` | Generic protocol error (`badline`, `badlen`) |

Robustness rules (because of §1.1):

- Host never sends a new `AUTH` while the device is in `READY`; on ambiguity the host
  re-syncs by waiting for a `HELLO`/`IDLE`-state frame.
- Host drains its RX buffer before writing a frame, and re-reads the previous
  line-length worth of data before treating an answer as missing.
- One automatic retry with a **fresh nonce** on protocol error (never re-use a nonce).

---

## PART 1 — Embedded firmware (`src/12-authenticate`)

### 1.1 File layout (target)

One package, **two targets** (no second crate/project anywhere):

```text
src/12-authenticate/
├─ Cargo.toml        (+ deps, feature change)      # build via §2
├─ Embed.toml        (unchanged)
├─ memory.x          (unchanged)
├─ src/
│  ├─ lib.rs         (NEW) pure-logic library target  →  host `#[test]`s via `cargo test --lib`
│  │                 protocol codec, crypto, hex, key format, debounce + state machine
│  ├─ main.rs        (firmware binary) `#![no_main]` embassy supervisor; imports the lib
│  ├─ constants.rs   pins, timeouts, UICR addrs, protocol constants (shared, part of lib)
│  ├─ logger.rs      keep RTT/defmt logging; UART mirror disabled by default
│  ├─ protocol.rs    frame encode/decode + idempotent line state machine  (lib)
│  ├─ crypto.rs      HMAC-SHA256 wrapper, constant-time verify, hex codec   (lib)
│  ├─ uicr.rs        key load/provision/verify + APPROTECT (NVMC, raw pac)  (bin/emu)
│  ├─ display.rs     async 5×5 LED matrix multiplexer + frame patterns      (bin)
│  ├─ button.rs      debounced async wait on Button A (P0_14)  (+ lib: pure press filter)
├─ tests/
│  └─ ondevice.rs    (NEW) defmt-test integration suites that RUN ON THE MICRO:BIT (§1.11b)
└─ .cargo/config.toml  (+ probe-rs test runner for on-target `cargo test`, §1.11b)
```

Nothing outside `src/12-authenticate` is needed for P1 — the "projects" the plan adds are
one firmware package (lib+bin here) and the one host tool crate in §2.1.

### 1.2 `Cargo.toml` changes

- **Remove** `usb-device`, `usbd-serial` from any candidate plans — not added.
- **Add**: `hmac = "0.12"` (feature `mac`), `sha2 = "0.10"`, `heapless = "0.8"` (already in
  the workspace lockfile).
- **Feature change**: default stays `["v2"]`; drop `uart-log` from default (UARTE0 is owned
  by the protocol now). Logging goes to defmt/RTT via `probe-rs`; optional verbatim relay to
  the host as `LOG` frames is deferred (log after P1 core works).
- **Targets**: `src/lib.rs` (lib, auto-discovered) + `src/main.rs` (bin) in this same package.
  No `[[bin]]` overrides, no extra member crates. The lib must stay **hardware-free** (only
  `hmac`/`sha2`/`heapless` + `core`) so it compiles for both the host (tests) and the target
  (firmware); every embassy/BSP import stays in the bin targets → `src/main.rs`.
- **On-target test wiring**: dev-dependencies `defmt-test`, `defmt-rtt`, `panic-probe`
  (plus a `[[test]] name = "ondevice"` with `harness = false`), and a probe-rs runner entry in
  `.cargo/config.toml`. Details + the APPROTECT caveat in §1.11(b).

### 1.3 Task graph (Embassy)

```
main()
 └─ spawn ─ display::task(mux, chan<-State)     // 2 ms row scan, 5×5 bitmaps
                     └─ spawn
UARTE0 recv (interrupt + static buffers) ──► supervisor loop (async)
                     ▲  ▲
                     │  └─ button::wait_press(P0_14)     // async, 20 ms debounce
                     └─ crypto::hmac_sha256(key, nonce)
```

- One supervisor task in `main.rs` runs everything (simplest correct model for this small
  state machine). UART framing uses Embassy `uarte` with static TX/RX buffers (pattern the
  existing `uart_worker` in `logger.rs`). Display multiplexer is a separate spawned task
  (reference: `11-snake-game/src/display.rs` nonblocking display pattern, ported to embassy
  timers). Shared state: `embassy_sync::channel` for outbound frames and display state signals.

### 1.4 State machine (firmware)

`WaitingHello → Idle → Armed(WaitingButton) → Responding → Idle` plus a
`Provisioning` branch. Log every transition (see 1.8).

### 1.5 Key store & provisioning (`uicr.rs`)

- **Read**: volatile `core::ptr::read_volatile` on `0x10001080..+32`; verify magic word
  `0xA17EE2B5` at `0x10001100`. Missing/erased → `UNPROVISIONED`.
- **Write (provision)**: raw NVMC sequence via `pac` (safe-wrapped, data in a
  critical section):
  1. `NVMC.CONFIG.WEN = Write (0x01)`
  2. write 8 words to `0x10001080`, plus magic word at `0x10001100`
  3. `NVMC.CONFIG.WEN = Read (0x00)`; read-back compare.
  (UICR page erase, if ever needed for re-provision: `WEN=ErasePage`, address `0x10001000`.)
- **Lock**: write 0x00000000 to `UICR.APPROTECT` (`0x10001208`), then `cortex_m::peripheral::SCB::sys_reset()`.
- Boot behaviour: if `UNPROVISIONED` → display ✗/idle frame, accept only `PROV`/`PING`, log loudly.

### 1.6 Crypto (`crypto.rs`)

- `fn mac(key: &[u8;32], nonce: &[u8;32]) -> [u8;32]` wrapping `hmac::Hmac<Sha256>`.
- Constant-time compare via `hmac::Hmac::verify_slice` semantics on the host; firmware
  comparison uses the same helper for symmetry.
- Hex codec implemented locally (avoid pulling `hex` into `no_std` for one 64-byte string).
- Work pre-computed: single HMAC over the 32-byte nonce; megaproject, a few microseconds on
  the M4 @64 MHz — negligible inside the auth window.

### 1.7 Display (`display.rs`) & button (`button.rs`)

- Patterns: `Idle` (off), `Waiting` (blinking middle-row bar), `Success` (✓ 1 s),
  `Fail` (✗ 1 s), `Unprovisioned` (blinking corner dots). Frames are 5×5 bitmaps played by
  the multiplexer task; pins from the BSP `display_pins!` macro (cols `P0_28,P0_11,P0_31,
  P1_05,P0_30`; rows `P0_21,P0_22,P0_15,P0_24,P0_19`).
- Button: `button::wait_press()` polls `Input` on `P0_14` with 20 ms debounce,
  `Timer::after` between samples; only armed in `Armed` state.

### 1.8 Indicative logs (firmware) — the interesting points

Emit via existing `logger::flush(Level, args)` → defmt/RTT. Level choice shown per line.

| Point | Level | Indicative log line |
|---|---|---|
| Boot complete | Info | `--- microbit auth firmware v0.1.0 ---` |
| Key presence resolved | Info | `uicr: state=PROVISIONED magic=0xA17EE2B5 @0x10001080` / `…state=UNPROVISIONED` |
| Serial ready | Info | `serial: UARTE0 115200 8N1 (bridged to /dev/ttyACM*)` |
| Valid `AUTH` received | Info | `proto: challenge accepted nonce=<64hex>` |
| LED state change | Debug | `display: frame=Waiting blink=500ms` |
| Button press (armed) | Info | `button: A pressed (debounce 20ms) → AUTHORIZING` |
| HMAC computed | Debug | `crypto: mac=<64hex> computed in <µs>` |
| Response sent | Info | `proto: tx AUTH_OK <64hex>` |
| Timeout | Warn | `auth: window 5000ms elapsed, no press → AUTH_TIMEOUT` |
| Malformed frame | Warn | `proto: ERR badlen (78 bytes, line ignored)` |
| Unprovisioned boot | Error | `uicr: no key! only PROV/PING accepted` |
| Prov write result | Info | `uicr: 32B key written, readback match=ok magic=0xA17EE2B5` |
| Approval lock | Info | `uicr: APPROTECT=0x00000000 → READBACK LOCKED, resetting…` |
| Supervisor heartbeat | Debug | `auth: state=Armed elapsed_ms=1234` (only under verbose) |

### 1.9 Acceptance criteria (P1)

- `PING/PONG`, `AUTH→(READY→AUTH_OK|AUTH_TIMEOUT)` work over `/dev/ttyACM*`.
- Log lines above appear at the interesting points (RTT + verified at the terminal).
- Provisioning `PROV` writes key; readback verified; messing the magic word ⇒ `UNPROVISIONED`.
- `LOCK` resets with `HELLO ... MAGIC:01`; then `probe-rs`/drag-and-drop can no longer read
  flash (see §8 recovery before you try!).
- Button self-test §1.10 passes (device writes to UART **only after a physical press**).
- `cargo test --lib` runs the §1.11a logic suites green (protocol, crypto, debounce, state machine).
- On-target suite green: `cargo test --target thumbv7em-none-eabihf --test ondevice` (§1.11b, pre-LOCK).

### 1.10 Button self-test (test program)

Independent of the crypto flow, a way to prove that Button A + debounce work and that the
device writes to UART **only after a physical press**. Implemented *before* the auth/HMAC
work so wiring / pin / debounce problems are caught early. Reuses `button.rs` and the
display patterns (waiting = blinking bar, success = ✓).

**Primary flavor — command-driven test mode inside the auth firmware**

1. Host sends `BTN_TEST` (accepted only while the supervisor is `Idle`).
2. Device replies `BTN_TEST_READY`, lights the waiting pattern, and then writes **nothing**
   until a real click. This is the core assertion: no press ⇒ no output.
3. Debounced press (≥ 20 ms) ⇒ device writes exactly one frame `BTN_PRESSED <elapsed_ms>`
   and plays ✓ for 1 s before returning to `Idle`.
4. No press within the window (default 30 s) ⇒ `BTN_TEST_TIMEOUT`, back to `Idle`.
5. Each `BTN_TEST` re-arms it (one-shot per command); presses during the ✓ wait are ignored.

Indicative logs:

| Point | Level | Indicative log line |
|---|---|---|
| Test armed | Info | `btn-test: armed window=30000ms (silence until press)` |
| Press observed | Info | `btn-test: PRESS after 1243ms → tx BTN_PRESSED 1243` |
| Release ignored | Debug | `btn-test: release after 1452ms (no-op, only press counts)` |
| Timeout | Warn | `btn-test: timeout 30000ms, no press → BTN_TEST_TIMEOUT` |

Running it:

- Raw terminal check: one shell `stty -F /dev/ttyACM0 115200 && cat /dev/ttyACM0`, another
  `printf 'BTN_TEST\r\n' > /dev/ttyACM0`; `BTN_PRESSED <ms>` appears only after clicking A.
- Host subcommand (§2.2): `microbit-authenticator btn-test` — sends `BTN_TEST`, stays quiet,
  exits `0` with elapsed ms as soon as `BTN_PRESSED` arrives; `BTN_TEST_TIMEOUT`/lost port ⇒ exit `1`.

**No second project / no extra binary is needed.** The self-test is the `BTN_TEST` protocol
mode in the *same* firmware binary — one `cargo embed …` flashes both the auth flow and the
test mode. Only the physical press itself has to stay on hardware; everything around it becomes
a normal `#[test]` (§1.11). If a zero-host-input bench variant is ever wanted later, add a
`feature = "autotest"` that auto-arms `BTN_TEST` at boot in the same `main.rs` — still no new crate.

Acceptance (feeds M2):

- Zero UART output before the first click (only `BTN_TEST_READY` when armed).
- One click ⇒ exactly one `BTN_PRESSED <ms>` with `ms ≥ ~20` (debounce floor).
- Holding the button 5 s produces no extra/repeat frames.
- Timeout fires when nothing is pressed; re-arming works afterwards.
- `cargo test --lib` is green for the §1.11 logic tests (codec, debounce filter, state machine).

### 1.11 Testing strategy — same project, `#[test]`s at every layer

Three layers, one package, no new projects:

- **(a) Host logic tests** — plain `#[test]`s in the lib, instant, no board (isolate pure logic).
- **(b) On-target tests** — `defmt-test` + probe-rs, runs **on the micro:bit's real M4**;
  the integration layer that host tests can't provide.
- **(c) Hardware-in-the-loop button check** — the `BTN_TEST` HITL, where the host asserts the
  real serial path only emits bytes **after** a physical press.

**(a) Host logic tests — `cargo test --lib` (fast, no board, runs in CI/locally)**

The lib target (`protocol`, `crypto`, `hex`, key-format, debounce filter, state machine)
contains `#[cfg(test)] mod tests` with plain `#[test]` functions:

- **Crypto**: RFC 4231 HMAC-SHA256 test vectors computed with the on-board key format
  (empty vs 32-byte keys); ensures firmware and host crate (same crates) agree.
- **Protocol codec**: round-trip every frame of §3 (`encode_frame` → `parse_frame`),
  including `BTN_TEST`, `BTN_PRESSED <ms>`, `AUTH_OK <64hex>`; rejects `badlen`/garbage.
- **Hex codec**: `[u8;32]` ↔ 64-char hex, symmetry + uppercase-rejection.
- **Button logic**: feed the *pure* press filter a synthetic sample stream (high/low with
  jitter) and assert exactly one `PRESSEVENT` after a stable-low run — the debounce rule
  "≥ 20 ms stable ⇒ one event" is testable without a button.
- **State machine**: drive `Idle → Armed → (press-event|time tick) → …` with synthetic
  events and assert the exact outbound frames — including the crux of the user ask:
  **while `Armed`, any input frame other than reality yields no output** (the "silence
  until press" property), `release` alone produces nothing, and timeout emits
  `BTN_TEST_TIMEOUT` after `window_ms` ticks.
- **Key format**: magic-word check `0xA17EE2B5`, "erase = 0xFF" detection → `UNPROVISIONED`.

Command: `cargo test --lib` from `src/12-authenticate`. The bin is never touched.

**(b) On-target tests — `defmt-test` + probe-rs (runs on the real micro:bit)**

First-class, per the project direction. Tests compile to a small `no_std` binary that
probe-rs flashes to the chip; defmt-test runs the suite on the actual Cortex-M4 and streams
pass/fail back over RTT, printing a normal colored cargo test report. This validates the
*integrated* behavior that host tests cannot: real UARTE0 framing, real M4 cryptography,
real GPIO/UICR access.

Wiring:

```toml
# src/12-authenticate/.cargo/config.toml  (append to the existing file)
[target.thumbv7em-none-eabihf]
runner = "probe-rs run --chip nRF52833_xxAA"     # used only by cargo test with --target
```

```toml
# src/12-authenticate/Cargo.toml (append)
[dev-dependencies]
defmt-test = "0.4"                                        # pin per defmt-test README at impl time
defmt-rtt = "0.4"                                         # RTT/logging for the test harness
panic-probe = { version = "0.3", features = ["print-defmt"] }  # failing test → visible panic

[[test]]
name = "ondevice"
path = "tests/ondevice.rs"
harness = false      # defmt-test supplies its own harness/entry point
```

`tests/ondevice.rs` skeleton:

```rust
#![no_std]
#![no_main]

#[defmt_test::tests]
mod tests {
    use defmt::assert_eq;

    #[init]
    fn init() {
        // runs once before all tests (static buffers, optional peripheral setup)
    }

    #[test]
    fn hmac_rfc4231_vectors_on_device() {
        // HMAC-SHA256 computed on the real M4, compared against RFC 4231
    }

    #[test]
    fn uarte0_framing_integration() {
        // feed the SAME frame-parsing code the firmware uses over the real UARTE0
    }

    #[test]
    fn uicr_magic_word_unprovisioned() {
        // assert magic != 0xA17EE2B5 on a fresh UICR (run BEFORE provisioning/lock)
    }
}
```

Run it — board attached, replaces the flashed firmware until you `cargo embed` again:

```text
cd ~/checkouts/microbit/src/12-authenticate
cargo test --target thumbv7em-none-eabihf --test ondevice
```

Scope note: always `--lib` on the host or `--test ondevice` on the target. A bare
`cargo test` would try to build *all* test targets (the harness-less device binary fails on
the host, the `#![no_main]` bin fails everywhere) — see R9.

On-target suites for this project:

- HMAC-SHA256 RFC 4231 vectors computed on the real M4 (cross-check of §1.11a).
- UARTE0 integration: run exact frame parse/serialize logic against the real peripheral,
  asserting the same results as the host codec tests.
- Key-format / magic word on the actual UICR (fresh/unprovisioned, before §8 LOCK).
- Display multiplexer: drive a frame for one refresh tick and assert pin outputs via the
  `embedded-hal` traits the driver is written against.

**APPROTECT warning (critical):** on-target tests need the debugger to flash the chip. After
§8 locks the board with `APPROTECT`, `cargo test --target … --test ondevice` cannot flash any
more. Plan therefore: **run all on-target suites during M3–M4, before `LOCK`**; post-lock the
coverage shifts to §1.11a (host) + §1.11c (HITL), and recovery requires the §8 full-chip
erase (key destroyed, by design).

**(c) Hardware-in-the-loop button check — `BTN_TEST` mode + host `btn-test` subcommand**

The two assertions that need real silicon — and a human finger — scripted over the real
serial path:

1. Before the click: device writes nothing except `BTN_TEST_READY` (host asserts the port is
   silent for N seconds after arming).
2. After the click: exactly one `BTN_PRESSED <ms>` arrives with `ms ≥ 20`.

`microbit-authenticator btn-test` automates both (exits 0 only on (2), 1 on timeout/lost port)
— see §2.2. This is the *only* step that genuinely cannot be automated or mocked.

**(d) Optional host hardware mocks — `embedded-hal-mock` (stretch)**

If driver code (e.g. the display pattern player, the button read path) is written against
`embedded-hal` traits, `embedded-hal-mock` can script the exact expected pin transactions and
run them on the host. Nice-to-have; not required for M0–M6, and only pursued if host coverage
of the display/button drivers is wanted.

[Ferrous Systems]: https://github.com/ferrous-systems/defmt-test

---

## PART 2 — Host authenticator (`host/microbit-authenticator`)

### 2.1 Crate placement & workspace

- New directory `host/microbit-authenticator/`, plain std binary crate; add
  `"host/microbit-authenticator"` to the `members` list in root `Cargo.toml`.
- It is a std member, so it builds for the host by default; firmware members keep their
  explicit `--target thumbv7em-none-eabihf` invocations. Never built from the chapter dir's
  `.cargo/config.toml` context (that only affects the firmware crate).
- Dependencies: `serialport = "4"` (port open/baud/timeout), `hmac = "0.12"`, `sha2 = "0.10"`,
  `hex = "0.4"`, `rand = "0.8"` (`OsRng` for nonce + keygen), `clap = "4"` (subcommands),
  `anyhow` (error handling), `const-time compare` built into `hmac::mac::Mac::verify_slice`.
- **No** tokio/async — a PAM-spawned helper must start and quit in milliseconds.

### 2.2 CLI surface

```
microbit-authenticator [auth]        # default; PAM entry point (see §7)
  --port /dev/ttyACM0                # override port discovery (env MICROBIT_PORT too)
  --timeout 5                        # seconds to wait for button (default 5, max 15)
microbit-authenticator keygen        # print 64-hex random key to stdout
  --out /etc/microbit-auth/secret.key  # optional: write + chmod 600
microbit-authenticator provision     # upload key: PROV → verify → LOCK (run as root)
  --port /dev/ttyACM0 --key <64-hex>   # reads key from file or env MICROBIT_KEY
microbit-authenticator test          # interactive HITL check (LED + button + verify)
  --port /dev/ttyACM0 --timeout 10
microbit-authenticator btn-test      # §1.10 button self-test: BTN_TEST → wait → BTN_PRESSED
  --port /dev/ttyACM0 --timeout 30   #    exits 0 only when the device WROTE after a real press
microbit-authenticator ping          # reachability probe (HELLO/PONG exchange)
```

### 2.3 Port discovery (avoid hardcoding `/dev/ttyACM0`)

Priority: `MICROBIT_PORT` env → `--port` → glob `/dev/serial/by-id/*CMSIS-DAP*` matching
`if01` (DAPLink CDC interface; the MSC bulk interface is `if00`) → scan `/dev/ttyACM*`
and match VID/PID via `udevadm`. Log the chosen path. (Optional `udev` crate dep if glob
proves flaky on CachyOS; keep as stretch.)

### 2.4 Auth flow (PAM mode) — exit codes & logs

1. Open port @115200 8N1, no flow control; drain input.
2. Send `PING` (or wait for `HELLO`), confirm endpoint.
3. Generate `nonce = rand::thread_rng() (OsRng) 32 bytes`.
4. `tx AUTH <64hex>`; read lines ≤ ~10 s total budget, timeouts per line.
5. On `READY`: LED shown on device; wait for `AUTH_OK <mac>` / `AUTH_TIMEOUT` / `AUTH_ERR`.
6. Verify `mac == HMAC-SHA256(secret, nonce)` using host key file (default
   `/etc/microbit-auth/secret.key`, `root:root 0600`) with constant-time compare.
7. Exit `0` on match, else `1`; `2` for config problems (missing key file / bad flags).

Indicative logs (stderr; `--verbose` controls verbosity; PAM mode logs through
`log=` in §7 / journald):

| Point | Indicative log line |
|---|---|
| Startup | `microbit-authenticator v0.1.0 mode=auth pam=yes` |
| Port | `serial: opened /dev/ttyACM0 115200 8N1 (no flow control)` |
| Nonce | `nonce: 32B <64hex>` (truncate to 8 hex when not verbose) |
| TX challenge | `tx: AUTH <64hex>` |
| RX path | `rx: READY` |
| RX result | `rx: AUTH_OK <64hex>` / WARN `rx: AUTH_TIMEOUT (window 5000ms)` / `rx: AUTH_ERR <reason>` |
| BTN self-test | `rx: BTN_TEST_READY` → (silence while waiting) → `rx: BTN_PRESSED <ms> (button ok)` |
| Verify | `verify: expected=<64hex> received=<64hex> match=ok (constant-time)` |
| Outcome | `result: SUCCESS (exit 0)` / `result: FAILURE (exit <n>)` |

### 2.5 Acceptance criteria (P2)

- `ping`/`auth`/`test` work against flashed-P2 firmware with the button.
- Nonzero exit on: timeout, wrong key, missing port, garbage response.
- Round-trip verified against P1's `AUTH_OK` frames; MAC matches host computation.
- Never write the key into a log/argument (pass via env/file only).

---

## PART 3 — Linux PAM integration (SDDM + KDE lock screen)

### 3.1 PAM drop-ins

CachyOS/Arch PAM files live in `/etc/pam.d/`. Target **both** login and lock:

- Login/greeter: `/etc/pam.d/sddm`
- Lock screen (KDE Plasma `kscreenlocker`): `/etc/pam.d/kde` (cond: same service name
  in use — verify with `grep -r kscreenlocker /etc/pam.d`).

Insert as **first** line of the `auth` section, before the `system-auth` include:

```text
auth sufficient pam_exec.so quiet log=/var/log/microbit-auth.log /usr/local/bin/microbit-authenticator auth
```

- `sufficient`: success → login granted immediately, remaining `auth` rules skipped;
  failure/non-zero → fall through to password. Exactly "either/or".
- `quiet`: do not print module chatter to the greeter UI.
- Full path + static name → guards against PATH spoofing (pam_exec runs as root).
- Back up files before editing; pacman updates may replace them (`.pacnew`) — re-apply
  via a snippet/comment and the §3.4 test.

Optional hardening (verify not config-breaking on the user's KDE build):
`auth [success=1 default=ignore] pam_exec.so …` vs `sufficient` — keep `sufficient` per spec.

### 3.2 Install steps (P2 → P3)

```text
cargo build -p microbit-authenticator --release
sudo install -m 0755 target/release/microbit-authenticator /usr/local/bin/microbit-authenticator
sudo mkdir -p /etc/microbit-auth
sudo openssl rand -hex 32 | sudo tee /etc/microbit-auth/secret.key   # or: keygen --out
sudo chmod 600 /etc/microbit-auth/secret.key && sudo chown root:root /etc/microbit-auth/secret.key
```

### 3.3 udev rule — stable device name + permissions

`/etc/udev/rules.d/99-microbit-auth.rules`:

```text
SUBSYSTEM=="tty", ATTRS{idVendor}=="0d28", ATTRS{idProduct}=="0204", MODE="0660", GROUP="uucp", SYMLINK+="microbit-auth"
```

(Verify VID/PID + group `uucp` vs `dialout` on the live board via `lsusb` / `udevadm
info`. `cargo embed` flashing is unrelated to this rule.)

### 3.4 Verification procedure (end-to-end)

1. `sudo udevadm control --reload && sudo udevadm trigger`
2. `microbit-authenticator ping` (as user) → expect `HELLO`.
3. `microbit-authenticator test` → press Button A → `result: SUCCESS`.
4. `loginctl lock-session` → LED should light → press A → session unlocks instantly.
5. Repeat, *don't* press A → KDE password field appears; type password → unlock works
   (this proves the `sufficient` fall-through).
6. Reboot → SDDM greeter: same two outcomes.

### 3.5 Indicative logs (PAM layer)

| Point | Where |
|---|---|
| PAM stack invokes module | `auth/session` via `log=/var/log/microbit-auth.log`, journald |
| Module decision | exit code recorded by pam_exec (0/success vs 1/fail) |
| Greeter fall-through | user sees password field; syslog shows the `sufficient`-skip |

### 3.6 Acceptance criteria (P3)

- Lock + login behave per §3.4 on a clean reboot.
- Ignoring the micro:bit always yields the password path.
- Rollback documented: restore backup of the two PAM files → `systemctl daemon-reload` not
  needed for PAM.

---

## 8. Provisioning & recovery runbook

Performed once per board (and whenever the key MUST be rotated; rotation destroys the old
key and requires re-provision).

```text
# 1. Boot the board into unprovisioned state (fresh flash of P1 firmware)  → LED ✗/dots, "UNPROVISIONED" in logs
# 2. Generate the key on the host
sudo microbit-authenticator keygen --out /etc/microbit-auth/secret.key
# 3. Upload + readback-verify + set APPROTECT + auto-reset
sudo microbit-authenticator provision --port /dev/ttyACM0
#    logs: "uicr: 32B key written, readback match=ok", "APPROTECT=0x00000000 → READBACK LOCKED"
# 4. On reset expect: HELLO AUTH/1.0 MAGIC:01
# 5. End-to-end: microbit-authenticator test (button press required)
```

**Recovery after APPROTECT (key lost — by design):**

```text
# Full-chip erase via CTRL-AP (board flashes back to plain micro:bit):
probe-rs erase --chip nRF52833_xxAA            # or: pyocd erase --chip
cargo embed --features v2 --target thumbv7em-none-eabihf   # re-flash firmware
# then start at step 1 (new key will be needed; generate then provision)
```

Do this *once* on a disposable board before relying on it, so the unlock path is proven.
Note: with APPROTECT active, drag-and-drop flashing will fail and must go through the erase.

---

## 9. Security notes (document in README too)

- Symmetric secret: host root-level access = full impersonation (file 0600, root-owned;
  threat model is "non-root user can't log in without the hardware").
- Physical theft of the board = attacker holds key + button. Optional stretch: bind the
  host key to the specific board (store a hash of HELLO serial) to refuse clones.
- Replay prevented by a fresh nonce per attempt; constant-time MAC compare on both sides.
- Don't transmit the key after provisioning; don't log key material (env/file-only).
- APPROTECT readback-block applies after next reset; verify it actually took effect in
  provisioning logs before trusting the board in production use.

---

## 10. Milestones & acceptance

| M | Deliverable | Acceptance |
|---|---|---|
| M0 | Baseline builds (firmware via §2 command; host crate skeleton) | `cargo embed` flashes; `microbit-authenticator --help` runs |
| M1 | lib/bin split; protocol core (HELLO/PING/PONG + AUTH frame codec) + supervisor | `cargo embed` flashes; `cargo test --lib` green; loopback frames via terminal; log lines at §1.8 points |
| M2 | display.rs + button.rs + **button self-test §1.10** + §1.11 tests | §1.10 passes (device writes only after a real click), §1.11a logic tests green via `cargo test --lib`, **and** AUTH → waiting pattern → press → `AUTH_OK`/`AUTH_TIMEOUT` |
| M3 | crypto.rs both sides + RFC 4231 vectors: host `cargo test --lib` **and** on-target `cargo test --target thumbv7em-none-eabihf --test ondevice` | vectors pass both toolchains; defmt-test suite green on the M4 |
| M4 | uicr.rs: provision + APPROTECT + recovery runbook proven — run ALL remaining on-target suites BEFORE `LOCK` (post-lock they can't flash) | §8 steps end-to-end, incl. recovery on spare board |
| M5 | P2 CLI complete + P3 PAM/udev | §3.4 both outcomes on lock + greeter |
| M6 | Hardening & docs: security notes, README, rollback | M3–M5 green on clean config |

## 11. Risks & mitigations

| # | Risk | Mitigation |
|---|---|---|
| R1 | Byte drops on the DAPLink bridge at 115200 | Short frames, drain-before-write, single retry with fresh nonce (§3) |
| R2 | Dragging-and-dropping after APPROTECT fails mysteriously | Prominent README note; recovery runbook §8; "disposable board test" |
| R3 | APPROTECT trigger value varies by silicon revision | Re-verify vs nRF52833 PS; provisioning reads back + asserts before "LOCKED" |
| R4 | KDE lock screen uses a PAM service other than `kde` | Verify grep before editing; test §3.4 step 4 early |
| R5 | pacman clobbers PAM files on update | Backups + snippet approach; re-run §3.4 |
| R6 | Bad UX (greeter frozen while waiting) | Auth window 5 s default, max 15 s; documented `--timeout` |
| R7 | Host key stored on disk → root-only read needed | 0600 root; keygen/copy steps in §7 |
| R8 | Auth vs button-test could collide on the same UART | Supervisor is one state machine: `BTN_TEST` accepted only from `Idle`, `AUTH` rejected while test armed |
| R9 | Bare `cargo test` tries to build *all* targets: the `#![no_main]` bin fails everywhere and the harness-less device test fails on the host | Always scope: host `cargo test --lib`, target `cargo test --target thumbv7em-none-eabihf --test ondevice` (documented in §2); never bare `cargo test` |
| R10 | `APPROTECT` lock blocks on-target `cargo test`; the probe-rs runner entry could also confuse `cargo embed` | Run all defmt-test suites pre-LOCK (§1.11b); if `cargo embed` ever trips on the runner entry, pass it per-invocation with `cargo test --target thumbv7em-none-eabihf --test ondevice --config 'target.thumbv7em-none-eabihf.runner="probe-rs run --chip nRF52833_xxAA"'` and keep config.toml clean |

## 12. Open decisions (ask before implementing)

1. Confirm host key path `/etc/microbit-auth/secret.key` (vs e.g. `/etc/microbit.secret`).
2. Confirm default auth window **5 s** is acceptable on the lock screen.
3. Confirm whether to also touch `/etc/pam.d/kde` for the lock screen (depends on R4).
4. Preferred host serial port discovery: by-id glob vs udev crate.
5. After APPROTECT is locked, re-flashing requires the §8 erase — acceptable for this
   learning board (expected).