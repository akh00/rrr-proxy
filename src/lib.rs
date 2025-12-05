pub mod ebpf;
pub mod manager;
pub mod proxy;

pub use manager::endpoint::*;
pub use proxy::*;

pub mod consts {
    use std::env::var;
    use std::sync::LazyLock;

    use uuid::Uuid;

    pub static REGISTER_ENDPOINT: LazyLock<String> =
        LazyLock::new(|| match var("REGISTER_ENDPOINT") {
            Ok(reg_endpoint) => reg_endpoint,
            Err(_) => "ws://localhost:5555".to_string(),
        });
    pub static TRAFFIC_WAIT_TIMEOUT: LazyLock<u64> =
        LazyLock::new(|| match var("TRAFFIC_WAIT_TIMEOUT") {
            Ok(wait_timeout) => wait_timeout.parse::<u64>().unwrap(),
            Err(_) => 5000, // default
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
}

#[cfg(test)]
mod integration_tests {
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::Arc;
    use std::time::Duration;
    use std::{net::UdpSocket, str};

    use axum::http::StatusCode;
    use axum_test::{TestResponse, TestServer};
    use futures::future::join_all;
    use log::info;
    use once_cell::sync::OnceCell;
    use serde_json::json;
    use tokio::sync::RwLock;
    use tungstenite::Message;
    use ws_mock::matchers::Any;
    use ws_mock::ws_mock_server::{WsMock, WsMockServer};

    use crate::manager::load::ReportLoadSysProvider;
    use crate::manager::models::AllocateResponse;
    use crate::manager::register::RegisterAgent;
    use crate::{AllocatorService, ProxyManager};

    static MOCK_SERVER_REF: OnceCell<WsMockServer> = OnceCell::new();

    async fn async_init() -> &'static WsMockServer {
        if let None = MOCK_SERVER_REF.get() {
            let server = WsMockServer::start().await;
            let _ = MOCK_SERVER_REF.set(server);
        }
        MOCK_SERVER_REF.get().unwrap()
    }

    fn init() {
        let _ = env_logger::builder()
            .target(env_logger::Target::Stdout)
            .filter_level(log::LevelFilter::Trace)
            .is_test(true)
            .try_init();
    }

    fn client_send(sender: UdpSocket, msg: &str, sleep_for: u64) -> impl Future<Output = ()> {
        async move {
            loop {
                std::thread::sleep(Duration::from_millis(if sleep_for == 0 {
                    1
                } else {
                    sleep_for
                }));
                match sender.send(msg.as_bytes()) {
                    Ok(len) => {
                        assert!(len > 0, "Error len={:?}", len);
                        println!(
                            "Buffer send from client from: {}",
                            sender.local_addr().unwrap().port()
                        );
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
                        assert!(!should_fail, "error not happened!");
                        break;
                    }
                    Err(error) => {
                        assert!(should_fail, "error happened {:?}", error);
                        break;
                    }
                }
            }
        }
    }

    fn server_endpoint(
        socket: UdpSocket,
        msg: &str,
        timeout: u64,
        should_fail: bool,
    ) -> impl Future<Output = ()> {
        async move {
            let mut buf = [0; 64 * 1024];
            if timeout != 0 {
                socket
                    .set_read_timeout(Some(Duration::from_millis(timeout)))
                    .expect("Timeout should be set");
            }
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
                        assert!(should_fail, "error happened: {:?} exiting", error);
                        break;
                    }
                }
            }
        }
    }

    fn allocate_tcp_client_sockets(
        resp: &TestResponse,
    ) -> (TcpStream, TcpStream, AllocateResponse) {
        let parsed: AllocateResponse =
            serde_json::from_str(&resp.text()).expect("AllocateResponse shouldbe parsed");
        let local_port = parsed.proxy_tcp_port;
        let client_tcp_socket = TcpStream::connect(std::format!("127.0.0.1:{}", local_port))
            .expect("should be able to connect to tcp socket");
        let client_reciever = client_tcp_socket
            .try_clone()
            .expect("should be able to clone tcp socket");
        (client_reciever, client_tcp_socket, parsed)
    }
    fn allocate_udp_client_sockets(
        resp: &TestResponse,
    ) -> (UdpSocket, UdpSocket, AllocateResponse) {
        let parsed: AllocateResponse =
            serde_json::from_str(&resp.text()).expect("AllocateResponse should be parsed");
        let local_port = parsed.proxy_udp_port;
        let client_udp_socket =
            UdpSocket::bind("0.0.0.0:0").expect("should be able to bind udp socket");
        let client_reciever = client_udp_socket
            .try_clone()
            .expect("should be able to clone udp socket");
        client_udp_socket
            .connect(std::format!("127.0.0.1:{}", local_port))
            .expect("should be able to connect to proxy socketudp socket");
        (client_reciever, client_udp_socket, parsed)
    }

    fn server_tcp_endpoint(
        std_listener: TcpListener,
        msg: &str,
        timeout: u64,
        should_fail: bool,
    ) -> impl Future<Output = ()> {
        async move {
            let mut buf = [0; 64 * 1024];
            let _ = std_listener.set_nonblocking(true);
            let listener = tokio::net::TcpListener::from_std(std_listener)
                .expect("Should be possible to convert");

            match tokio::time::timeout(Duration::from_millis(5000), listener.accept()).await {
                Ok(timeout_res) => {
                    if let Ok((stream, addr)) = timeout_res {
                        if let Ok(mut socket) = stream.into_std() {
                            if timeout != 0 {
                                let _ =
                                    socket.set_read_timeout(Some(Duration::from_millis(timeout)));
                            }
                            let _ = socket.set_nonblocking(false);
                            let mut socket_write = socket.try_clone().unwrap();
                            loop {
                                match socket.read(&mut buf) {
                                    Ok(len) => {
                                        assert!(len > 0, "Error len={:?}", len);
                                        assert!(
                                            str::from_utf8(&buf[..len]).unwrap().starts_with(msg),
                                            "not ping"
                                        );
                                        socket_write
                                            .write(std::format!("pong to {}", addr).as_bytes())
                                            .unwrap();
                                        info!("writing back to tcp connection");
                                        std::thread::sleep(Duration::from_millis(1000));
                                        break;
                                    }
                                    Err(error) => {
                                        assert!(should_fail, "error happened: {:?} exiting", error);
                                        break;
                                    }
                                }
                            }
                        }
                    }
                }
                Err(timeout_err) => {
                    assert!(false, "error during accepting socket {:?}", timeout_err);
                }
            }
            info!("exiting from tcp server endpoint");
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 10)]
    async fn reporting_shold_work() {
        init();
        let mock_server = async_init().await;
        let load_reporter = Arc::new(ReportLoadSysProvider::new());
        let register_agent = RegisterAgent::new(mock_server.uri().await, load_reporter);
        let mock = WsMock::new()
            // .matcher(StringContains::new("percent"))
            .matcher(Any::new())
            .respond_with(Message::Text("Pong".into()))
            .expect(2)
            .mount(mock_server);

        tokio::select! {
            _ = tokio::spawn(async {tokio::join!(
            mock,
            register_agent.run())}) => {},
           _ = tokio::time::sleep(Duration::from_millis(6000)) => {}
        }

        MOCK_SERVER_REF.get().unwrap().verify().await
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 10)]
    async fn it_should_pass() {
        init();
        let proxy_manager = Arc::new(RwLock::<ProxyManager>::new(ProxyManager::new()));

        let manager_app = AllocatorService::new(Arc::clone(&proxy_manager));
        let server = TestServer::new(manager_app.app).expect("Should create test server");

        let mut tasks = Vec::new();
        let mut upd_port = 0;
        let mut tcp_port = 0;

        if let Ok(server_tcp_socket) = TcpListener::bind("0.0.0.0:0") {
            tcp_port = server_tcp_socket.local_addr().unwrap().port();
            tasks.push(tokio::spawn(server_tcp_endpoint(
                server_tcp_socket,
                "tcp::ping",
                0,
                false,
            )));
        } else {
            assert!(false, "Can not bind socket");
        }
        if let Ok(server_udp_socket) = UdpSocket::bind("0.0.0.0:0") {
            upd_port = server_udp_socket.local_addr().unwrap().port();
            tasks.push(tokio::spawn(server_endpoint(
                server_udp_socket,
                "ping",
                0,
                false,
            )));
        } else {
            assert!(false, "Can not bind socket");
        }

        let resp = server
            .post(&"/api/v1/proxy-ports")
            .add_header("content-type", "application/json")
            .json(&json!(
               {
                "targetIp": "127.0.0.1",
                "targetUdpPort" : upd_port.to_string(), "targetTcpPort": tcp_port.to_string(), "targetSslTcpPort": 23456.to_string(),
                "privateIp": "127.0.0.1", "sessionId": "deadbeaf"
               }
            ))
            .await;
        let (client_reciever, client_udp_socket, _allocate_resp) =
            allocate_udp_client_sockets(&resp);
        tasks.push(tokio::spawn(client_send(
            client_udp_socket,
            &"ping from client",
            0,
        )));
        tasks.push(tokio::spawn(client_recieve(client_reciever, "pong", false)));
        let (mut client_tcp_sender, mut client_tcp_reader, allocate_resp) =
            allocate_tcp_client_sockets(&resp);
        tasks.push(tokio::spawn(async move {
            info!("sending tcp ping....");
            assert!(
                client_tcp_sender.write("tcp::ping".as_bytes()).is_ok(),
                "should be able to write"
            );
            info!("sent tcp ping....");
            let mut tcp_buf: [u8; 256] = [0; 256];
            info!("reading from tcp socket");
            let res = client_tcp_reader.read(&mut tcp_buf);
            info!("read from tcp socket");
            assert!(res.is_ok(), "should be able to read");
            assert!(
                str::from_utf8(&tcp_buf[..res.unwrap()])
                    .unwrap()
                    .starts_with("pong"),
                "should start from pong"
            );
        }));
        join_all(tasks).await;
        info!("all tasks are finished");

        let resp = server
            .delete(&format!(
                "/api/v1/proxy-ports/{}/sesionid",
                allocate_resp.proxy_udp_port
            ))
            .await;
        resp.assert_status(StatusCode::OK);
        info!("deleted udp endpoint");
        let resp = server
            .delete(&format!(
                "/api/v1/proxy-ports/{}/sessionid",
                allocate_resp.proxy_tcp_port
            ))
            .await;
        info!("deleted tcp endpoint");
        resp.assert_status(StatusCode::OK)
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 10)]
    async fn timeout_test() {
        init();
        let proxy_manager = Arc::new(RwLock::<ProxyManager>::new(ProxyManager::new()));

        let manager_app = AllocatorService::new(Arc::clone(&proxy_manager));
        let server = TestServer::new(manager_app.app).expect("Should create test server");

        let mut tasks = Vec::new();
        let mut upd_port = 0;

        if let Ok(server_udp_socket) = UdpSocket::bind("0.0.0.0:0") {
            upd_port = server_udp_socket.local_addr().unwrap().port();
            tasks.push(tokio::spawn(server_endpoint(
                server_udp_socket,
                "ping",
                10000,
                true,
            )));
        } else {
            assert!(false, "Can not bind socket");
        }

        let resp = server
            .post(&"/api/v1/proxy-ports")
            .add_header("content-type", "application/json")
            .json(&json!(
               {
                "targetIp": "127.0.0.1",
                "targetUdpPort" : upd_port.to_string(), "targetTcpPort": 2345.to_string(), "targetSslTcpPort": 23456.to_string(),
                "privateIp": "127.0.0.1", "sessionId": "deadbeaf"
               }
            ))
            .await;

        let (client_reciever, client_udp_socket, allocate_resp) =
            allocate_udp_client_sockets(&resp);
        tasks.push(tokio::spawn(client_send(
            client_udp_socket,
            &"ping from client",
            6000,
        )));
        tasks.push(tokio::spawn(client_recieve(client_reciever, "pong", true)));
        join_all(tasks).await.iter().for_each(|res| {
            if res.is_ok() {
                assert!(true);
            } else {
                assert!(false)
            }
        });
        let resp = server
            .delete(&format!(
                "/api/v1/proxy-ports/{}/sessionid",
                allocate_resp.proxy_udp_port
            ))
            .await;
        resp.assert_status(StatusCode::OK);
    }
}
