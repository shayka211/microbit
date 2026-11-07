//! `microbit-authenticator` — host side of the micro:bit challenge-response
//! login. Executed by PAM (`auth sufficient pam_exec.so ...`) or manually for
//! provisioning/testing.
//!
//! Exit codes (also the PAM contract):
//!   0  authentication succeeded (allowed by `pam_exec` enough → password skipped)
//!   1  authentication failed / timed out / wrong MAC
//!   2  configuration or I/O problem (no board, no key file, ...)
//!
//! Everything goes to stderr; stdout is only used for `keygen` output.

use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{bail, Result};
use authenticate::constants::{AUTH_WINDOW_MS, BTN_TEST_WINDOW_MS};
use authenticate::protocol::{AuthErrKind, Key, ProvErrKind};
use clap::{Parser, Subcommand};
use microbit_authenticator::keyfile;
use microbit_authenticator::link::{self, AuthOutcome, BtnOutcome, Link, ProvOutcome};
use microbit_authenticator::port;
use serialport::SerialPort;

/// Extra time on top of the board's own window, so the board's verdict
/// (press or AUTH_TIMEOUT) always arrives before we give up.
const MARGIN_MS: u64 = 2_000;
/// Default for quick request/reply commands.
const QUICK_MS: u64 = 3_000;

// ── CLI ────────────────────────────────────────────────────────────────────

#[derive(Debug, Parser)]
#[command(
    name = "microbit-authenticator",
    version,
    about = "micro:bit challenge-response authenticator (PAM auth helper)"
)]
struct Cli {
    /// Serial device (default: /dev/microbit-auth, then /dev/serial/by-id).
    #[arg(long, env = "MICROBIT_PORT", global = true)]
    port: Option<String>,

    /// How long to wait for the board, in ms (default depends on the command).
    #[arg(long, global = true)]
    timeout_ms: Option<u64>,

    /// Show every line from the board (incl. firmware debug logs) on stderr.
    #[arg(long, short = 'v', global = true)]
    verbose: bool,

    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Debug, Clone, Subcommand)]
