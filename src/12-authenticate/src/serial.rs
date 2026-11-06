//! UARTE0 serial transport on the DAPLink bridge (P0.06 TX / P1.08 RX, 115200).

use crate::logger::{flush, Level};
use authenticate::constants::MAX_FRAME_LEN;
use embassy_nrf::peripherals::UARTE0;
use embassy_nrf::uarte::{Uarte, UarteRx, UarteTx};

/// Render a protocol line byte-slice as a printable ASCII string for logging.
/// Non-printable bytes become `·`.
fn render_line(line: &[u8]) -> heapless::String<MAX_FRAME_LEN> {
    let mut s = heapless::String::new();
    for &b in line {
        let c = if b.is_ascii_graphic() || b == b' ' {
            b as char
        } else {
            '·'
        };
        let _ = s.push(c);
    }
    s
}

pub struct Serial {
    tx: UarteTx<'static, UARTE0>,
    rx: UarteRx<'static, UARTE0>,
    line: [u8; MAX_FRAME_LEN],
    len: usize,
}

impl Serial {
    pub fn new(uarte: Uarte<'static, UARTE0>) -> Self {
        let (tx, rx) = uarte.split();
        Self {
            tx,
            rx,
            line: [0u8; MAX_FRAME_LEN],
            len: 0,
        }
    }

    /// Write one protocol line (data + `\r\n`).
    pub async fn write_line(&mut self, data: &[u8]) {
        flush(
            Level::Debug,
            format_args!("serial tx: {}", render_line(data)),
        );
        let _ = self.tx.write(data).await;
        let _ = self.tx.write(b"\r\n").await;
    }

    /// Read one CR/LF-terminated line. Overlong lines are dropped wholesale.
    pub async fn read_line(&mut self) -> Result<&[u8], ()> {
        self.len = 0;
        loop {
            let mut byte = [0u8; 1];
            self.rx.read(&mut byte).await.map_err(|_| ())?;
            match byte[0] {
                b'\n' => {
                    if self.len == 0 {
                        continue; // stray bare LF
                    }
                    if self.line[self.len - 1] == b'\r' {
                        self.len -= 1;
                    }
                    flush(
                        Level::Debug,
                        format_args!(
                            "serial rx: {}",
                            render_line(&self.line[..self.len])
                        ),
                    );
                    return Ok(&self.line[..self.len]);
                }
                b'\r' => {} // dropped
                b if self.len < self.line.len() => {
                    self.line[self.len] = b;
                    self.len += 1;
                }
                _ => {
                    flush(Level::Warn, format_args!("serial rx: line overflow, dropping"));
                    self.len = 0; // overflow: drop the whole line
                }
            }
        }
    }
}