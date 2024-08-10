use crate::kernel::mdns;
use anyhow::Result;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::signal::Signal;
use esp_idf_svc::mdns::EspMdns;
use esp_idf_svc::sntp::{self, EspSntp};
use std::time::Duration;
use std::time::UNIX_EPOCH;

pub async fn init_rtc(mdns: &EspMdns) -> Result<EspSntp<'static>> {
    let ntp = mdns::query_mdns(mdns, "_ntp", "_udp")?.unwrap_or_else(|| mdns::Service {
        hostname: String::from("pool.ntp.org"),
        port: 123,
    });

    log::info!(
        "Time before SNTP sync: {:?}, synchronizing with {:?}",
        std::time::SystemTime::now(),
        ntp
    );

    static SNTP_UPDATED: Signal<CriticalSectionRawMutex, ()> = Signal::new();
    let sntp = sntp::EspSntp::new_with_callback(
        &sntp::SntpConf {
            servers: [ntp.hostname.as_str()],
            ..Default::default()
        },
        |synced_time| {
            SNTP_UPDATED.signal(());
            log::info!("Time synced via SNTP: {:?}", synced_time);
        },
    )?;
    // When the RTC is not initialized, the MCU boots with a time of 0;
    // if we are much "later" then it means the RTC has been initialized
    // at some point in the past
    if std::time::SystemTime::now()
        < UNIX_EPOCH + Duration::from_secs((2022 - 1970) * 365 * 24 * 60 * 60)
    {
        log::info!("RTC is not initialized, waiting for SNTP sync");
        SNTP_UPDATED.wait().await;
    } else {
        log::info!("RTC seems to be initialized already, not waiting for SNTP sync");
    }
    Ok(sntp)
}