enum Cmd {
    /// Authenticate with the micro:bit — the PAM entry point (default).
    Auth,
    /// Same as `auth`, with human-friendly prompts.
    Test,
    /// Button self-test: exits 0 only after a real press of button A.
    BtnTest,
    /// PING/PONG reachability probe.
    Ping,
    /// Print everything the board sends (frames + debug logs) until Ctrl-C.
    ///
    /// Don't run it next to another command: both would read the same port
    /// and steal each other's lines. Use `-v` on that command instead.
    Monitor,
    /// Generate a fresh 64-hex 32-byte secret.
    Keygen {
        /// Also write it to this path (created with mode 0600).
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// Write a secret into the micro:bit UICR, then (by default) lock readback.
    Provision {
        /// 64-hex key; defaults to the host key file.
        #[arg(long)]
        key: Option<String>,
        /// Skip the readback lock. The board can then be re-provisioned over
        /// serial — use this while developing.
        #[arg(long)]
        no_lock: bool,
    },
}

impl Cmd {
    fn default_timeout_ms(&self) -> u64 {
        match self {
            Cmd::Auth | Cmd::Test => AUTH_WINDOW_MS as u64 + MARGIN_MS,
            Cmd::BtnTest => BTN_TEST_WINDOW_MS as u64 + MARGIN_MS,
            _ => QUICK_MS,
        }
    }
}

struct Ctx {
    port: Option<String>,
    timeout: Duration,
    verbose: bool,
}

fn main() {
    let cli = Cli::parse();
    let cmd = cli.cmd.clone().unwrap_or(Cmd::Auth);
    let ctx = Ctx {
        timeout: Duration::from_millis(cli.timeout_ms.unwrap_or_else(|| cmd.default_timeout_ms())),
        port: cli.port,
        verbose: cli.verbose,
    };
    info(&format!("start: {cmd:?}"));
    let code = match cmd {
        Cmd::Auth => run_auth(&ctx, false),
        Cmd::Test => run_auth(&ctx, true),
        Cmd::BtnTest => run_btn_test(&ctx),
        Cmd::Ping => run_ping(&ctx),
        Cmd::Monitor => run_monitor(&ctx),
        Cmd::Keygen { out } => run_keygen(out),
        Cmd::Provision { key, no_lock } => run_provision(&ctx, key.as_deref(), !no_lock),
    };
    std::process::exit(match code {
        Ok(c) => c,
        Err(e) => {
            info(&format!("error: {e:#}"));
            2
        }
    });
}

/// Timestamped lifecycle line on stderr (ends up in the pam_exec log).
fn info(msg: &str) {
    let d = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    eprintln!("[{}.{:03}] {msg}", d.as_secs(), d.subsec_millis());
}

fn connect(ctx: &Ctx) -> Result<Link<Box<dyn SerialPort>>> {
    let path = port::discover(ctx.port.as_deref())?;
    info(&format!("serial: {path}"));
    let mut link = Link::new(port::open(&path)?).with_echo(ctx.verbose);
    link.drain()?;
    Ok(link)
}

fn load_host_key() -> Result<Key> {
    let path = keyfile::path();
    let (key, warning) = keyfile::load(&path)?;
    if let Some(w) = warning {
        info(&format!("warning: {w}"));
    }
    Ok(key)
}

// ── subcommands ────────────────────────────────────────────────────────────

fn run_auth(ctx: &Ctx, interactive: bool) -> Result<i32> {
    let key = load_host_key()?;
    let mut link = connect(ctx)?;
    let nonce: [u8; 32] = rand::random();
    if interactive {
        eprintln!("→ press button A on the micro:bit (middle row blinks)…");
    }
    let outcome = link::authenticate(&mut link, &key, &nonce, ctx.timeout)?;
    let (code, msg) = match outcome {
        AuthOutcome::Ok => (0, "OK (button pressed, MAC verified)"),
        AuthOutcome::BadMac => (1, "FAILURE (MAC mismatch: board and host keys differ)"),
        AuthOutcome::ButtonTimeout => (1, "FAILURE (button not pressed in time)"),
        AuthOutcome::Refused(AuthErrKind::Unprovisioned) => {
            (1, "FAILURE (board has no key: run `provision`)")
        }
        AuthOutcome::Refused(AuthErrKind::Busy) => (1, "FAILURE (board busy)"),
        AuthOutcome::NoAnswer => (1, "FAILURE (no answer from board)"),
    };
    info(&format!("result: {msg}"));
    if interactive {
        eprintln!(
            "{}",
            if code == 0 {
                "authenticated via micro:bit ✓"
            } else {
                "not authenticated ✗"
            }
        );
    }
    Ok(code)
}

fn run_btn_test(ctx: &Ctx) -> Result<i32> {
    let mut link = connect(ctx)?;
    eprintln!("→ press button A on the micro:bit (middle row blinks)…");
    let (code, msg) = match link::btn_test(&mut link, ctx.timeout)? {
        BtnOutcome::Pressed(ms) => (0, format!("OK (press after {ms} ms)")),
        BtnOutcome::DeviceTimeout => (1, "FAILURE (board timed out waiting for a press)".into()),
        BtnOutcome::NoAnswer => (1, "FAILURE (no answer from board)".into()),
    };
    info(&format!("result: {msg}"));
    Ok(code)
}

fn run_ping(ctx: &Ctx) -> Result<i32> {
    let mut link = connect(ctx)?;
    let ok = link::ping(&mut link, ctx.timeout)?;
    info(if ok {
        "result: OK (PONG)"
    } else {
        "result: FAILURE (no PONG)"
    });
    Ok(if ok { 0 } else { 1 })
}

fn run_monitor(ctx: &Ctx) -> Result<i32> {
    let path = port::discover(ctx.port.as_deref())?;
    info(&format!(
        "monitor: {path} (Ctrl-C to quit; replug/reset the board to see HELLO)"
    ));
    let mut link = Link::new(port::open(&path)?);
    loop {
        if let Some(line) = link.poll_line()? {
            info(&String::from_utf8_lossy(&line));
        }
    }
}

fn run_keygen(out: Option<PathBuf>) -> Result<i32> {
    let key: Key = rand::random();
    println!("{}", hex::encode(key));
    if let Some(p) = out {
        keyfile::save(&p, &key)?;
        info(&format!("keygen: wrote key to {} (mode 600)", p.display()));
    }
    Ok(0)
}

fn run_provision(ctx: &Ctx, key_arg: Option<&str>, lock: bool) -> Result<i32> {
    let key = match key_arg {
        Some(h) => keyfile::parse_hex(h)?,
        None => load_host_key()?,
    };
    let mut link = connect(ctx)?;

    match link::provision(&mut link, &key, ctx.timeout)? {
        ProvOutcome::Ok => info("provision: PROV_OK (key written and read back)"),
        ProvOutcome::Refused(ProvErrKind::Already) => bail!(
            "board is readback-locked and refuses a new key; \
             full chip erase + reflash needed (probe-rs erase --chip nRF52833_xxAA --allow-erase-all)"
        ),
        ProvOutcome::Refused(kind) => bail!("board refused the key: {}", kind.as_str()),
        ProvOutcome::NoAnswer => bail!("no answer to PROV"),
    }

    if !lock {
        info("provision: done (not locked — re-provisioning stays possible)");
        return Ok(0);
    }
    if !link::lock(&mut link, ctx.timeout)? {
        bail!("no answer to LOCK");
    }
    info("provision: LOCK_OK (readback protection on; board resets)");
    Ok(0)
}
