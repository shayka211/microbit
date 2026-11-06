//! `microbit-authenticator` — host side of the micro:bit challenge-response
//! login. Executed by PAM (`auth sufficient pam_exec.so ...`) or manually for
//! provisioning/testing.
//!
//! Exit codes (also the PAM contract):
//!   0  authentication succeeded (allowed by `pam_exec` enough → password skipped)
//!   1  authentication failed / timed out / wrong MAC
//!   2  configuration or I/O problem

use std::io::{self, Write};
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use authenticate::constants::MAX_FRAME_LEN;
use authenticate::crypto::verify;
use authenticate::protocol::Frame;
use clap::{Parser, Subcommand};
use serialport::SerialPort;

// ── CLI ────────────────────────────────────────────────────────────────────

#[derive(Debug, Parser)]
#[command(
    name = "microbit-authenticator",
    version,
    about = "micro:bit challenge-response authenticator (PAM auth helper)"
)]
struct Cli {
    /// Serial device (auto-detected via /dev/serial/by-id when omitted).
    #[arg(long, env = "MICROBIT_PORT", global = true)]
    port: Option<String>,

    /// Overall wait budget in milliseconds.
    #[arg(long, default_value_t = 5_000, global = true)]
    timeout_ms: u64,

    /// Verbose log lines on stderr.
    #[arg(long, short = 'v', global = true)]
    verbose: bool,

    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Debug, Clone, Subcommand)]
enum Cmd {
    /// Authenticate with the micro:bit — the PAM entry point (default).
    Auth,
    /// Interactive hardware-in-the-loop check (same as auth, more output).
    Test,
    /// Button self-test: exits 0 only if the device writes after a real click.
    BtnTest,
    /// PING/PONG reachability probe.
    Ping,
    /// Generate a fresh 64-hex 32-byte secret.
    Keygen {
        /// Also write it to this path (mode 0600).
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// Flash a secret into the micro:bit UICR, then enable readback lock.
    Provision {
        /// 64-hex key; defaults to the host key file.
        #[arg(long)]
        key: Option<String>,
    },
}

// ── plumbing ───────────────────────────────────────────────────────────────

fn main() {
    let cli = Cli::parse();
    let cmd = cli.cmd.clone().unwrap_or(Cmd::Auth);
    info(&format!("start: {cmd:?} (verbose={})", cli.verbose));
    let code = match cmd {
        Cmd::Auth => run_auth(&cli, false),
        Cmd::Test => run_auth(&cli, true),
        Cmd::BtnTest => run_btn_test(&cli),
        Cmd::Ping => run_ping(&cli),
        Cmd::Keygen { out } => run_keygen(out.as_deref()),
        Cmd::Provision { key } => run_provision(&cli, key.as_deref()),
    };
    std::process::exit(match code {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: {e:#}");
            2
        }
    });
}

fn log(cli: &Cli, msg: &str) {
    if cli.verbose {
        eprintln!("{msg}");
    }
}

/// Always-on lifecycle log line (stderr, so it never pollutes the PAM/exit contract).
fn info(msg: &str) {
    eprintln!("{msg}");
}

/// Verbose-only trace line, timestamped.
fn trace(cli: &Cli, msg: &str) {
    if cli.verbose {
        eprintln!("[{}] {msg}", timestamp());
    }
}

/// Monotonic-ish wall clock for trace lines.
fn timestamp() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let d = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    format!("{}.{:03}", d.as_secs(), d.subsec_millis())
}

fn read_line_timeout_ms(cli: &Cli) -> u64 {
    cli.timeout_ms.min(1_000).max(200)
}

fn open_port(cli: &Cli) -> Result<Box<dyn SerialPort>> {
    let path = discover_port(cli.port.as_deref())?;
    info(&format!("opening {path} @ 115200 8N1"));
    trace(cli, "serial: no flow control");
    let port = serialport::new(&path, 115_200)
        .timeout(Duration::from_millis(read_line_timeout_ms(cli)))
        .open()
        .with_context(|| format!("cannot open {path}"))?;
    info("serial port open");
    Ok(port)
}

