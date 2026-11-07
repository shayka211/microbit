//! 5×5 LED-matrix driver task (rows = anodes, active high; cols = cathodes,
//! active low).
//!
//! *What* to show is decided by [`authenticate::pattern::Screen`] (pure,
//! host-tested). This task just scans the matrix and sleeps whenever nothing
//! is lit, waking only on the next blink edge / flash end or a new [`View`].

use authenticate::pattern::{lit, Bitmap, Pattern, Screen, View, BLANK};
use embassy_futures::select::{select, Either};
use embassy_nrf::gpio::{AnyPin, Level, Output, OutputDrive};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::signal::Signal;
use embassy_time::{Duration, Instant, Timer};

/// Latest requested view. A `Signal` (not a queue): only the newest view
/// matters, and sending can never fail or block.
static VIEW: Signal<CriticalSectionRawMutex, View> = Signal::new();

/// Show `p` until told otherwise.
pub fn set(p: Pattern) {
    VIEW.signal(View::steady(p));
}

/// Show `p` briefly, then `then`.
pub fn flash(p: Pattern, then: Pattern) {
    VIEW.signal(View::flash(p, then));
}

/// How long each row stays lit per scan (5 rows ⇒ 200 Hz refresh).
const ROW_TIME: Duration = Duration::from_millis(1);

pub struct Display {
    cols: [Output<'static, AnyPin>; 5],
    rows: [Output<'static, AnyPin>; 5],
}

impl Display {
    pub fn new(cols: [AnyPin; 5], rows: [AnyPin; 5]) -> Self {
        Self {
            cols: cols.map(|p| Output::new(p, Level::High, OutputDrive::Standard)),
            rows: rows.map(|p| Output::new(p, Level::Low, OutputDrive::Standard)),
        }
    }

    /// One full scan of `bitmap` (~5 ms). Leaves every LED off afterwards.
    async fn scan(&mut self, bitmap: &Bitmap) {
        for r in 0..5 {
            // Columns first, then the row — avoids ghosting the previous row.
            for (c, col) in self.cols.iter_mut().enumerate() {
                col.set_level(if lit(bitmap, r, c) {
                    Level::Low
                } else {
                    Level::High
                });
            }
            self.rows[r].set_high();
            Timer::after(ROW_TIME).await;
            self.rows[r].set_low();
        }
    }
}

fn now_ms() -> u64 {
    Instant::now().as_millis()
}

#[embassy_executor::task]
pub async fn task(mut display: Display) {
    let mut screen = Screen::new(now_ms());

    loop {
        let now = now_ms();
        let bitmap = screen.bitmap(now);

        if bitmap != BLANK {
            display.scan(&bitmap).await;
            // Pick up a new view between scans (never mid-scan).
            if VIEW.signaled() {
                let view = VIEW.wait().await; // ready: returns immediately
                screen.apply(view, now_ms());
            }
            continue;
        }

        // Nothing lit: sleep (WFE) until the screen changes by itself or a
        // new view arrives.
        let view = match screen.next_change(now) {
            None => Some(VIEW.wait().await),
            Some(at) => match select(VIEW.wait(), Timer::at(Instant::from_millis(at))).await {
                Either::First(view) => Some(view),
                Either::Second(()) => None,
            },
        };
        if let Some(view) = view {
            screen.apply(view, now_ms());
        }
    }
}
