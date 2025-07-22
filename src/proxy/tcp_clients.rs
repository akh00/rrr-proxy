use std::{collections::HashMap, error::Error, fmt::Debug, net::SocketAddr, time::Duration};

use tokio::{
    io,
    net::{TcpListener, TcpStream},
    select,
    sync::mpsc::{self, Sender},
    task::JoinHandle,
    time::sleep,
};
use tracing::{debug, error, info, instrument};

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
        let s_stream = TcpStream::connect(endpoint.to_string()).await?;
        Ok(ProxyTcpClient {
            cl_stream: stream,
            s_stream,
            endpoint,
            addr,
            msg_tx,
        })
    }

    #[instrument(level = "debug")]
    pub async fn run(self) {
        let mut buf = [0; 64 * 1024];
        loop {
            select! {
                _ = self.cl_stream.readable() => {
                    match self.cl_stream.try_read(&mut buf) {
                        Ok(0) => break,
                        Ok(len) => {
                        // Wait for the socket to be writable
                        if let Ok(_) = self.s_stream.writable().await {
                            if let Err(err) = self.s_stream.try_write(&buf[0..len]) {
                                error!("could not write to tcp endpoint {:?}", err);
                               }
                            }
                        },
                        Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => {
                            continue;
                        },
                        Err(e) => {
                            error!("could not read from tcp endpoint {:?}", e);
                            break;
                        }
                    }
                },

               _ = self.s_stream.readable() => {
                match self.s_stream.try_read(&mut buf) {
                    Ok(0) => break,
                    Ok(len) => {
                    // Wait for the socket to be writable
                    if let Ok(_) = self.cl_stream.writable().await {
                        if let Err(err) = self.cl_stream.try_write(&buf[0..len]) {
                            error!("could not write to tcp endpoint {:?}", err);
                           }
                        }
                    },
                    Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => {
                        continue;
                    }
                    Err(e) => {
                        error!("could not read from tcp endpoint {:?}", e);
                        break;
                    }
                }

               },
               _ = sleep(Duration::from_millis(5000)) => {
                    info!("ProxyClient: No traffic for {:?} exiting", self.endpoint);
                    break;
               }
            }
        }
        if let Err(err) = self
            .msg_tx
            .send(ProxyClientMsg::ClientEnded { addr: self.addr })
            .await
        {
            error!("Error happened during sending end message {:?}", err);
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
        let clients = HashMap::new();
        ProxyTcpEndpoint {
            tcp_listener,
            msg_tx,
            endpoint,
            clients,
        }
    }
    async fn run(mut self) {
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
                                    info!("No clients left for the given endpoint {:?}", self.endpoint);
                                    break;
                                }
                            }
                        }
                    } else {
                        error!("Something wrong ");
                        break;
                    }
               },
               _ = sleep(Duration::from_millis(5000)) => {
                    info!("No traffic on ProxyEndpoint {:?}, exiting", self.endpoint);
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
            error!("Error happened during sending end message {:?}", err);
        }
        ()
    }
}