fn discover_port(over: Option<&str>) -> Result<String> {
    if let Some(p) = over {
        return Ok(p.to_owned());
    }
    if let Ok(entries) = std::fs::read_dir("/dev/serial/by-id") {
        for e in entries.flatten() {
            let path = e.path();
            let name = path.to_string_lossy().to_string();
            if (name.contains("CMSIS-DAP") || name.contains("MBED")) && name.contains("if01") {
                return Ok(path.to_string_lossy().to_string());
            }
        }
    }
    for i in 0..8 {
        let p = format!("/dev/ttyACM{i}");
        if Path::new(&p).exists() {
            return Ok(p);
        }
    }
    bail!("no micro:bit serial device found (is it plugged in?)")
}

fn write_line(port: &mut dyn SerialPort, line: &[u8]) -> io::Result<()> {
    port.write_all(line)?;
    port.write_all(b"\r\n")?;
    port.flush()
}

fn read_line(port: &mut dyn SerialPort) -> io::Result<Vec<u8>> {
    let mut line = Vec::with_capacity(MAX_FRAME_LEN);
    let mut b = [0u8; 1];
    loop {
        match port.read(&mut b) {
            Ok(0) => return Err(io::Error::new(io::ErrorKind::TimedOut, "no data")),
            Ok(_) => {
                if b[0] == b'\n' {
                    break;
                }
                line.push(b[0]);
                if line.len() > MAX_FRAME_LEN {
                    return Err(io::Error::new(io::ErrorKind::InvalidData, "line too long"));
                }
            }
            Err(ref e) if e.kind() == io::ErrorKind::TimedOut => {
                return Err(io::Error::new(io::ErrorKind::TimedOut, "no data"));
            }
            Err(e) => return Err(e),
        }
    }
    if line.last() == Some(&b'\r') {
        line.pop();
    }
    Ok(line)
}

fn drain_input(port: &mut dyn SerialPort) {
    std::thread::sleep(Duration::from_millis(200));
    let mut d = [0u8; 128];
    while let Ok(n) = port.read(&mut d) {
        if n == 0 {
            break;
        }
    }
}

fn send_frame(port: &mut dyn SerialPort, frame: &Frame) -> Result<()> {
    let mut buf = [0u8; MAX_FRAME_LEN];
    let n = frame.encode(&mut buf).map_err(|_| anyhow!("frame too long"))?;
    write_line(port, &buf[..n])?;
    Ok(())
}

fn host_key_path() -> PathBuf {
    std::env::var_os("MICROBIT_KEY_FILE")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/etc/microbit-auth/secret.key"))
}

fn load_host_key() -> Result<[u8; 32]> {
    let path = host_key_path();
    info(&format!("key: reading {}", path.display()));
    let raw = std::fs::read_to_string(&path)
        .with_context(|| format!("cannot read key file {}", path.display()))?;
    let decoded = hex::decode(raw.trim()).with_context(|| "key file is not hex")?;
    let key: [u8; 32] = decoded
        .try_into()
        .map_err(|_| anyhow!("key must be exactly 32 bytes (64 hex chars)"))?;
    info("key: loaded 32-byte host secret");
    Ok(key)
}

// ── subcommands ────────────────────────────────────────────────────────────

