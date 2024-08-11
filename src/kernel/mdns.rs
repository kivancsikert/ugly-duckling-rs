use std::{net::IpAddr, time::Duration};

use anyhow::{anyhow, Result};
use esp_idf_svc::mdns::{EspMdns, Interface, Protocol, QueryResult};
use esp_idf_sys::ESP_ERR_NOT_FOUND;

#[derive(Debug, Clone)]
pub struct Service {
    pub hostname: String,
    pub addr: IpAddr,
    pub port: u16,
}

pub fn query_mdns_address(mdns: &EspMdns, server: &str, port: u16) -> Result<Option<Service>> {
    let response = mdns.query_a(server, Duration::from_secs(5));
    log::info!("MDNS query result: {:?}", response);
    match response {
        Ok(addr) => {
            let addr = IpAddr::V4(addr);
            Ok(Some(Service {
                hostname: server.to_string(),
                addr,
                port,
            }))
        }
        Err(e) => {
            if e.code() == ESP_ERR_NOT_FOUND {
                Ok(None)
            } else {
                Err(anyhow!(e.to_string()))
            }
        }
    }
}

pub fn query_mdns(mdns: &EspMdns, service: &str, proto: &str) -> Result<Option<Service>> {
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
        addr: result.addr[0],
        port: result.port,
    }))
}
