use serde::{Deserialize, Serialize};
#[derive(Serialize, Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct AllocateRequest {
    pub target_udp_port: String,
    pub target_tcp_port: String,
    pub target_ssl_tcp_port: String,
    pub target_ip: String,
    pub session_id: String,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AllocateResponse {
    pub proxy_udp_port: u16,
    pub proxy_tcp_port: u16,
    pub proxy_ssl_tcp_port: u16,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LoadReport {
    #[serde(rename = "type")]
    pub _type: &'static str,
    pub load: u32,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RegisterRequest {
    #[serde(rename = "type")]
    pub _type: &'static str,
    pub guid: String,
    pub public_ipv4: String,
    pub public_ipv6: String,
    pub private_ip: String,
    pub az: String,
}
