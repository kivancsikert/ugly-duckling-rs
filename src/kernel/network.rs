use std::time::Duration;

use crate::kernel::mdns;
use anyhow::Result;
use embassy_executor::Spawner;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::mutex::Mutex;
use embassy_sync::signal::Signal;
use esp_idf_svc::mdns::EspMdns;
use esp_idf_svc::mqtt::client::EventPayload;
use esp_idf_svc::mqtt::client::{
    EspAsyncMqttClient, EspAsyncMqttConnection, MqttClientConfiguration,
};
use esp_idf_svc::sntp::{self, EspSntp};
use std::sync::Arc;
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

pub async fn init_mqtt(
    mdns: &EspMdns,
    instance: &str,
) -> Result<(
    EspAsyncMqttClient,
    Arc<Mutex<CriticalSectionRawMutex, EspAsyncMqttConnection>>,
    String,
)> {
    let mqtt = mdns::query_mdns(mdns, "_mqtt", "_tcp")?.unwrap_or_else(|| mdns::Service {
        hostname: String::from("bumblebee.local"),
        port: 1883,
    });
    log::info!("MDNS query result: {:?}", mqtt);

    let url: &str = &format!("mqtt://{}:{}", mqtt.hostname, mqtt.port);
    let (mqtt, conn) = EspAsyncMqttClient::new(
        url,
        &MqttClientConfiguration {
            client_id: Some(instance),
            ..Default::default()
        },
    )
    .expect("Couldn't connect to MQTT");

    let topic_root = format!("devices/ugly-duckling/{}", instance);

    // TODO Figure out how to avoid this warning
    #[allow(clippy::arc_with_non_send_sync)]
    let conn = Arc::new(Mutex::new(conn));

    // TODO Need something more robust than this, but for the time being it will do
    let connected = Arc::new(Signal::<CriticalSectionRawMutex, ()>::new());
    Spawner::for_current_executor()
        .await
        .spawn(handle_mqtt_events(conn.clone(), connected.clone()))
        .expect("Couldn't spawn MQTT handler");
    connected.wait().await;

    Ok((mqtt, conn, topic_root))
}

#[embassy_executor::task]
async fn handle_mqtt_events(
    conn: Arc<Mutex<CriticalSectionRawMutex, EspAsyncMqttConnection>>,
    connected: Arc<Signal<CriticalSectionRawMutex, ()>>,
) {
    let mut conn = conn.lock().await;
    loop {
        let event = conn.next().await.expect("Cannot receive message");
        match event.payload() {
            EventPayload::Received {
                id,
                topic,
                data,
                details,
            } => {
                log::info!(
                    "Received message with ID {} on topic {:?}: {:?}; details: {:?}",
                    id,
                    topic,
                    std::str::from_utf8(data),
                    details
                );
            }
            EventPayload::Connected(session_present) => {
                log::info!(
                    "Connected to MQTT broker (session present: {})",
                    session_present
                );
                connected.signal(());
            }
            EventPayload::Disconnected => {
                log::info!("Disconnected from MQTT broker");
                // TODO Reconnect
            }
            _ => {
                log::info!("Received event: {:?}", event.payload());
            }
        }
    }
}
