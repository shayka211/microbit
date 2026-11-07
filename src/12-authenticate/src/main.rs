#![no_std]
#![no_main]

mod button;
mod display;
mod key;
mod logger;
mod serial;

use authenticate::constants::{AUTH_WINDOW_MS, BTN_TEST_WINDOW_MS, MAX_FRAME_LEN, UICR_MAGIC};
use authenticate::crypto;
use authenticate::protocol::{AuthErrKind, Frame, Nonce, ProvErrKind};
use authenticate::state::{ArmedEvent, ArmedKind, Session};
use embassy_executor::Spawner;
use embassy_futures::select::{select, Either};
use embassy_nrf::gpio::Pin as _;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
use embassy_time::{Duration, Instant, Timer};
use panic_halt as _;

embassy_nrf::bind_interrupts!(struct Irqs {
    UARTE0_UART0 => embassy_nrf::uarte::InterruptHandler<embassy_nrf::peripherals::UARTE0>;
});

/// Queue through which the supervisor tells the display task what to show.
static PATTERNS: Channel<CriticalSectionRawMutex, display::Pattern, 8> = Channel::new();

// ---------------------------------------------------------------------------
// Helpers (thin wrappers around `logger` so call sites are short)
// ---------------------------------------------------------------------------

/// Queue a protocol frame for transmission on the wire.
fn send(frame: &Frame) {
    let mut buf = [0u8; MAX_FRAME_LEN];
    if let Ok(n) = frame.encode(&mut buf) {
        logger::send_frame(&buf[..n]);
    }
}

