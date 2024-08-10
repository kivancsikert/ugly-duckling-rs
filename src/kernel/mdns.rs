use std::time::Duration;

use esp_idf_svc::mdns::{EspMdns, Interface, Protocol, QueryResult};

#[derive(Debug, Clone)]
pub struct Service {
    pub hostname: String,
    pub port: u16,
}

pub fn query_mdns(mdns: &EspMdns, service: &str, proto: &str) -> anyhow::Result<Option<Service>> {
    let mut results = [QueryResult {
        instance_name: None,
        hostname: None,
        port: 0,
        txt: Vec::new(),
        addr: Vec::new(),
        interface: Interface::STA,
        ip_protocol: Protocol::V4,
    }];
    mdns.query_ptr(service, proto, Duration::from_secs(5), 1, &mut results)?;
    log::info!("MDNS query result: {:?}", results);
    let result = results[0].clone();
    Ok(result.hostname.map(|hostname| Service {
        hostname: format!("{}.local", hostname),
        port: result.port,
    }))
}
