#![no_std]
#![no_main]

//! micro:bit v2 authenticator firmware.
//!
//! This file only performs side effects (UART, UICR, LEDs, button). Every
//! protocol decision comes from `authenticate::device`, which is unit-tested
//! on the host and also drives the host crate's fake-device tests.

mod button;
mod display;
mod key;
mod logger;
mod serial;

use authenticate::constants::{MAX_FRAME_LEN, UICR_MAGIC};
use authenticate::device::{self, Action, DeviceState};
use authenticate::pattern::Pattern;
use authenticate::protocol::{Frame, Key, Nonce, ProvErrKind};
use authenticate::state::{ArmedEvent, ArmedKind};
use embassy_executor::Spawner;
use embassy_futures::select::{select, Either};
use embassy_nrf::gpio::Pin as _;
use embassy_time::{Duration, Timer};
use panic_halt as _;

embassy_nrf::bind_interrupts!(struct Irqs {
    UARTE0_UART0 => embassy_nrf::uarte::InterruptHandler<embassy_nrf::peripherals::UARTE0>;
});

/// Debug log line `TAG text`. Compiled out of release builds.
macro_rules! dlog {
    ($tag:literal, $($arg:tt)*) => {
        logger::log_debug($tag, format_args!($($arg)*))
    };
}

/// Queue a protocol frame for transmission (never dropped).
async fn send(frame: &Frame) {
    let mut buf = [0u8; MAX_FRAME_LEN];
    if let Ok(n) = frame.encode(&mut buf) {
        logger::send_frame(&buf[..n]).await;
    }
}

/// Everything the main loop owns.
struct App {
    button: button::ButtonA,
    key: Option<Key>,
}

impl App {
    fn state(&self) -> DeviceState {
        DeviceState {
            provisioned: self.key.is_some(),
            locked: key::is_locked(),
        }
    }

    fn resting(&self) -> Pattern {
        Pattern::resting(self.key.is_some())
    }

    /// Carry out what `device::on_line` decided.
    async fn perform(&mut self, action: Action) {
        match action {
            Action::Reply(frame) => {
                dlog!("#INF ", "proto: reply {}", frame.name());
                send(&frame).await;
            }
            Action::Arm {
                kind,
                window_ms,
                nonce,
            } => self.run_armed(kind, window_ms, nonce).await,
            Action::Provision(new_key) => self.provision(&new_key).await,
            Action::Lock => {
                dlog!(
                    "#WRN ",
                    "uicr: APPROTECT=0x0 -> READBACK LOCKED, resetting..."
                );
                key::enable_readback_protection();
                send(&Frame::LockOk).await;
                Timer::after(Duration::from_millis(50)).await; // let LOCK_OK drain
                key::reset_chip();
            }
            Action::Ignore(why) => {
                dlog!("#WRN ", "proto: line ignored ({:?})", why);
            }
        }
    }

    /// Armed session: show "press A", wait for the button or the window, answer.
    async fn run_armed(&mut self, kind: ArmedKind, window_ms: u32, nonce: Nonce) {
        dlog!(
            "#INF ",
            "armed: {:?}, waiting for button A ({}ms)",
            kind,
            window_ms
        );
        send(&kind.ready_frame()).await;
        display::set(Pattern::Waiting);

        let window = Timer::after(Duration::from_millis(window_ms as u64));
        let event = match select(self.button.wait_press(), window).await {
            Either::First(ms) => ArmedEvent::Press(ms),
            Either::Second(()) => ArmedEvent::Timeout,
        };

        let reply = device::armed_outcome(kind, event, self.key.as_ref(), &nonce);
        dlog!("#INF ", "armed: {:?} -> {}", event, reply.name());
        send(&reply).await;

        // Non-blocking: the display task returns to the resting pattern by
        // itself, and we go straight back to listening for the next line.
        let shown = match event {
            ArmedEvent::Press(_) => Pattern::Success,
            ArmedEvent::Timeout => Pattern::Fail,
        };
        display::flash(shown, self.resting());
    }

    async fn provision(&mut self, new_key: &Key) {
        match key::provision(new_key) {
            Ok(()) => {
                self.key = key::load_key();
                dlog!(
                    "#INF ",
                    "uicr: 32B key written, readback ok, magic={:#010x}",
                    UICR_MAGIC
                );
                display::set(self.resting());
                send(&Frame::ProvOk).await;
            }
            Err(e) => {
                dlog!("#ERR ", "uicr: provision failed ({:?})", e);
                send(&Frame::ProvErr(ProvErrKind::WriteFail)).await;
            }
        }
    }
}

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    let p = embassy_nrf::init(Default::default());

    // UART: TX belongs to the logger task, RX stays here.
    let uarte =
        embassy_nrf::uarte::Uarte::new(p.UARTE0, Irqs, p.P1_08, p.P0_06, Default::default());
    let (tx, rx) = uarte.split();
    spawner.spawn(logger::task(tx)).expect("spawn logger task");
    let mut serial = serial::Serial::new(rx);

    dlog!(
        "#INF ",
        "--- micro:bit auth firmware v{} ---",
        env!("CARGO_PKG_VERSION")
    );

    let matrix = display::Display::new(
        [
            p.P0_28.degrade(),
            p.P0_11.degrade(),
            p.P0_31.degrade(),
            p.P1_05.degrade(),
            p.P0_30.degrade(),
        ],
        [
            p.P0_21.degrade(),
            p.P0_22.degrade(),
            p.P0_15.degrade(),
            p.P0_24.degrade(),
            p.P0_19.degrade(),
        ],
    );
    spawner
        .spawn(display::task(matrix))
        .expect("spawn display task");

    let mut app = App {
        button: button::ButtonA::new(p.P0_14.degrade()),
        key: key::load_key(),
    };
    let st = app.state();
    dlog!(
        "#INF ",
        "uicr: provisioned={} locked={}",
        st.provisioned,
        st.locked
    );
    display::set(app.resting());
    send(&Frame::Hello {
        magic: st.provisioned as u8,
    })
    .await;

    loop {
        let action = match serial.read_line().await {
            Ok(line) => device::on_line(line, app.state()),
            Err(()) => continue, // UART error (e.g. framing); just resync
        };
        app.perform(action).await;
    }
}
