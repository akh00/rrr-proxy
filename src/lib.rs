pub mod ebpf;
pub mod proxy;
pub mod signaling;

pub use proxy::*;
pub use signaling::endpoint::*;

#[cfg(test)]
mod integration_tests {
    use std::net::UdpSocket;
    use std::str;
    use std::sync::Arc;
    use std::time::Duration;

    use axum::http::StatusCode;
    use axum_test::TestServer;
    use futures::future::join_all;
    use serde_json::{Value, json};
    use tokio::sync::RwLock;

    use crate::{AllocatorService, ProxyManager, ProxyManagerShared};

    fn init() {
        let _ = env_logger::builder()
            .target(env_logger::Target::Stdout)
            .filter_level(log::LevelFilter::Trace)
            .is_test(true)
            .try_init();
    }

    fn client_recieve(
        reciever: UdpSocket,
        msg: &str,
        should_fail: bool,
    ) -> impl Future<Output = ()> {
        async move {
            let mut buf = [0; 64 * 1024];
            loop {
                match reciever.recv_from(&mut buf) {
                    Ok((len, addr_from)) => {
                        assert!(len > 0, "Error len={:?}", len);
                        assert!(
                            str::from_utf8(&buf[..len]).unwrap().starts_with(msg),
                            "not pong from server"
                        );
                        println!("Buffer recieved on  client from: {}", addr_from);
                        std::thread::sleep(Duration::from_millis(10));
                        break;
                    }
                    Err(error) => {
                        if should_fail {
                            assert!(true, "error happened {:?}", error);
                        } else {
                            assert!(false, "error happened {:?}", error);
                        }
                        break;
                    }
                }
            }
        }
    }

    fn server_endpoint(socket: UdpSocket, msg: &str) -> impl Future<Output = ()> {
        async move {
            let mut buf = [0; 64 * 1024];
            loop {
                match socket.recv_from(&mut buf) {
                    Ok((len, addr)) => {
                        assert!(len > 0, "Error len={:?}", len);
                        assert!(
                            str::from_utf8(&buf[..len]).unwrap().starts_with(msg),
                            "not ping"
                        );
                        println!(
                            "Buffer recieved {:?} from: {:?}",
                            str::from_utf8(&buf[..len]).unwrap(),
                            addr
                        );
                        socket
                            .send_to(std::format!("pong to {}", addr).as_bytes(), addr)
                            .unwrap();
                        std::thread::sleep(Duration::from_millis(10));
                        break;
                    }
                    Err(error) => {
                        assert!(false, "error happened {:?}", error);
                        break;
                    }
                }
            }
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 10)]
    async fn it_should_pass() {
        init();
        let proxy_manager = Arc::new(RwLock::<ProxyManager>::new(ProxyManager::new()));

        let singnaling_app = AllocatorService::new(Arc::clone(&proxy_manager));
        let server = TestServer::new(singnaling_app.app).expect("Should create test server");

        let mut tasks = Vec::new();
        let mut upd_port = 0;

        if let Ok(server_udp_socket) = UdpSocket::bind("0.0.0.0:0") {
            upd_port = server_udp_socket.local_addr().unwrap().port();
            tasks.push(tokio::spawn(server_endpoint(server_udp_socket, "ping")));
        } else {
            assert!(false, "Can not bind socket");
        }

        let resp = server
            .post(&"/api/v1/allocate")
            .add_header("content-type", "application/json")
            .json(&json!(
               {
                "targetIp": "127.0.0.1",
                "targetUdpPort" : upd_port, "targetTcpPort": 23456, "targetSslTcpPort": 23456
               }
            ))
            .await;

        let raw_text = resp.text();
        let parsed: Value = serde_json::from_str(&raw_text).unwrap();
        let response = parsed.as_object().unwrap();
        let local_port = response
            .get("proxyUdpPort")
            .unwrap()
            .as_number()
            .unwrap()
            .as_i64()
            .unwrap();
        let client_udp_socket =
            UdpSocket::bind("0.0.0.0:0").expect("should be able to bind udp socket");
        let client_reciever = client_udp_socket
            .try_clone()
            .expect("should be able to clone udp socket");
        client_udp_socket
            .connect(std::format!("127.0.0.1:{}", local_port))
            .expect("should be able to connect to proxy socketudp socket");
        tasks.push(tokio::spawn(async move {
            loop {
                match client_udp_socket.send("ping from client".as_bytes()) {
                    Ok(len) => {
                        assert!(len > 0, "Error len={:?}", len);
                        println!("Buffer send from client from: {}", local_port);
                        std::thread::sleep(Duration::from_millis(10));
                        break;
                    }
                    Err(error) => {
                        assert!(false, "error happened {:?}", error);
                        break;
                    }
                }
            }
        }));
        tasks.push(tokio::spawn(client_recieve(client_reciever, "pong", false)));
        join_all(tasks).await;
        let resp = server
            .delete(&format!("/api/v1/delete/{}", local_port))
            .await;
        resp.assert_status(StatusCode::OK);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 10)]
    async fn timeout_test() {
        init();
        let proxy_manager = Arc::new(RwLock::<ProxyManager>::new(ProxyManager::new()));

        let singnaling_app = AllocatorService::new(Arc::clone(&proxy_manager));
        let server = TestServer::new(singnaling_app.app).expect("Should create test server");

        let mut tasks = Vec::new();
        let mut upd_port = 0;

        if let Ok(server_udp_socket) = UdpSocket::bind("0.0.0.0:0") {
            upd_port = server_udp_socket.local_addr().unwrap().port();
            tasks.push(tokio::spawn(server_endpoint(server_udp_socket, "ping")));
        } else {
            assert!(false, "Can not bind socket");
        }

        let resp = server
            .post(&"/api/v1/allocate")
            .add_header("content-type", "application/json")
            .json(&json!(
               {
                "targetIp": "127.0.0.1",
                "targetUdpPort" : upd_port, "targetTcpPort": 23456, "targetSslTcpPort": 23456
               }
            ))
            .await;

        let raw_text = resp.text();
        let parsed: Value = serde_json::from_str(&raw_text).unwrap();
        let response = parsed.as_object().unwrap();
        let local_port = response
            .get("proxyUdpPort")
            .unwrap()
            .as_number()
            .unwrap()
            .as_i64()
            .unwrap();
        let client_udp_socket =
            UdpSocket::bind("0.0.0.0:0").expect("should be able to bind udp socket");
        let client_reciever = client_udp_socket
            .try_clone()
            .expect("should be able to clone udp socket");
        client_udp_socket
            .connect(std::format!("127.0.0.1:{}", local_port))
            .expect("should be able to connect to proxy socketudp socket");
        tasks.push(tokio::spawn(async move {
            loop {
                std::thread::sleep(Duration::from_millis(10000));
                match client_udp_socket.send("ping from client".as_bytes()) {
                    Ok(len) => {
                        assert!(len > 0, "Error len={:?}", len);
                        println!("Buffer send from client from: {}", local_port);
                        std::thread::sleep(Duration::from_millis(10));
                        break;
                    }
                    Err(error) => {
                        assert!(false, "error happened {:?}", error);
                        break;
                    }
                }
            }
        }));
        tasks.push(tokio::spawn(client_recieve(client_reciever, "pong", true)));
        join_all(tasks).await;
        let resp = server
            .delete(&format!("/api/v1/delete/{}", local_port))
            .await;
        resp.assert_status(StatusCode::OK);
    }
}
