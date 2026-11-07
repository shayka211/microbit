//! UARTE0 serial *reader* — owns the RX half only.
//! TX is owned by the logger task (`logger::task`).

use authenticate::constants::MAX_FRAME_LEN;
use embassy_nrf::peripherals::UARTE0;
use embassy_nrf::uarte::UarteRx;

pub struct Serial {
    rx: UarteRx<'static, UARTE0>,
    line: [u8; MAX_FRAME_LEN],
    len: usize,
}

impl Serial {
    pub fn new(rx: UarteRx<'static, UARTE0>) -> Self {
        Self {
            rx,
            line: [0u8; MAX_FRAME_LEN],
            len: 0,
        }
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
                    return Ok(&self.line[..self.len]);
                }
                b'\r' => {} // dropped
                b if self.len < self.line.len() => {
                    self.line[self.len] = b;
                    self.len += 1;
                }
                _ => {
                    self.len = 0; // overflow: drop the whole line
                }
            }
        }
    }
}