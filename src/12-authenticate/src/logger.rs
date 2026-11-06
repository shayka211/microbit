//! defmt/RTT logger.
//!
//! The old UART-mirror feature was removed on purpose: `UARTE0` is owned by the
//! auth protocol, so logs go exclusively to the probe-rs RTT pane (or are read
//! with `probe-rs run`).

use core::fmt::Write; // brings `write_fmt` into scope for our Sink

use defmt_rtt as _; // global defmt logger → RTT over SWD probe
use panic_probe as _; // panic handler → prints panics through defmt

#[defmt::panic_handler]
fn defmt_panic() -> ! {
    core::panic!()
}

/// Log level marker.
#[derive(Clone, Copy)]
pub enum Level {
    Debug,
    Info,
    Warn,
    Error,
}

/// Flush a formatted line to the defmt/RTT logger.
pub fn flush(level: Level, args: core::fmt::Arguments) {
    // Format into our stack buffer (max 160 chars, truncation-safe).
    let mut buf = [0u8; 160];
    let mut len = 0usize;

    struct Sink<'a>(&'a mut [u8], &'a mut usize);
    impl<'a> core::fmt::Write for Sink<'a> {
        fn write_str(&mut self, s: &str) -> core::fmt::Result {
            let avail = self.0.len() - *self.1;
            if avail == 0 {
                return Err(core::fmt::Error);
            }
            let n = s.len().min(avail);
            self.0[*self.1..*self.1 + n].copy_from_slice(&s.as_bytes()[..n]);
            *self.1 += n;
            Ok(())
        }
    }
    let mut sink = Sink(&mut buf, &mut len);
    let _ = sink.write_fmt(args);

    let text = core::str::from_utf8(&buf[..len]).unwrap_or("?");
    match level {
        Level::Error => defmt::error!("{}", text),
        Level::Warn => defmt::warn!("{}", text),
        Level::Info => defmt::info!("{}", text),
        Level::Debug => defmt::debug!("{}", text),
    }
}