/// Debug-log: `TAG text\r\n`.  No-op in release builds.
#[cfg(debug_assertions)]
fn dlog(tag: &str, args: core::fmt::Arguments<'_>) {
    logger::log_debug(tag, args);
}
#[cfg(not(debug_assertions))]
fn dlog(_tag: &str, _args: core::fmt::Arguments<'_>) {}

// ---------------------------------------------------------------------------
// Protocol state-machine stages
// ---------------------------------------------------------------------------

/// Run one armed session: wait for Button A or the window to expire.
/// Always transitions back to idle after queueing the appropriate frame.
async fn run_armed(
    button: &mut button::ButtonA,
    key: &Option<[u8; 32]>,
    mut sess: Session,
    nonce: Nonce,
) {
    let start = Instant::now();
    let outcome = select(
        button.wait_press(),
        Timer::after(Duration::from_millis(sess.window_ms as u64)),
    )
    .await;
    let press_ms = match outcome {
        Either::First(ms) => Some(ms),
        Either::Second(_) => None,
    };
    let now_ms = start.elapsed().as_millis() as u32;
    let event = sess.step(now_ms, press_ms);

    match event {
        Some(ArmedEvent::Press(elapsed)) => match sess.kind {
            ArmedKind::Auth => {
                let k = key.as_ref().expect("authorized only when provisioned");
                let mac = crypto::sign(k, &nonce);
                dlog(
                    "#INF ",
                    format_args!("auth: button A pressed after {}ms -> AUTH_OK sent", elapsed),
                );
                send(&Frame::AuthOk(mac));
                show_then_idle().await;
            }
            ArmedKind::BtnTest => {
                dlog(
                    "#INF ",
                    format_args!(
                        "btn-test: PRESS after {}ms -> tx BTN_PRESSED {}",
                        elapsed, elapsed
                    ),
                );
                send(&Frame::BtnPressed(elapsed));
                show_then_idle().await;
            }
        },
        Some(ArmedEvent::Timeout) => match sess.kind {
            ArmedKind::Auth => {
                dlog(
                    "#WRN ",
                    format_args!(
                        "auth: window {}ms elapsed, no press -> AUTH_TIMEOUT",
                        sess.window_ms
                    ),
                );
                send(&Frame::AuthTimeout);
                let _ = PATTERNS.try_send(display::Pattern::Idle);
            }
            ArmedKind::BtnTest => {
                dlog(
                    "#WRN ",
                    format_args!(
                        "btn-test: timeout {}ms, no press -> BTN_TEST_TIMEOUT",
                        sess.window_ms
                    ),
                );
                send(&Frame::BtnTestTimeout);
                let _ = PATTERNS.try_send(display::Pattern::Idle);
            }
        },
        None => {} // unreachable: select always resolves to press or timeout
    }
}

/// Flash the success pattern for 1 second, then return to idle.
async fn show_then_idle() {
    let _ = PATTERNS.try_send(display::Pattern::Success);
    Timer::after(Duration::from_millis(1_000)).await;
    let _ = PATTERNS.try_send(display::Pattern::Idle);
}

/// Process one received protocol frame while the device is idle.
/// Returns the new armed session if the frame triggered one.
async fn handle_frame(
    key: &mut Option<[u8; 32]>,
    line: &[u8],
) -> Option<(Session, Nonce)> {
    if line.len() > MAX_FRAME_LEN {
        dlog("#WRN ", format_args!("proto: ERR badlen (line ignored)"));
        return None;
    }

    match Frame::parse(line) {
        Ok(Frame::Ping) => {
            dlog("#INF ", format_args!("proto: PING -> PONG"));
            send(&Frame::Pong);
            None
        }
        Ok(Frame::Auth(nonce)) if key.is_some() => {
            dlog(
                "#INF ",
                format_args!(
                    "proto: challenge accepted -> READY, awaiting button A (window {}ms)",
                    AUTH_WINDOW_MS
                ),
            );
            send(&Frame::Ready);
            let _ = PATTERNS.try_send(display::Pattern::Waiting);
            Some((Session::new(ArmedKind::Auth, AUTH_WINDOW_MS), nonce))
        }
        Ok(Frame::Auth(_)) => {
            dlog("#WRN ", format_args!("proto: AUTH refused (unprovisioned)"));
            send(&Frame::AuthErr(AuthErrKind::Unprovisioned));
            None
        }
        Ok(Frame::BtnTest) => {
            dlog(
                "#INF ",
                format_args!(
                    "proto: BTN_TEST armed (silence until press, window {}ms)",
                    BTN_TEST_WINDOW_MS
                ),
            );
            send(&Frame::BtnTestReady);
            let _ = PATTERNS.try_send(display::Pattern::Waiting);
            Some((
                Session::new(ArmedKind::BtnTest, BTN_TEST_WINDOW_MS),
                Nonce::default(),
            ))
        }
        Ok(Frame::Prov(key_hex)) => {
            match key::provision(&key_hex) {
                Ok(()) => {
                    *key = key::load_key();
                    dlog(
                        "#INF ",
                        format_args!(
                            "uicr: 32B key written, readback match=ok magic={:#010x}",
                            UICR_MAGIC
                        ),
                    );
                    let _ = PATTERNS.try_send(display::Pattern::Idle);
                    send(&Frame::ProvOk);
                }
                Err(kind) => {
                    dlog(
                        "#ERR ",
                        format_args!("uicr: provision failed ({:?})", kind),
                    );
                    send(&Frame::ProvErr(ProvErrKind::WriteFail));
                }
            }
            None
        }
        Ok(Frame::Lock) => {
            dlog(
                "#WRN ",
                format_args!("uicr: APPROTECT=0x0 -> READBACK LOCKED, resetting..."),
            );
            key::enable_readback_protection();
            send(&Frame::LockOk);
            Timer::after(Duration::from_millis(50)).await;
            key::reset_chip();
        }
        Ok(_) => {
            dlog(
                "#WRN ",
                format_args!("proto: ignoring out-of-context frame (state=Idle)"),
            );
            None
        }
        Err(e) => {
            dlog(
                "#WRN ",
                format_args!("proto: parse error {:?}, line ignored", e),
            );
            None
        }
    }
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    let p = embassy_nrf::init(Default::default());

    // UART — split TX/RX.  TX goes to the logger task, RX stays here.
    let uarte = embassy_nrf::uarte::Uarte::new(
        p.UARTE0,
        Irqs,
        p.P1_08,
        p.P0_06,
        Default::default(),
    );
    let (tx, rx) = uarte.split();
    spawner.spawn(logger::task(tx)).expect("spawn logger task");
    let mut serial = serial::Serial::new(rx);

    dlog(
        "#INF ",
        format_args!("--- micro:bit auth firmware v0.1.0 ---"),
    );
    dlog(
        "#INF ",
        format_args!("serial: UARTE0 115200 8N1 (DAPLink bridge -> /dev/ttyACM*)"),
    );

    // LED matrix.
    let display::Display { cols, rows } = display::Display::new(
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
        .spawn(display::task(&PATTERNS, cols, rows))
        .expect("spawn display task");

    // Button A (P0.14).
    let mut button = button::ButtonA::new(p.P0_14.degrade());

    let mut key = key::load_key();
    let provisioned = key.is_some();
    if provisioned {
        dlog(
            "#INF ",
            format_args!("uicr: state=PROVISIONED magic={:#010x}", UICR_MAGIC),
        );
        let _ = PATTERNS.try_send(display::Pattern::Idle);
    } else {
        dlog(
            "#WRN ",
            format_args!("uicr: state=UNPROVISIONED (only PROV/PING accepted)"),
        );
        let _ = PATTERNS.try_send(display::Pattern::Unprovisioned);
    }

    // Boot banner.
    send(&Frame::Hello {
        magic: if provisioned { 1 } else { 0 },
    });
    dlog(
        "#INF ",
        format_args!(
            "proto: tx HELLO AUTH/1.0 MAGIC:{}",
            if provisioned { 1 } else { 0 }
        ),
    );

    let mut session: Option<(Session, Nonce)> = None;

    loop {
        match session.take() {
            Some((sess, nonce)) => {
                run_armed(&mut button, &key, sess, nonce).await;
            }
            None => {
                let line = match serial.read_line().await {
                    Ok(l) => l,
                    Err(()) => continue,
                };
                let mut tmp = [0u8; MAX_FRAME_LEN];
                let n = line.len();
                tmp[..n].copy_from_slice(line);
                session = handle_frame(&mut key, &tmp[..n]).await;
            }
        }
    }
}