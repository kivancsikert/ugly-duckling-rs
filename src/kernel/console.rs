use std::io::Read;

use anyhow::Result;
use embassy_time::Timer;
use esp_idf_hal::gpio::{AnyOutputPin, PinDriver};

pub async fn console_handler_task(pin: AnyOutputPin) -> Result<()> {
    let mut stdin = std::io::stdin();
    let mut buf = [0u8];
    let mut status = PinDriver::output(pin)?;

    loop {
        match stdin.read_exact(&mut buf) {
            Ok(()) => (),
            _ => {
                Timer::after_millis(100).await;
                continue;
            }
        }
        status.set_low()?;
        Timer::after_millis(250).await;
        status.set_high()?;
        Timer::after_millis(250).await;
    }
}
