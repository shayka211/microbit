//! Global async logger — the **only** place that owns the UART TX.
//!
//! Any module calls `log_debug("tag", format_args!(...))` and the message
//! is pushed into a global channel.  The logger `task` drains it and writes
//! to the wire (with `\r\n`).
//!
//! Protocol frames (`send_frame`) use a **separate** queue so they can never
//! be lost behind debug chatter:
//! - frames: `send_frame().await` waits for space instead of dropping;
//! - logs: dropped when their queue is full (debug only, never critical);
//! - the task always writes pending frames before pending logs.
//!
//! Both share the one UART TX, so whole lines never interleave.

#[cfg(debug_assertions)]
use core::fmt::Write;

#[cfg(debug_assertions)]
use embassy_futures::select::{select, Either};
use embassy_nrf::peripherals::UARTE0;
use embassy_nrf::uarte::UarteTx;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
use heapless::Vec;

use authenticate::constants::MAX_FRAME_LEN;

/// Channel message: up to `MAX_FRAME_LEN` bytes.
type Msg = Vec<u8, MAX_FRAME_LEN>;

/// Protocol frames (never dropped).
static FRAMES: Channel<CriticalSectionRawMutex, Msg, 4> = Channel::new();
/// Debug-log lines (dropped when full).
#[cfg(debug_assertions)]
static LOGS: Channel<CriticalSectionRawMutex, Msg, 16> = Channel::new();

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

fn to_msg(data: &[u8]) -> Msg {
    let mut msg: Msg = Vec::new();
    // Callers never exceed MAX_FRAME_LEN; truncate defensively if they do.
    let n = data.len().min(MAX_FRAME_LEN);
    let _ = msg.extend_from_slice(&data[..n]);
    msg
}

#[cfg(debug_assertions)]
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

/// Queue a protocol frame for transmission (waits if the queue is full).
/// The logger task appends `\r\n`.
pub async fn send_frame(data: &[u8]) {
    FRAMES.send(to_msg(data)).await;
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
    let _ = LOGS.try_send(to_msg(&line[..pos]));
}

/// No-op in release builds — the compiler strips every call site.
#[cfg(not(debug_assertions))]
pub fn log_debug(_tag: &str, _args: core::fmt::Arguments<'_>) {}

// ---------------------------------------------------------------------------
// Logger task
// ---------------------------------------------------------------------------

/// Dedicated task that owns the UART TX peripheral.
/// Writes every queued message to the wire, frames first.
#[embassy_executor::task]
pub async fn task(mut tx: UarteTx<'static, UARTE0>) {
    loop {
        let msg = next_msg().await;
        let _ = tx.write(&msg).await;
        let _ = tx.write(b"\r\n").await;
    }
}

#[cfg(debug_assertions)]
async fn next_msg() -> Msg {
    // `select` polls its first future first, so a ready frame always wins.
    match select(FRAMES.receive(), LOGS.receive()).await {
        Either::First(m) | Either::Second(m) => m,
    }
}

#[cfg(not(debug_assertions))]
async fn next_msg() -> Msg {
    FRAMES.receive().await
}
