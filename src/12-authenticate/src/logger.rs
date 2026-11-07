//! Global async logger — the **only** place that owns the UART TX.
//!
//! Any module calls `log_debug("tag", format_args!(...))` and the message
//! is pushed into a global channel.  The `logger_task` drains the channel
//! and writes to the wire (with `\r\n`).
//!
//! Protocol frame transmission also goes through the same channel
//! (`send_frame`), so all UART output is serialised.

use core::fmt::Write;

use embassy_nrf::peripherals::UARTE0;
use embassy_nrf::uarte::UarteTx;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
use heapless::Vec;

use authenticate::constants::MAX_FRAME_LEN;

/// Channel message: up to `MAX_FRAME_LEN` bytes.
type Msg = Vec<u8, MAX_FRAME_LEN>;

/// Global queue — every `log_debug` / `send_frame` call pushes here.
static QUEUE: Channel<CriticalSectionRawMutex, Msg, 16> = Channel::new();

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Push raw bytes into the queue (non-blocking — drops if full).
fn enqueue(data: &[u8]) {
    let mut msg: Msg = Vec::new();
    let _ = msg.extend_from_slice(data);
    let _ = QUEUE.try_send(msg);
}

fn fmt_buf<'a>(buf: &'a mut [u8], args: core::fmt::Arguments<'_>) -> &'a [u8] {
    let mut pos = 0;
    struct Sink<'a>(&'a mut [u8], &'a mut usize);
    impl Write for Sink<'_> {
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
    let mut sink = Sink(buf, &mut pos);
    let _ = sink.write_fmt(args);
    &buf[..pos]
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Queue a protocol frame for transmission on the wire.
/// The logger task will append `\r\n`.
pub fn send_frame(data: &[u8]) {
    enqueue(data);
}

/// Format and queue a debug-log line.  The logger task prepends the
/// tag and appends `\r\n`.
///
/// This is the **only** function callers need — no `&mut serial` required.
#[cfg(debug_assertions)]
pub fn log_debug(tag: &str, args: core::fmt::Arguments<'_>) {
    let mut text_buf = [0u8; 160];
    let text = fmt_buf(&mut text_buf, args);

    // Build `TAG text` in a stack buffer, then push to queue.
    let mut line = [0u8; MAX_FRAME_LEN];
    let mut pos = 0usize;
    for &b in tag.as_bytes() {
        if pos < line.len() {
            line[pos] = b;
            pos += 1;
        }
    }
    for &b in text {
        if pos < line.len() {
            line[pos] = b;
            pos += 1;
        }
    }
    enqueue(&line[..pos]);
}

/// No-op in release builds — the compiler strips every call site.
#[cfg(not(debug_assertions))]
pub fn log_debug(_tag: &str, _args: core::fmt::Arguments<'_>) {}

// ---------------------------------------------------------------------------
// Logger task
// ---------------------------------------------------------------------------

/// Dedicated task that owns the UART TX peripheral.
/// Drains the global queue and writes every message to the wire.
#[embassy_executor::task]
pub async fn task(mut tx: UarteTx<'static, UARTE0>) {
    loop {
        let msg = QUEUE.receive().await;
        let _ = tx.write(&msg).await;
        let _ = tx.write(b"\r\n").await;
    }
}