//! 5×5 LED-matrix display task (row-anode / col-cathode polarity).

use crate::logger::flush;
use crate::logger::Level as LogLevel;
use embassy_nrf::gpio::{AnyPin, Level, Output, OutputDrive};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
use embassy_time::{Duration, Timer};

/// Frames the display task can show.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Pattern {
    /// Everything off.
    Idle,
    /// Blinking middle row bar — "challenge accepted, press A".
    Waiting,
    /// Static tick — outcome: success.
    Success,
    /// Static cross — outcome: failure (kept for diagnostics / future AUTH_ERR flash).
    #[allow(dead_code)]
    Fail,
    /// Blinking corner dots — unprovisioned.
    Unprovisioned,
}

/// Owned 5×5 matrix (one embassy `Output` per pin).
pub struct Display {
    pub cols: [Output<'static, AnyPin>; 5],
    pub rows: [Output<'static, AnyPin>; 5],
}

impl Display {
    pub fn new(cols: [AnyPin; 5], rows: [AnyPin; 5]) -> Self {
        let cols = cols.map(|p| Output::new(p, Level::High, OutputDrive::Standard));
        let rows = rows.map(|p| Output::new(p, Level::Low, OutputDrive::Standard));
        Self { cols, rows }
    }
}

/// Render a pattern (with blink phase) to a 5×5 bitmap; bit 1 = LED on.
fn frame_for(pattern: Pattern, blink: bool) -> [[u8; 5]; 5] {
    let empty = [[0u8; 5]; 5];
    let mut f = empty;
    match pattern {
        Pattern::Idle => {}
        Pattern::Waiting => {
            if blink {
                f[2] = [1; 5];
            }
        }
        Pattern::Success => {
            // tick
            for (r, c) in [(0, 4), (1, 3), (2, 2), (3, 1), (3, 2), (4, 0), (4, 1)] {
                f[r][c] = 1;
            }
        }
        Pattern::Fail => {
            // cross
            for (r, c) in [
                (0, 0),
                (0, 4),
                (1, 1),
                (1, 3),
                (2, 2),
                (3, 1),
                (3, 3),
                (4, 0),
                (4, 4),
            ] {
                f[r][c] = 1;
            }
        }
        Pattern::Unprovisioned => {
            if blink {
                for (r, c) in [(0, 0), (0, 4), (4, 0), (4, 4)] {
                    f[r][c] = 1;
                }
            }
        }
    }
    f
}

/// Row-scanning multiplexer. Runs forever; picks up `Pattern` updates from the
/// channel whenever one is queued.
#[embassy_executor::task]
pub async fn task(
    patterns: &'static Channel<CriticalSectionRawMutex, Pattern, 8>,
    mut cols: [Output<'static, AnyPin>; 5],
    mut rows: [Output<'static, AnyPin>; 5],
) {
    const SCAN: Duration = Duration::from_micros(1_000);
    let mut pattern = Pattern::Idle;
    let mut frame_count = 0u32;
    let mut blink = false;

    loop {
        frame_count = frame_count.wrapping_add(1);
        // 5 rows × 1 ms ≈ 5 ms/frame, so 200 frames ≈ 1 second per blink phase.
        if frame_count % 200 == 0 {
            blink = !blink;
        }
        if let Ok(p) = patterns.try_receive() {
            if p != pattern {
                flush(LogLevel::Debug, format_args!("display: pattern -> {p:?}"));
                pattern = p;
            }
        }
        let frame = frame_for(pattern, blink);

        for row in 0..5 {
            rows[row].set_high();
            for col in 0..5 {
                if frame[row][col] != 0 {
                    cols[col].set_low();
                } else {
                    cols[col].set_high();
                }
            }
            Timer::after(SCAN).await;
            for c in cols.iter_mut() {
                c.set_high();
            }
            rows[row].set_low();
        }
    }
}