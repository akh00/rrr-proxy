use std::{
    collections::HashMap,
    error::Error,
    fmt::Debug,
    net::{SocketAddr, ToSocketAddrs},
    time::Duration,
};

use tokio::{
    io,
    net::{TcpListener, TcpStream},
    select,
    sync::mpsc::{self, Sender},
    task::JoinHandle,
    time::sleep,
};
use tracing::{debug, error, info, instrument};

use crate::{consts, manager::pmetrics::globals};

use super::{Endpoint, ProxyClientMsg, ProxyManagerMsg};

#[derive(Debug)]
struct ProxyTcpClient {
    cl_stream: TcpStream,
    s_stream: TcpStream,
    endpoint: Endpoint,
    addr: SocketAddr,
    msg_tx: Sender<ProxyClientMsg>,
}

impl ProxyTcpClient {
    async fn new(
        stream: TcpStream,
        addr: SocketAddr,
        endpoint: Endpoint,
        msg_tx: Sender<ProxyClientMsg>,
    ) -> Result<Self, Box<dyn Error>> {
        // just in case resolving
        let remote_adr = endpoint
            .to_string()
            .to_socket_addrs()
            .unwrap()
            .next()
            .ok_or("Something went wrong")
            .unwrap();
        let s_stream = TcpStream::connect(remote_adr).await?;
        (*globals::TCP_CONNECTION_TO_SRV).increment(1);
        Ok(ProxyTcpClient {
            cl_stream: stream,
            s_stream,
            endpoint,
            addr,
            msg_tx,
        })
    }

    #[inline]
    async fn read_and_write(
        read_stream: &TcpStream,
        write_stream: &TcpStream,
        mut buf: &mut [u8],
    ) -> Result<(), Box<dyn Error>> {
        _ = read_stream.readable().await?;
        match read_stream.try_read(&mut buf) {
            Ok(0) => Err(Box::<dyn Error>::from("socket is closed")),
            Ok(len) => {
                Self::write_to_stream(write_stream, &buf[0..len]).await?;
                Ok(())
            }
            Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => Ok(()),
            Err(e) => {
                error!("could not read from tcp endpoint {:?}", e);
                Err(Box::<dyn Error>::from("socket is closed"))
            }
        }
    }

    #[inline]
    async fn write_to_stream(tcp_stream: &TcpStream, buf: &[u8]) -> Result<(), Box<dyn Error>> {
        let timeout = *consts::TRAFFIC_WAIT_TIMEOUT;
        loop {
            select! {
                _ = tcp_stream.writable() => {
                    match tcp_stream.try_write(buf) {
                        Ok(_) => break,
                        Err(_) => continue,
                    }

                },
                _ = sleep(Duration::from_millis(timeout)) => {
                    return Err(Box::<dyn Error>::from(
                        "Can not write to tcp stream, timeout",
                    ))
               }
            }
        }
        Ok(())
    }

    #[instrument(level = "debug")]
    pub async fn run(self) {
        let mut bufi = [0; 64 * 1024];
        let mut bufo = [0; 64 * 1024];
        let timeout = *consts::TRAFFIC_WAIT_TIMEOUT;
        loop {
            select! {
                a = Self::read_and_write(&self.cl_stream, &self.s_stream, &mut bufi) => if let Ok(_) = a { continue; } else { break; },
                b = Self::read_and_write(&self.s_stream, &self.cl_stream, &mut bufo) => if let Ok(_) = b { continue; } else { break; },
                _ = sleep(Duration::from_millis(timeout)) => {
                    info!("ProxyClient: No traffic for {:?} exiting", &self.endpoint);
                    break;
                }
            }
        }
        if let Err(err) = self
            .msg_tx
            .send(ProxyClientMsg::ClientEnded { addr: self.addr })
            .await
        {
            info!(
                "Reciever already dropped while sending end message {:?}",
                err
            );
        }
    }
}

#[derive(Debug)]
pub(crate) struct ProxyTcpEndpointHandler {
    endpoint_handle: JoinHandle<()>,
    pub local_port: u16,
}

impl ProxyTcpEndpointHandler {
    #[instrument(level = "debug")]
    pub async fn new(
        endpoint: Endpoint,
        msg_tx: Sender<ProxyManagerMsg>,
    ) -> Result<Self, Box<dyn Error>> {
        let listener = TcpListener::bind("0.0.0.0:0").await?;
        let port = listener.local_addr()?.port();
        let tcp_endpoint = ProxyTcpEndpoint::new(listener, msg_tx, endpoint.clone());
        let handle = tokio::spawn(async move {
            tcp_endpoint.run().await;
        });
        let tcp_handler = ProxyTcpEndpointHandler {
            endpoint_handle: handle,
            local_port: port,
        };
        Ok(tcp_handler)
    }

    pub async fn kill(&self) {
        self.endpoint_handle.abort();
    }
}

#[derive(Debug)]
struct ProxyTcpEndpoint {
    //agent
    tcp_listener: TcpListener,
    msg_tx: Sender<ProxyManagerMsg>,
    endpoint: Endpoint,
    clients: HashMap<SocketAddr, JoinHandle<()>>,
}

impl ProxyTcpEndpoint {
    fn new(tcp_listener: TcpListener, msg_tx: Sender<ProxyManagerMsg>, endpoint: Endpoint) -> Self {
        (*globals::TCP_SOCKET_ALLOCATE).increment(1);
        let clients = HashMap::new();
        ProxyTcpEndpoint {
            tcp_listener,
            msg_tx,
            endpoint,
            clients,
        }
    }
    async fn run(mut self) {
        let timeout = *consts::TRAFFIC_WAIT_TIMEOUT;
        let (msg_tx, mut rx) = mpsc::channel::<ProxyClientMsg>(1000);
        loop {
            select! {
                a = self.tcp_listener.accept() => {
                   if let Ok((stream, addr)) = a {
                            if let Ok(client) = ProxyTcpClient::new(stream, addr, self.endpoint.clone(), msg_tx.clone()).await {
                                let handler = tokio::spawn( async move {
                                    client.run().await;
                            });
                            self.clients.insert(addr, handler);
                        }
                    } else {
                        debug!("Error recieven from socket");
                        break;
                    }
               },
               b = rx.recv() => {
                   if let Some(msg) = b {
                        match msg {
                            ProxyClientMsg::ClientEnded { addr } => {
                                self.clients.remove(&addr); //removing one of the accepted tcp connections
                                if self.clients.len() == 0 {
                                    info!("No clients left for the given endpoint {:?}", &self.endpoint);
                                    break;
                                }
                            }
                        }
                    } else {
                        error!("Something wrong ");
                        break;
                    }
               },
               _ = sleep(Duration::from_millis(timeout)) => {
                    info!("No traffic on ProxyEndpoint {:?}, exiting", &self.endpoint);
                    break;
               }
            };
        }
        if let Err(err) = self
            .msg_tx
            .send(ProxyManagerMsg::TcpEndpointEnded {
                endpoint: self.endpoint,
            })
            .await
        {
            info!(
                "Reciever already dropped while sending end message {:?}",
                err
            );
        }
        ()
    }
}
