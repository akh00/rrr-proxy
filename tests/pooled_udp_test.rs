#[cfg(test)]
mod pooled_udp_tests {
    use std::sync::Arc;
    use std::time::Duration;

    use axum::http::StatusCode;
    use axum_test::TestServer;
    use serde_json::json;
    use tokio::net::UdpSocket;
    use tokio::sync::RwLock;
    use tokio::task::JoinHandle;
    use tokio::time::timeout;

    use rrr_proxy::manager::{endpoint::AllocatorService, models::AllocateResponse};
    use rrr_proxy::proxy::{ProxyManager, UdpProxyMode};

    const REPLY_WAIT: Duration = Duration::from_millis(1000);
    const SILENCE_WAIT: Duration = Duration::from_millis(500);

    fn server(manager: ProxyManager) -> TestServer {
        let app = AllocatorService::new(Arc::new(RwLock::new(manager))).app;
        TestServer::new(app)
    }

    fn binding_request(ufrag: &str) -> Vec<u8> {
        let username = format!("{}:remote", ufrag).into_bytes();
        let padded_len = (username.len() + 3) & !3;
        let attrs_len = 4 + padded_len;
        let mut packet = Vec::with_capacity(20 + attrs_len);
        packet.extend_from_slice(&0x0001u16.to_be_bytes());
        packet.extend_from_slice(&(attrs_len as u16).to_be_bytes());
        packet.extend_from_slice(&[0x21, 0x12, 0xA4, 0x42]);
        packet.extend_from_slice(&[7; 12]);
        packet.extend_from_slice(&0x0006u16.to_be_bytes());
        packet.extend_from_slice(&(username.len() as u16).to_be_bytes());
        packet.extend_from_slice(&username);
        packet.resize(20 + attrs_len, 0);
        packet
    }

    // a media server stand-in answering every datagram with its own tag
    async fn tagged_target(tag: &'static str) -> (u16, JoinHandle<()>) {
        let socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let port = socket.local_addr().unwrap().port();
        let handle = tokio::spawn(async move {
            let mut buf = [0; 4 * 1024];
            while let Ok((_, addr)) = socket.recv_from(&mut buf).await {
                let _ = socket.send_to(tag.as_bytes(), addr).await;
            }
        });
        (port, handle)
    }

    async fn free_udp_port() -> u16 {
        let socket = UdpSocket::bind("0.0.0.0:0").await.unwrap();
        socket.local_addr().unwrap().port()
    }

    async fn client() -> UdpSocket {
        UdpSocket::bind("127.0.0.1:0").await.unwrap()
    }

    async fn exchange(
        client: &UdpSocket,
        proxy_port: u16,
        data: &[u8],
        wait: Duration,
    ) -> Option<String> {
        client
            .send_to(data, ("127.0.0.1", proxy_port))
            .await
            .unwrap();
        let mut buf = [0; 4 * 1024];
        match timeout(wait, client.recv_from(&mut buf)).await {
            Ok(Ok((len, _))) => Some(String::from_utf8_lossy(&buf[..len]).into_owned()),
            _ => None,
        }
    }

    async fn allocate(
        server: &TestServer,
        target_port: u16,
        session: &str,
        ufrag: Option<&str>,
    ) -> axum_test::TestResponse {
        let mut body = json!({
            "targetIp": "127.0.0.1",
            "targetUdpPort": target_port.to_string(),
            "targetTcpPort": "0",
            "targetSslTcpPort": "0",
            "sessionId": session,
        });
        if let Some(ufrag) = ufrag {
            body["ufrag"] = json!(ufrag);
        }
        server
            .post("/api/v1/proxy-ports")
            .add_header("content-type", "application/json")
            .json(&body)
            .await
    }

