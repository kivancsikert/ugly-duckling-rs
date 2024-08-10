mod kernel;

use anyhow::Result;
use embassy_executor::Executor;
use embassy_futures::join::join_array;
use embassy_futures::select::select;
use embassy_futures::select::Either::First;
use embassy_time::{Duration, Timer};
use esp_idf_hal::gpio::{AnyIOPin, AnyOutputPin, IOPin, PinDriver};
use esp_idf_hal::modem::Modem;
use esp_idf_hal::prelude::Peripherals;
use serde_json::json;
use static_cell::StaticCell;
use std::future::Future;
use std::pin::Pin;

static EXECUTOR: StaticCell<Executor> = StaticCell::new();

fn main() -> Result<()> {
    // It is necessary to call this function once. Otherwise some patches to the runtime
    // implemented by esp-idf-sys might not link properly. See https://github.com/esp-rs/esp-idf-template/issues/71
    esp_idf_svc::sys::link_patches();

    // Bind the log crate to the ESP Logging facilities
    esp_idf_svc::log::EspLogger::initialize_default();

    log::info!("  ______                   _    _       _");
    log::info!(" |  ____|                 | |  | |     | |");
    log::info!(" | |__ __ _ _ __ _ __ ___ | |__| |_   _| |__");
    log::info!(" |  __/ _` | '__| '_ ` _ \\|  __  | | | | '_ \\");
    log::info!(" | | | (_| | |  | | | | | | |  | | |_| | |_) |");
    log::info!(
        " |_|  \\__,_|_|  |_| |_| |_|_|  |_|\\__,_|_.__/ {}",
        env!("GIT_VERSION")
    );

    let executor = EXECUTOR.init(Executor::new());
    executor.run(|spawner| {
        spawner.spawn(run_tasks()).unwrap();
    });
}

macro_rules! task {
    ($task:expr) => {
        Box::pin(task($task)) as Pin<Box<dyn Future<Output = ()>>>
    };
}

#[embassy_executor::task]
async fn run_tasks() {
    let peripherals = Peripherals::take().expect("Failed to take peripherals");

    let tasks: [Pin<Box<dyn Future<Output = ()>>>; 3] = [
        task!(blink(peripherals.pins.gpio4.into())),
        task!(reset_watcher(peripherals.pins.gpio0.downgrade())),
        task!(start_device(peripherals.modem)),
    ];
    join_array(tasks).await;
}

async fn task<Fut: Future<Output = Result<()>>>(future: Fut) {
    let result = future.await;
    match result {
        Ok(_) => log::info!("Task completed successfully"),
        Err(e) => log::error!("Task failed with {:?}", e),
    };
}

async fn start_device(modem: Modem) -> Result<()> {
    let kernel = kernel::Device::init(modem).await?;

    let uptime_us = unsafe { esp_idf_sys::esp_timer_get_time() };
    log::info!("Device started in {} ms", uptime_us as f64 / 1000.0);

    kernel.mqtt.subscribe("commands/ping").await?;

    kernel.mqtt.publish("init", json!({
        "type": "ugly-duckling",
        "model": "mk6",
        "id": kernel.config.id,
        "instance": kernel.config.instance,
        // TODO Add mac
        "deviceConfig": serde_json::to_string(&kernel.config)?,
        // TODO Do we need this?
        "app": "ugly-duckling-rs",
        // TODO Extract this to static variable
        "version": env!("GIT_VERSION"),
        // TODO Add wakeup reason
        // TODO Add bootCount
        "time": std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_secs(),
        // TODO Add sleepWhenIdle
    })).await?;

    loop {
        kernel.mqtt
            .publish("telemetry", json!({
            "uptime": Duration::from_micros(unsafe { esp_idf_sys::esp_timer_get_time() as u64 }).as_millis(),
            "memory": unsafe { esp_idf_sys::esp_get_free_heap_size() },
        }))
            .await?;
        Timer::after_secs(5).await;
    }
}

async fn reset_watcher(pin: AnyIOPin) -> Result<()> {
    let mut button = PinDriver::input(pin)?;
    button.set_pull(esp_idf_hal::gpio::Pull::Up)?;
    log::info!("Factory reset watcher started");
    loop {
        button.wait_for_low().await?;
        log::info!("Button pressed, waiting for release");

        if let First(_) = select(Timer::after_secs(5), button.wait_for_high()).await {
            log::info!("Button pressed for more than 5 seconds, resetting WiFi");
            unsafe { esp_idf_sys::esp_wifi_restore() };
            unsafe { esp_idf_sys::esp_restart() };
        }
    }
}

async fn blink(pin: AnyOutputPin) -> Result<()> {
    log::info!("Blink task started");

    let mut status = PinDriver::output(pin)?;
    loop {
        status.set_high()?;
        Timer::after_secs(1).await;

        status.set_low()?;
        Timer::after_secs(1).await;
    }
}
