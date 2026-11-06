#![no_std]
#![no_main]

use embassy_executor::Spawner;
use embassy_time::Timer;
use {defmt_rtt as _, panic_probe as _};

#[embassy_executor::main]
async fn main(_spawner: Spawner) {
    // 1. Initialize the HAL (Hardware Abstraction Layer)
    let _p = embassy_nrf::init(Default::default());

    // 2. Log initial setup messages
    defmt::info!("--- Micro:bit v2 RTT Logger Started ---");
    defmt::warn!("Testing a warning message.");
    defmt::error!("Testing an error message.");

    let mut counter = 0;

    // 3. Enter the async infinite loop
    loop {
        defmt::info!("Loop tick: {}", counter);
        counter += 1;

        // Yield execution for 1 second. 
        // The CPU drops into a low-power wfe sleep state here automatically.
        Timer::after_secs(1).await;
    }
}