    async fn allocate_port(
        server: &TestServer,
        target_port: u16,
        session: &str,
        ufrag: Option<&str>,
    ) -> u16 {
        let resp = allocate(server, target_port, session, ufrag).await;
        resp.assert_status(StatusCode::OK);
        serde_json::from_str::<AllocateResponse>(&resp.text())
            .expect("AllocateResponse should be parsed")
            .proxy_udp_port
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn sessions_share_one_port_and_route_by_ufrag() {
        let server = server(ProxyManager::with_udp_pool(1, 0).unwrap());
        let (target1, _t1) = tagged_target("target-1").await;
        let (target2, _t2) = tagged_target("target-2").await;

        let port1 = allocate_port(&server, target1, "session-1", Some("ufrag1")).await;
        let port2 = allocate_port(&server, target2, "session-2", Some("ufrag2")).await;
        assert_eq!(port1, port2, "pooled mode must share the inbound port");

        let (client1, client2) = (client().await, client().await);
        assert_eq!(
            exchange(&client1, port1, &binding_request("ufrag1"), REPLY_WAIT)
                .await
                .as_deref(),
            Some("target-1")
        );
        assert_eq!(
            exchange(&client2, port1, &binding_request("ufrag2"), REPLY_WAIT)
                .await
                .as_deref(),
            Some("target-2")
        );
        // once a flow is open, non-STUN media follows the client address
        assert_eq!(
            exchange(&client1, port1, b"media-1", REPLY_WAIT)
                .await
                .as_deref(),
            Some("target-1")
        );
        assert_eq!(
            exchange(&client2, port1, b"media-2", REPLY_WAIT)
                .await
                .as_deref(),
            Some("target-2")
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn unroutable_datagrams_are_dropped() {
        let server = server(ProxyManager::with_udp_pool(1, 0).unwrap());
        let (target, _t) = tagged_target("target").await;
        let port = allocate_port(&server, target, "session", Some("ufrag")).await;

        let stranger = client().await;
        assert_eq!(
            exchange(&stranger, port, b"media", SILENCE_WAIT).await,
            None
        );
        assert_eq!(
            exchange(&stranger, port, &binding_request("unknown"), SILENCE_WAIT).await,
            None
        );
        // the same client is routed once it presents a known ufrag
        assert_eq!(
            exchange(&stranger, port, &binding_request("ufrag"), REPLY_WAIT)
                .await
                .as_deref(),
            Some("target")
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn pooled_allocation_requires_ufrag() {
        let server = server(ProxyManager::with_udp_pool(1, 0).unwrap());
        let (target, _t) = tagged_target("target").await;
        allocate(&server, target, "no-ufrag", None)
            .await
            .assert_status(StatusCode::NOT_ACCEPTABLE);
        allocate(&server, target, "empty-ufrag", Some(""))
            .await
            .assert_status(StatusCode::NOT_ACCEPTABLE);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn delete_ends_only_its_session() {
        let server = server(ProxyManager::with_udp_pool(1, 0).unwrap());
        let (target1, _t1) = tagged_target("target-1").await;
        let (target2, _t2) = tagged_target("target-2").await;
        let port = allocate_port(&server, target1, "session-1", Some("ufrag1")).await;
        allocate_port(&server, target2, "session-2", Some("ufrag2")).await;

        let (client1, client2) = (client().await, client().await);
        assert!(
            exchange(&client1, port, &binding_request("ufrag1"), REPLY_WAIT)
                .await
                .is_some()
        );
        assert!(
            exchange(&client2, port, &binding_request("ufrag2"), REPLY_WAIT)
                .await
                .is_some()
        );

        server
            .delete(&format!("/api/v1/proxy-ports/{}/session-1", port))
            .await
            .assert_status(StatusCode::OK);

        // the deleted session's flow and route are gone
        assert_eq!(
            exchange(&client1, port, b"media-1", SILENCE_WAIT).await,
            None
        );
        assert_eq!(
            exchange(&client1, port, &binding_request("ufrag1"), SILENCE_WAIT).await,
            None
        );
        // the shared port keeps serving the other session
        assert_eq!(
            exchange(&client2, port, b"media-2", REPLY_WAIT)
                .await
                .as_deref(),
            Some("target-2")
        );
        server
            .delete(&format!("/api/v1/proxy-ports/{}/session-1", port))
            .await
            .assert_status(StatusCode::BAD_REQUEST);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn shared_ufrag_survives_until_last_session_ends() {
        let server = server(ProxyManager::with_udp_pool(1, 0).unwrap());
        let (target, _t) = tagged_target("target").await;
        let port = allocate_port(&server, target, "session-1", Some("ufrag")).await;
        allocate_port(&server, target, "session-2", Some("ufrag")).await;

        server
            .delete(&format!("/api/v1/proxy-ports/{}/session-1", port))
            .await
            .assert_status(StatusCode::OK);
        let client = client().await;
        assert_eq!(
            exchange(&client, port, &binding_request("ufrag"), REPLY_WAIT)
                .await
                .as_deref(),
            Some("target")
        );

        server
            .delete(&format!("/api/v1/proxy-ports/{}/session-2", port))
            .await
            .assert_status(StatusCode::OK);
        assert_eq!(exchange(&client, port, b"media", SILENCE_WAIT).await, None);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn sessions_are_spread_over_the_pool() {
        let manager = ProxyManager::with_udp_pool(2, 0).unwrap();
        let pooled = manager.pooled_udp_ports();
        assert_eq!(pooled.len(), 2);
        let server = server(manager);
        let (target, _t) = tagged_target("target").await;

        let first = allocate_port(&server, target, "s1", Some("u1")).await;
        let second = allocate_port(&server, target, "s2", Some("u2")).await;
        let third = allocate_port(&server, target, "s3", Some("u3")).await;
        assert_ne!(first, second);
        assert_eq!(first, third);
        assert!(pooled.contains(&first) && pooled.contains(&second));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn pool_binds_from_base_port() {
        let base = free_udp_port().await;
        let manager = ProxyManager::with_udp_pool(1, base).unwrap();
        assert_eq!(manager.udp_mode(), UdpProxyMode::Pooled);
        assert_eq!(manager.pooled_udp_ports(), vec![base]);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn ephemeral_pool_ports_are_distinct() {
        let manager = ProxyManager::with_udp_pool(1000, 0).unwrap();
        let mut ports = manager.pooled_udp_ports();
        ports.sort_unstable();
        ports.dedup();
        assert_eq!(
            ports.len(),
            1000,
            "pooled sockets must not share an ephemeral port"
        );
    }

    // a fixed pooled port is shared with the next instance during a rolling restart
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn fixed_pool_port_can_be_rebound() {
        let base = free_udp_port().await;
        let current = ProxyManager::with_udp_pool(1, base).unwrap();
        let next = ProxyManager::with_udp_pool(1, base).unwrap();
        assert_eq!(current.pooled_udp_ports(), next.pooled_udp_ports());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn pool_fails_when_no_port_binds() {
        let taken = UdpSocket::bind("0.0.0.0:0").await.unwrap();
        let port = taken.local_addr().unwrap().port();
        assert!(ProxyManager::with_udp_pool(1, port).is_err());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn pool_size_zero_keeps_per_session_ports() {
        let manager = ProxyManager::with_udp_pool(0, 0).unwrap();
        assert_eq!(manager.udp_mode(), UdpProxyMode::PerSession);
        assert!(manager.pooled_udp_ports().is_empty());
        let server = server(manager);
        let (target, _t) = tagged_target("target").await;

        // ufrag is optional and ignored, plain media is forwarded without STUN
        let port1 = allocate_port(&server, target, "s1", None).await;
        let port2 = allocate_port(&server, target, "s2", Some("ignored")).await;
        assert_ne!(port1, port2);
        let client = client().await;
        assert_eq!(
            exchange(&client, port2, b"media", REPLY_WAIT)
                .await
                .as_deref(),
            Some("target")
        );
    }
}