fn run_auth(cli: &Cli, interactive: bool) -> Result<i32> {
    let key = load_host_key()?;
    let mut port = open_port(cli)?;
    drain_input(&mut *port);

    let nonce: [u8; 32] = rand::random();
    trace(cli, &format!("nonce: 32B {}", hex::encode(nonce)));
    send_frame(&mut *port, &Frame::Auth(nonce))?;
    trace(cli, &format!("tx: AUTH {}", hex::encode(nonce)));
    info("tx: challenge AUTH sent");

    let deadline = Instant::now() + Duration::from_millis(cli.timeout_ms);
    loop {
        if Instant::now() >= deadline {
            info("result: FAILURE (timeout)");
            return Ok(1);
        }
        let line = match read_line(&mut *port) {
            Ok(l) => l,
            Err(e) if e.kind() == io::ErrorKind::TimedOut => continue,
            Err(e) => return Err(e.into()),
        };
        match Frame::parse(&line) {
            Ok(Frame::Ready) => {
                trace(cli, "rx: READY (challenge accepted, awaiting button A)");
            }
            Ok(Frame::AuthOk(mac)) => {
                let ok = verify(&mac, &key, &nonce);
                log(
                    cli,
                    &format!(
                        "verify: expected={} received={} match={}",
                        hex::encode(verify_expected(&key, &nonce)),
                        hex::encode(mac),
                        if ok { "TRUE (constant-time)" } else { "FALSE" }
                    ),
                );
                if ok {
                    info("result: OK (MAC verified)");
                } else {
                    info("result: FAILURE (MAC mismatch)");
                }
                if interactive {
                    eprintln!("authenticated via micro:bit ✓");
                }
                return Ok(if ok { 0 } else { 1 });
            }
            Ok(Frame::AuthTimeout) => {
                trace(cli, "rx: AUTH_TIMEOUT (button not pressed in time)");
                info("result: FAILURE (button timeout)");
                return Ok(1);
            }
            Ok(Frame::AuthErr(kind)) => {
                trace(cli, &format!("rx: AUTH_ERR {}", kind.as_str()));
                info(&format!("result: FAILURE (device: {})", kind.as_str()));
                return Ok(1);
            }
            Ok(_) => trace(cli, "rx: unrelated frame ignored"),
            Err(e) => trace(cli, &format!("rx: parse error {e:?} (ignored)")),
        }
    }
}

fn verify_expected(key: &[u8; 32], nonce: &[u8; 32]) -> [u8; 32] {
    authenticate::crypto::sign(key, nonce)
}

fn run_btn_test(cli: &Cli) -> Result<i32> {
    let mut port = open_port(cli)?;
    drain_input(&mut *port);
    send_frame(&mut *port, &Frame::BtnTest)?;
    trace(cli, "tx: BTN_TEST");
    info("tx: BTN_TEST (press Button A when the display blinks)");

    let deadline = Instant::now() + Duration::from_millis(cli.timeout_ms);
    loop {
        if Instant::now() >= deadline {
            trace(cli, "result: FAILURE (no BTN_PRESSED within timeout)");
            return Ok(1);
        }
        let line = match read_line(&mut *port) {
            Ok(l) => l,
            Err(e) if e.kind() == io::ErrorKind::TimedOut => continue,
            Err(e) => return Err(e.into()),
        };
        match Frame::parse(&line) {
            Ok(Frame::BtnTestReady) => trace(cli, "rx: BTN_TEST_READY (device silent until a real click)"),
            Ok(Frame::BtnPressed(ms)) => {
                trace(cli, &format!("rx: BTN_PRESSED {ms}ms (elapsed since arming)"));
                info("result: OK (real button press observed)");
                return Ok(0);
            }
            Ok(Frame::BtnTestTimeout) => {
                trace(cli, "rx: BTN_TEST_TIMEOUT (no press in window)");
                info("result: FAILURE (device timed out waiting for press)");
                return Ok(1);
            }
            Ok(_) => trace(cli, "rx: unrelated frame ignored"),
            Err(e) => trace(cli, &format!("rx: parse error {e:?} (ignored)")),
        }
    }
}

