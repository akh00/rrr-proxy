use std::future::ready;

use axum::{Router, routing::get};
use metrics_exporter_prometheus::{Matcher, PrometheusBuilder, PrometheusHandle};

use crate::consts;

pub mod globals {
    use std::sync::LazyLock;

    use metrics::{Counter, counter};

    pub static IN_UDP_SOCKET_COUNTER: LazyLock<Counter> =
        LazyLock::new(|| counter!("allocate_in_udp_socket"));
    pub static OUT_UDP_SOCKET_COUNTER: LazyLock<Counter> =
        LazyLock::new(|| counter!("allocate_out_udp_socket"));
    pub static IDLE_IN_UDP_SOCKET: LazyLock<Counter> =
        LazyLock::new(|| counter!("idle_in_udp_socket"));
    pub static IDLE_OUT_UDP_SOCKET: LazyLock<Counter> =
        LazyLock::new(|| counter!("idle_out_udp_socket"));
    pub static TCP_CONNECTION_TO_SRV: LazyLock<Counter> =
        LazyLock::new(|| counter!("tcp_connections_to_ms"));
    pub static TCP_SOCKET_ALLOCATE: LazyLock<Counter> =
        LazyLock::new(|| counter!("allocate_tcp_server"));
    pub static IDLE_IN_TCP_CONNECTION: LazyLock<Counter> =
        LazyLock::new(|| counter!("idle_in_tcp_connection"));
    pub static IDLE_TCP_SERVER: LazyLock<Counter> = LazyLock::new(|| counter!("idle_tcp_server"));
}

pub struct MetricService {
    app: Router,
}

impl MetricService {
    pub fn new() -> MetricService {
        let ph = MetricService::setup_metrics_recorder();
        let app = Router::new().route(
            &(*consts::METRICS_EXPOSE_PATH),
            get(move || ready(ph.render())),
        );
        MetricService { app }
    }

    pub async fn start(&mut self, port: u16) -> Result<(), anyhow::Error> {
        let listener =
            tokio::net::TcpListener::bind("0.0.0.0:".to_owned() + &port.to_string()).await?;
        match axum::serve(listener, self.app.clone()).await {
            Ok(_) => Ok(()),
            Err(err) => Err(anyhow::Error::from(err)),
        }
    }

    pub fn setup_metrics_recorder() -> PrometheusHandle {
        const EXPONENTIAL_SECONDS: &[f64] = &[
            0.00005, 0.0001, 0.00025, 0.0005, 0.001, 0.0025, 0.005, 0.01, 0.025, 0.05, 0.1,
        ];

        PrometheusBuilder::new()
            .set_buckets_for_metric(
                Matcher::Full("http_requests_duration_seconds".to_string()),
                EXPONENTIAL_SECONDS,
            )
            .unwrap()
            .install_recorder()
            .unwrap()
    }
}
