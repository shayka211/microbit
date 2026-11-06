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

embassy_nrf::bind_interrupts!(struct Irqs {
    UARTE0_UART0 => embassy_nrf::uarte::InterruptHandler<embassy_nrf::peripherals::UARTE0>;
});

/// Queue through which the supervisor tells the display task what to show.
static PATTERNS: Channel<CriticalSectionRawMutex, display::Pattern, 8> = Channel::new();

/// Encode and transmit one protocol frame.
async fn send(serial: &mut serial::Serial, frame: &Frame) {
    let mut buf = [0u8; MAX_FRAME_LEN];
    if let Ok(n) = frame.encode(&mut buf) {
        serial.write_line(&buf[..n]).await;
    }
}

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    let p = embassy_nrf::init(Default::default());

    logger::flush(
        logger::Level::Info,
        format_args!("--- micro:bit auth firmware v0.1.0 ---"),
    );
    logger::flush(
        logger::Level::Info,
        format_args!("serial: UARTE0 115200 8N1 (DAPLink bridge -> /dev/ttyACM*)"),
    );

    // UART protocol channel (P1.08 RX / P0.06 TX).
    let uarte = embassy_nrf::uarte::Uarte::new(
        p.UARTE0,
        Irqs,
        p.P1_08,
        p.P0_06,
        Default::default(),
    );
    let mut serial = serial::Serial::new(uarte);

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
        logger::flush(
            logger::Level::Info,
            format_args!("uicr: state=PROVISIONED magic={:#010x}", UICR_MAGIC),
        );
        let _ = PATTERNS.try_send(display::Pattern::Idle);
    } else {
        logger::flush(
            logger::Level::Warn,
            format_args!("uicr: state=UNPROVISIONED (only PROV/PING accepted)"),
        );
        let _ = PATTERNS.try_send(display::Pattern::Unprovisioned);
    }

    // Boot banner.
    send(
        &mut serial,
        &Frame::Hello {
            magic: if provisioned { 1 } else { 0 },
        },
    )
    .await;
    logger::flush(
        logger::Level::Info,
        format_args!(
            "proto: tx HELLO AUTH/1.0 MAGIC:{}",
            if provisioned { 1 } else { 0 }
        ),
    );

    // An armed challenge: (Session, challenge nonce). `None` == idle.
    let mut session: Option<(Session, Nonce)> = None;

    loop {
        match session.take() {
            // ── Armed: wait for Button A or the window to elapse ───────────
            Some((mut sess, nonce)) => {
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
                            let mac = crypto::sign(&key.expect("authorized only when provisioned"), &nonce);
                            logger::flush(
                                logger::Level::Info,
                                format_args!("auth: button A pressed after {}ms -> AUTH_OK sent", elapsed),
                            );
                            send(&mut serial, &Frame::AuthOk(mac)).await;
                            let _ = PATTERNS.try_send(display::Pattern::Success);
                            Timer::after(Duration::from_millis(1_000)).await;
                            let _ = PATTERNS.try_send(display::Pattern::Idle);
                        }
                        ArmedKind::BtnTest => {
                            logger::flush(
                                logger::Level::Info,
                                format_args!(
                                    "btn-test: PRESS after {}ms -> tx BTN_PRESSED {}",
                                    elapsed, elapsed
                                ),
                            );
                            send(&mut serial, &Frame::BtnPressed(elapsed)).await;
                            let _ = PATTERNS.try_send(display::Pattern::Success);
                            Timer::after(Duration::from_millis(1_000)).await;
                            let _ = PATTERNS.try_send(display::Pattern::Idle);
                        }
                    },
                    Some(ArmedEvent::Timeout) => match sess.kind {
                        ArmedKind::Auth => {
                            logger::flush(
                                logger::Level::Warn,
                                format_args!(
                                    "auth: window {}ms elapsed, no press -> AUTH_TIMEOUT",
                                    sess.window_ms
                                ),
                            );
                            send(&mut serial, &Frame::AuthTimeout).await;
                            let _ = PATTERNS.try_send(display::Pattern::Idle);
                        }
                        ArmedKind::BtnTest => {
                            logger::flush(
                                logger::Level::Warn,
                                format_args!(
                                    "btn-test: timeout {}ms, no press -> BTN_TEST_TIMEOUT",
                                    sess.window_ms
                                ),
                            );
                            send(&mut serial, &Frame::BtnTestTimeout).await;
                            let _ = PATTERNS.try_send(display::Pattern::Idle);
                        }
                    },
                    None => {
                        // select only resolves on press or timeout; unreachable
                    }
                }
            }

            // ── Idle: wait for a host line ─────────────────────────────────
            None => {
                let line = match serial.read_line().await {
                    Ok(l) => l,
                    Err(()) => continue,
                };
                if line.len() > MAX_FRAME_LEN {
                    logger::flush(logger::Level::Warn, format_args!("proto: ERR badlen (line ignored)"));
                    continue;
                }

                match Frame::parse(line) {
                    Ok(Frame::Ping) => {
                        logger::flush(logger::Level::Info, format_args!("proto: PING -> PONG"));
                        send(&mut serial, &Frame::Pong).await;
                    }
                    Ok(Frame::Auth(nonce)) if key.is_some() => {
                        logger::flush(
                            logger::Level::Info,
                            format_args!(
                                "proto: challenge accepted -> READY, awaiting button A (window {}ms)",
                                AUTH_WINDOW_MS
                            ),
                        );
                        send(&mut serial, &Frame::Ready).await;
                        let _ = PATTERNS.try_send(display::Pattern::Waiting);
                        session = Some((Session::new(ArmedKind::Auth, AUTH_WINDOW_MS), nonce));
                    }
                    Ok(Frame::Auth(_)) => {
                        logger::flush(
                            logger::Level::Warn,
                            format_args!("proto: AUTH refused (unprovisioned)"),
                        );
                        send(&mut serial, &Frame::AuthErr(AuthErrKind::Unprovisioned)).await;
                    }
                    Ok(Frame::BtnTest) => {
                        logger::flush(
                            logger::Level::Info,
                            format_args!(
                                "proto: BTN_TEST armed (silence until press, window {}ms)",
                                BTN_TEST_WINDOW_MS
                            ),
                        );
                        send(&mut serial, &Frame::BtnTestReady).await;
                        let _ = PATTERNS.try_send(display::Pattern::Waiting);
                        session = Some((
                            Session::new(ArmedKind::BtnTest, BTN_TEST_WINDOW_MS),
                            Nonce::default(),
                        ));
                    }
                    Ok(Frame::Prov(key_hex)) => match key::provision(&key_hex) {
                        Ok(()) => {
                            key = key::load_key();
                            logger::flush(
                                logger::Level::Info,
                                format_args!(
                                    "uicr: 32B key written, readback match=ok magic={:#010x}",
                                    UICR_MAGIC
                                ),
                            );
                            let _ = PATTERNS.try_send(display::Pattern::Idle);
                            send(&mut serial, &Frame::ProvOk).await;
                        }
                        Err(kind) => {
                            logger::flush(
                                logger::Level::Error,
                                format_args!("uicr: provision failed ({:?})", kind),
                            );
                            send(&mut serial, &Frame::ProvErr(ProvErrKind::WriteFail)).await;
                        }
                    },
                    Ok(Frame::Lock) => {
                        logger::flush(
                            logger::Level::Warn,
                            format_args!("uicr: APPROTECT=0x0 -> READBACK LOCKED, resetting..."),
                        );
                        key::enable_readback_protection();
                        send(&mut serial, &Frame::LockOk).await;
                        Timer::after(Duration::from_millis(50)).await;
                        key::reset_chip();
                    }
                    Ok(_) => {
                        logger::flush(
                            logger::Level::Warn,
                            format_args!("proto: ignoring out-of-context frame (state=Idle)"),
                        );
                    }
                    Err(e) => {
                        logger::flush(
                            logger::Level::Warn,
                            format_args!("proto: parse error {:?}, line ignored", e),
                        );
                    }
                }
            }
        }
    }
}