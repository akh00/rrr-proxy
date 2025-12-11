use metrics_exporter_prometheus::{Matcher, PrometheusBuilder, PrometheusHandle};

pub mod globals {
    use std::sync::LazyLock;

    use metrics::{Counter, counter};
    use metrics_exporter_prometheus::PrometheusHandle;

    use crate::manager::pmetrics;

    pub static PROMETHEUS_HANDLER: LazyLock<PrometheusHandle> =
        LazyLock::new(|| pmetrics::setup_metrics_recorder());
    pub static IN_UDP_SOCKET_COUNTER: LazyLock<Counter> =
        LazyLock::new(|| counter!("allocate_in_udp_socket"));
    pub static OUT_UDP_SOCKET_COUNTER: LazyLock<Counter> =
        LazyLock::new(|| counter!("allocate_out_udp_socket"));
    pub static TCP_CONNECTION_TO_SRV: LazyLock<Counter> =
        LazyLock::new(|| counter!("tcp_connections_to_ms"));
    pub static TCP_SOCKET_ALLOCATE: LazyLock<Counter> =
        LazyLock::new(|| counter!("allocate_tcp_server"));
}

pub(crate) fn setup_metrics_recorder() -> PrometheusHandle {
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
