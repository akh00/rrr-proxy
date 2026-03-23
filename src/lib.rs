pub mod ebpf;
pub mod manager;
pub mod proxy;

pub mod consts {
    use std::env::var;
    use std::sync::LazyLock;
    use uuid::Uuid;

    pub static REGISTER_ENDPOINT: LazyLock<String> =
        LazyLock::new(|| match var("REGISTER_ENDPOINT") {
            Ok(reg_endpoint) => reg_endpoint,
            Err(_) => "ws://localhost:5555".to_string(),
        });
    pub static SERVICE_PORT: LazyLock<u16> = LazyLock::new(|| match var("SERVICE_PORT") {
        Ok(service_port) => service_port.parse::<u16>().unwrap_or(3333),
        Err(_) => 3333,
    });
    pub static METRICS_PORT: LazyLock<u16> = LazyLock::new(|| match var("METRICS_PORT") {
        Ok(metrics_port) => metrics_port.parse::<u16>().unwrap_or(9321),
        Err(_) => 9321,
    });
    pub static TRAFFIC_WAIT_TIMEOUT: LazyLock<u64> =
        LazyLock::new(|| match var("TRAFFIC_WAIT_TIMEOUT") {
            Ok(wait_timeout) => wait_timeout.parse::<u64>().unwrap_or(5),
            Err(_) => 5, // default
        });
    pub static HTTP_REQUEST_TIMEOUT: LazyLock<u64> =
        LazyLock::new(|| match var("HTTP_REQUEST_TIMEOUT") {
            Ok(wait_timeout) => wait_timeout.parse::<u64>().unwrap_or(1000),
            Err(_) => 1000, // default
        });
    pub static HTTP_REQUEST_CONCURRENT_NUM: LazyLock<usize> =
        LazyLock::new(|| match var("HTTP_REQUEST_CONCURRENT_NUM") {
            Ok(concurrent_num) => concurrent_num.parse::<usize>().unwrap_or(128),
            Err(_) => 128, // default
        });
    pub static PUBLIC_IPV4: LazyLock<String> = LazyLock::new(|| match var("PUBLIC_IPV4") {
        Ok(ipv4) => ipv4,
        Err(_) => "127.0.0.1".to_string(), // default
    });
    pub static PUBLIC_IPV6: LazyLock<String> = LazyLock::new(|| match var("PUBLIC_IPV6") {
        Ok(ipv6) => ipv6,
        Err(_) => "::1".to_string(), // default
    });
    pub static PRIVATE_IP: LazyLock<String> = LazyLock::new(|| match var("PRIVATE_IP") {
        Ok(ip) => ip,
        Err(_) => "192.168.49.1:3333".to_string(), // default
    });
    pub static AVAILABILITY_ZONE: LazyLock<String> =
        LazyLock::new(|| match var("AVAILABILITY_ZONE") {
            Ok(az) => az,
            Err(_) => "a".to_string(), // default
        });
    pub static PROXY_GUID: LazyLock<String> = LazyLock::new(|| match var("PROXY_GUID") {
        Ok(guid) => guid,
        Err(_) => Uuid::new_v4().to_string(), // default
    });
    pub static HEARTBIT_DELAY: LazyLock<u64> = LazyLock::new(|| match var("HEARTBIT_DELAY") {
        Ok(delay) => delay.parse::<u64>().unwrap_or(10000),
        Err(_) => 10000, // default
    });
    pub static UDP_CHANNEL_SIZE: LazyLock<usize> =
        LazyLock::new(|| match var("UDP_CHANNEL_SIZE") {
            Ok(delay) => delay.parse::<usize>().unwrap_or(10),
            Err(_) => 10, // default
        });
    pub static GRACEFULLL_SHUTDOWN_DELAY: LazyLock<u32> =
        LazyLock::new(|| match var("GRACEFULLL_SHUTDOWN_DELAY") {
            Ok(delay) => delay.parse::<u32>().unwrap_or(10),
            Err(_) => 10, // default
        });
    pub static MAX_REGISTRATION_ATTEMPTS: LazyLock<u16> =
        LazyLock::new(|| match var("MAX_REGISTRATION_ATTEMPTS") {
            Ok(attempts) => attempts.parse::<u16>().unwrap_or(10000),
            Err(_) => 10000, // default
        });
    pub static METRICS_EXPOSE_PATH: LazyLock<String> =
        LazyLock::new(|| match var("METRICS_EXPOSE_PATH") {
            Ok(expose_path) => expose_path,
            Err(_) => "/actuator/prometheus".to_string(), // default
        });
    pub static ALLOCATE_PATH: LazyLock<String> = LazyLock::new(|| match var("ALLOCATE_PATH") {
        Ok(allocate_path) => allocate_path,
        Err(_) => "/api/v1/proxy-ports".to_string(), // default
    });
    pub static DELETE_PATH: LazyLock<String> = LazyLock::new(|| match var("DELETE_PATH") {
        Ok(delete_path) => delete_path,
        Err(_) => "/api/v1/proxy-ports/{port}/{session_id}".to_string(), // default
    });
    pub static STATUS_PATH: LazyLock<String> = LazyLock::new(|| match var("STATUS_PATH") {
        Ok(status_path) => status_path,
        Err(_) => "/api/v1/status".to_string(), // default
    });
}
