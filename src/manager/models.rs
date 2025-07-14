use serde::{Deserialize, Serialize};
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AllocateRequest {
    pub target_ip: String,
    pub target_udp_port: u16,
    pub target_tcp_port: u16,
    pub target_ssl_tcp_port: u16,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AllocateResponse {
    pub proxy_udp_port: u16,
    pub proxy_tcp_port: u16,
    pub proxy_ssl_tcp_port: u16,
    pub load_percentage: u16,
}
