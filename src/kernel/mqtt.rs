use crate::kernel::mdns;
use anyhow::Result;
use embassy_executor::Spawner;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::mutex::Mutex;
use embassy_sync::signal::Signal;
use esp_idf_svc::mdns::EspMdns;
use esp_idf_svc::mqtt::client::{
    EspAsyncMqttClient, EspAsyncMqttConnection, MqttClientConfiguration,
};
use esp_idf_svc::mqtt::client::{EventPayload, MessageId, QoS};
use serde_json::Value;
use std::sync::Arc;

pub struct Mqtt {
    topic_root: String,
    mqtt: Arc<Mutex<CriticalSectionRawMutex, EspAsyncMqttClient>>,
    _conn: Arc<Mutex<CriticalSectionRawMutex, EspAsyncMqttConnection>>,
}

type IncomingMessageHandler = Box<dyn Fn(&str, &str)>;

impl Mqtt {
    pub async fn create(
        mdns: &EspMdns,
        instance: &str,
        handler: IncomingMessageHandler,
    ) -> Result<Mqtt> {
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
        let mqtt = Arc::new(Mutex::new(mqtt));

        // TODO Figure out how to avoid this warning
        #[allow(clippy::arc_with_non_send_sync)]
        let conn = Arc::new(Mutex::new(conn));

        // TODO Need more robust reconnection logic, but for the time being it will do
        let connected = Arc::new(Signal::<CriticalSectionRawMutex, ()>::new());
        Spawner::for_current_executor()
            .await
            .spawn(handle_mqtt_events(
                conn.clone(),
                format!("{topic_root}/"),
                handler,
                connected.clone(),
            ))
            .expect("Couldn't spawn MQTT handler");
        connected.wait().await;

        Ok(Self {
            topic_root,
            mqtt,
            // TODO Do we need this?
            _conn: conn,
        })
    }

    pub async fn publish(&self, topic: &str, payload: Value) -> Result<MessageId> {
        let topic = format!("{}/{}", self.topic_root, topic);
        self.mqtt
            .lock()
            .await
            .publish(
                &topic,
                QoS::AtMostOnce,
                false,
                payload.to_string().as_bytes(),
            )
            .await
            .map_err(anyhow::Error::from)
    }

    pub async fn subscribe(&self, topic: &str) -> Result<MessageId> {
        let topic = format!("{}/{}", self.topic_root, topic);
        self.mqtt
            .lock()
            .await
            .subscribe(&topic, QoS::AtMostOnce)
            .await
            .map_err(anyhow::Error::from)
    }
}

#[embassy_executor::task]
async fn handle_mqtt_events(
    conn: Arc<Mutex<CriticalSectionRawMutex, EspAsyncMqttConnection>>,
    incoming_prefix: String,
    handler: IncomingMessageHandler,
    connected: Arc<Signal<CriticalSectionRawMutex, ()>>,
) {
    loop {
        let mut conn = conn.lock().await;
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
                let data = std::str::from_utf8(data);
                if let Ok(data) = data {
                    if let Some(path) = topic {
                        if let Some(path) = path.strip_prefix(&incoming_prefix) {
                            log::info!("Received message for path: {:?}", path);
                            (handler)(path, data);
                            continue;
                        }
                    }
                    log::info!("Received message on unknown topic: {:?}", topic);
                } else {
                    log::error!(
                        "Failed to parse message: {:?} for topic {:?}",
                        data.err(),
                        topic
                    );
                }
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
