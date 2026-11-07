//! Finding and opening the micro:bit's serial port.

use std::path::Path;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use authenticate::constants::UART_BAUD;
use serialport::SerialPort;

/// Symlink created by `deploy/99-microbit-auth.rules`.
pub const UDEV_SYMLINK: &str = "/dev/microbit-auth";

/// How long one `read()` may block. Short, so deadlines are honoured promptly;
/// `Link` keeps partial lines across timeouts.
const READ_TIMEOUT: Duration = Duration::from_millis(100);

/// Pick the port: explicit override, then the udev symlink, then the DAPLink
/// CDC interface in `/dev/serial/by-id`.
///
/// There is deliberately no "first /dev/ttyACM*" fallback: under PAM we must
/// not send challenges to whatever unrelated serial device happens to exist.
pub fn discover(over: Option<&str>) -> Result<String> {
    if let Some(p) = over {
        return Ok(p.to_owned());
    }
    if Path::new(UDEV_SYMLINK).exists() {
        return Ok(UDEV_SYMLINK.to_owned());
    }
    if let Ok(entries) = std::fs::read_dir("/dev/serial/by-id") {
        let mut found: Vec<String> = entries
            .flatten()
            .map(|e| e.path().to_string_lossy().into_owned())
            .filter(|n| {
                (n.contains("micro:bit") || n.contains("CMSIS-DAP") || n.contains("MBED"))
                    && n.contains("if01")
            })
            .collect();
        found.sort();
        if let Some(p) = found.into_iter().next() {
            return Ok(p);
        }
    }
    bail!("no micro:bit serial device found (is it plugged in? or pass --port)")
}

pub fn open(path: &str) -> Result<Box<dyn SerialPort>> {
    let port = serialport::new(path, UART_BAUD)
        .timeout(READ_TIMEOUT)
        .open()
        .with_context(|| format!("cannot open {path}"))?;
    Ok(port)
}
