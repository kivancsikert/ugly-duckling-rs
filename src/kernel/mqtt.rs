use crate::kernel::mdns;
use crate::make_static;
use anyhow::Result;
use embassy_executor::Spawner;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::{Channel, Receiver, Sender};
use embassy_sync::mutex::Mutex;
use embassy_sync::signal::Signal;
use esp_idf_svc::mdns::EspMdns;
use esp_idf_svc::mqtt::client::{
    EspAsyncMqttClient, EspAsyncMqttConnection, MqttClientConfiguration,
};
use esp_idf_svc::mqtt::client::{EventPayload, MessageId, QoS};
use serde::Serialize;
use std::sync::Arc;

const MQTT_PUBLISH_QUEUE_SIZE: usize = 16;

#[derive(Debug)]
struct MqttMessage {
    topic: String,
    qos: QoS,
    retain: bool,
    payload: String,
}

pub struct Mqtt {
    topic_root: String,
    mqtt: Arc<Mutex<CriticalSectionRawMutex, EspAsyncMqttClient>>,
    _conn: Arc<Mutex<CriticalSectionRawMutex, EspAsyncMqttConnection>>,
    publish_sender:
        Arc<Sender<'static, CriticalSectionRawMutex, MqttMessage, MQTT_PUBLISH_QUEUE_SIZE>>,
}

pub type IncomingMessageResponse = Option<(String, String)>;
type IncomingMessageHandler = Box<dyn Fn(&str, &str) -> Result<IncomingMessageResponse>>;

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

        let publish_channel =
            make_static!(Channel::<CriticalSectionRawMutex, MqttMessage, MQTT_PUBLISH_QUEUE_SIZE>);
        let publish_sender = Arc::new(publish_channel.sender());
        let publish_receiver = Arc::new(publish_channel.receiver());
        Spawner::for_current_executor()
            .await
            .spawn(handle_mqtt_publish(mqtt.clone(), publish_receiver))
            .expect("Couldn't spawn MQTT publisher");

        // TODO Need more robust reconnection logic, but for the time being it will do
        let connected = Arc::new(Signal::<CriticalSectionRawMutex, ()>::new());
        Spawner::for_current_executor()
            .await
            .spawn(handle_mqtt_events(
                conn.clone(),
                publish_sender.clone(),
                format!("{topic_root}/"),
                handler,
                connected.clone(),
            ))
            .expect("Couldn't spawn MQTT incoming event handler");
        connected.wait().await;

        Ok(Self {
            topic_root,
            mqtt,
            // TODO Do we need this?
            _conn: conn,
            publish_sender,
        })
    }

    pub async fn publish<T>(&self, topic: &str, payload: &T) -> Result<()>
    where
        T: ?Sized + Serialize,
    {
        let topic = format!("{}/{}", self.topic_root, topic);
        let payload = serde_json::to_string(payload).map_err(anyhow::Error::from)?;
        let message = MqttMessage {
            topic,
            qos: QoS::AtMostOnce,
            retain: false,
            payload,
        };
        // TODO What happens when the queue is full?
        self.publish_sender.send(message).await;
        Ok(())
    }

    pub async fn subscribe(&self, topic: &str) -> Result<MessageId> {
        log::info!("Subscribing to topic: {:?}", topic);
        let topic = format!("{}/{}", self.topic_root, topic);
        let message_id = self
            .mqtt
            .lock()
            .await
            .subscribe(&topic, QoS::AtMostOnce)
            .await
            .map_err(anyhow::Error::from)?;
        log::info!(
            "Subscribed to topic: {:?} with message ID {}",
            topic,
            message_id
        );
        Ok(message_id)
    }
}

#[embassy_executor::task]
async fn handle_mqtt_publish(
    mqtt: Arc<Mutex<CriticalSectionRawMutex, EspAsyncMqttClient>>,
    receiver: Arc<Receiver<'static, CriticalSectionRawMutex, MqttMessage, MQTT_PUBLISH_QUEUE_SIZE>>,
) {
    loop {
        let message = receiver.receive().await;
        let result = mqtt
            .lock()
            .await
            .publish(
                &message.topic,
                message.qos,
                message.retain,
                message.payload.as_bytes(),
            )
            .await;
        if let Err(e) = result {
            log::error!("Failed to publish message: {:?}, message: {:?}", e, message);
        }
    }
}

#[embassy_executor::task]
async fn handle_mqtt_events(
    conn: Arc<Mutex<CriticalSectionRawMutex, EspAsyncMqttConnection>>,
    publish_sender: Arc<
        Sender<'static, CriticalSectionRawMutex, MqttMessage, MQTT_PUBLISH_QUEUE_SIZE>,
    >,
    prefix: String,
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
                        if let Some(path) = path.strip_prefix(&prefix) {
                            log::info!("Received message for path: {:?}", path);
                            let result = (handler)(path, data);
                            match result {
                                Ok(Some((response_path, response))) => {
                                    let response_topic = format!("{}{}", prefix, response_path);
                                    log::info!("Publishing response to: {:?}", response_topic);
                                    publish_sender
                                        .send(MqttMessage {
                                            topic: response_topic,
                                            qos: QoS::AtMostOnce,
                                            retain: false,
                                            payload: response,
                                        })
                                        .await;
                                }
                                Ok(None) => {}
                                Err(e) => {
                                    log::error!("Error handling message: {:?}", e);
                                    // TODO Publish error
                                }
                            }
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