fn run_ping(cli: &Cli) -> Result<i32> {
    let mut port = open_port(cli)?;
    drain_input(&mut *port);
    send_frame(&mut *port, &Frame::Ping)?;
    trace(cli, "tx: PING");
    info("tx: PING");

    let deadline = Instant::now() + Duration::from_millis(cli.timeout_ms);
    loop {
        if Instant::now() >= deadline {
            trace(cli, "result: FAILURE (no PONG)");
            return Ok(1);
        }
        let line = match read_line(&mut *port) {
            Ok(l) => l,
            Err(e) if e.kind() == io::ErrorKind::TimedOut => continue,
            Err(e) => return Err(e.into()),
        };
        match Frame::parse(&line) {
            Ok(Frame::Pong) => {
                trace(cli, "rx: PONG");
                info("result: OK (device answered PONG)");
                return Ok(0);
            }
            Ok(_) => trace(cli, "rx: unrelated frame ignored"),
            Err(e) => trace(cli, &format!("rx: parse error {e:?} (ignored)")),
        }
    }
}

fn run_keygen(out: Option<&Path>) -> Result<i32> {
    let key: [u8; 32] = rand::random();
    let hexs = hex::encode(key);
    println!("{hexs}");
    if let Some(p) = out {
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(p)
            .with_context(|| format!("cannot write {}", p.display()))?;
        f.write_all(format!("{hexs}\n").as_bytes())?;
        f.sync_all()?;
        let _ = std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o600));
        info(&format!("keygen: wrote 32-byte key to {} (mode 0600)", p.display()));
    }
    Ok(0)
}

fn run_provision(cli: &Cli, key_arg: Option<&str>) -> Result<i32> {
    let key: [u8; 32] = match key_arg {
        Some(h) => {
            let d = hex::decode(h).map_err(|_| anyhow!("--key is not valid hex"))?;
            d.try_into().map_err(|_| anyhow!("--key must be 32 bytes (64 hex chars)"))?
        }
        None => load_host_key()?,
    };
    info("provision: using 32-byte key");

    let mut port = open_port(cli)?;
    drain_input(&mut *port);

    // 1) PROV → expect PROV_OK
    send_frame(&mut *port, &Frame::Prov(key))?;
    trace(cli, &format!("tx: PROV {}", hex::encode(key)));
    info("provision: tx PROV (flash key + magic)");
    let deadline = Instant::now() + Duration::from_millis(cli.timeout_ms);
    let mut provisioned = false;
    while !provisioned {
        if Instant::now() >= deadline {
            bail!("timeout waiting for PROV_OK");
        }
        let line = match read_line(&mut *port) {
            Ok(l) => l,
            Err(e) if e.kind() == io::ErrorKind::TimedOut => continue,
            Err(e) => return Err(e.into()),
        };
        match Frame::parse(&line) {
            Ok(Frame::ProvOk) => {
                trace(cli, "rx: PROV_OK (key written, readback verified)");
                info("provision: PROV_OK (key flashed + verified)");
                provisioned = true;
            }
            Ok(Frame::ProvErr(kind)) => {
                trace(cli, &format!("rx: PROV_ERR {}", kind.as_str()));
                info(&format!("provision: FAILURE (device: {})", kind.as_str()));
                return Ok(1);
            }
            Ok(_) => {}
            Err(e) => trace(cli, &format!("rx: parse error {e:?} (ignored)")),
        }
    }

    // 2) LOCK → expect LOCK_OK (device then resets with HELLO)
    send_frame(&mut *port, &Frame::Lock)?;
    trace(cli, "tx: LOCK");
    info("provision: tx LOCK (enable APPROTECT readback protection)");
    let deadline = Instant::now() + Duration::from_millis(cli.timeout_ms);
    loop {
        let line = match read_line(&mut *port) {
            Ok(l) => l,
            Err(e) if e.kind() == io::ErrorKind::TimedOut => {
                if Instant::now() >= deadline {
                    bail!("timeout waiting for LOCK_OK");
                }
                continue;
            }
            Err(e) => return Err(e.into()),
        };
        match Frame::parse(&line) {
            Ok(Frame::LockOk) => {
                trace(cli, "rx: LOCK_OK → readback protection enabled (reset after)");
                info("provision: LOCK_OK (readback protection enabled)");
                return Ok(0);
            }
            Ok(_) => {}
            Err(e) => trace(cli, &format!("rx: parse error {e:?} (ignored)")),
        }
    }
}