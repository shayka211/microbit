# How to check everything works

Nothing below touches PAM or `/etc` — it's all safe to run any time.

## Quick answer

From `src/12-authenticate`:

```text
./verify.sh                # everything automatic (~1 min, board plugged in)
./verify.sh --press        # ... plus "press button A" at the end
./verify.sh --no-hardware  # only what needs no board
```

| Check | What it proves | Board? |
|---|---|---|
| 1 | debug + release firmware compile | no |
| 2 | shared logic: protocol codec, HMAC, the device's reply rules, LED patterns/blinking | no |
| 3 | host tool end-to-end against a **fake board** that runs the firmware's own reply logic (good/bad key, no press, unprovisioned, silent board, debug noise, split lines, stale replies, provisioning/lock) | no |
| 4 | the logic suites again, compiled for and run **on the real chip** | yes |
| 5 | flash the debug firmware | yes |
| 6 | real serial: ping speed, junk lines ignored, 5 s auth window times out, board listens again right away | yes |
| 7 | (`--press`) a real button-A press reaches the host | yes + you |

## Flashing

```text
cargo embed --features v2 --target thumbv7em-none-eabihf   # as before (debug)
cargo embed-dev                                            # same thing, shorter
cargo embed-rel                                            # release: no debug logs, for daily use
```

## Seeing what the firmware does

The firmware logs over the **UART** (debug builds only), on the same line as
the protocol. Lines starting with `#` are logs; the host ignores them.

```text
cd ../../host/microbit-authenticator
cargo run -- monitor        # just watch (Ctrl-C to quit); reset/replug to see HELLO
cargo run -- -v ping        # run a command and show every line the board sends
```

Example (`-v auth`, nobody presses):

```
  < READY
  < #INF armed: Auth, waiting for button A (5000ms)
  < AUTH_TIMEOUT
result: FAILURE (button not pressed in time)
```

Only one program can read the port at a time — don't run `monitor` next to
another command.

## Testing the real login flow without touching the system

Use a throw-away key file and an **unlocked** board (it can be re-provisioned
any time):

```text
cd ../../host/microbit-authenticator
export MICROBIT_KEY_FILE=$HOME/microbit-dev.key
cargo run -- keygen --out $MICROBIT_KEY_FILE
cargo run -- provision --no-lock      # board stores the key, stays unlocked
cargo run -- test                     # press A → exit 0;  wait → exit 1
cargo run -- btn-test                 # just the button
```

`echo $?` after each command shows the exit code PAM would see
(0 = log in, 1 = fall back to password, 2 = setup problem).

## What the LEDs mean

| LEDs | Meaning |
|---|---|
| blinking corners | no key yet (`provision` needed) |
| off | ready, idle |
| blinking middle row | challenge received — press A now |
| tick (1 s) | press accepted, answer sent |
| cross (1 s) | window elapsed without a press |
