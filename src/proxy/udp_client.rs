use rustc_hash::FxHashMap;
use std::{
    collections::hash_map::Entry,
    error::Error,
    net::{SocketAddr, ToSocketAddrs},
    time::Duration,
};
use tokio::{
    net::UdpSocket,
    select,
    sync::mpsc::{self, Receiver, Sender},
    task::JoinHandle,
    time::sleep,
};
use tracing::{debug, error, info, trace};

use super::{Endpoint, ProxyClientMsg, ProxyManagerMsg};
use crate::manager::pmetrics;
use crate::proxy::FixBoxError;
use crate::{consts, manager::pmetrics::globals};
//
//  agent pattern implementation
//
//
//

#[derive(Debug)]
struct ProxyEndpoint {
    //agent
    udp_socket: UdpSocket,
    rx: Receiver<(Vec<u8>, SocketAddr)>, // reciever from ms faced clients
    tx: Sender<(Vec<u8>, SocketAddr)>,   //sender to routeer
    msg_tx: Sender<ProxyManagerMsg>,
    endpoint: Endpoint,
}

impl ProxyEndpoint {
    fn new(
        udp_socket: UdpSocket,
        tx: Sender<(Vec<u8>, SocketAddr)>,
        rx: Receiver<(Vec<u8>, SocketAddr)>,
        msg_tx: Sender<ProxyManagerMsg>,
        endpoint: Endpoint,
    ) -> Self {
        (*pmetrics::globals::IN_UDP_SOCKET_COUNTER).increment(1);
        ProxyEndpoint {
            udp_socket,
            rx,
            tx,
            msg_tx,
            endpoint,
        }
    }

    #[inline]
    async fn run(mut self) {
        let timeout = *consts::TRAFFIC_WAIT_TIMEOUT;
        let mut buf = [0; 4 * 1024];
        loop {
            select! {
                a = self.udp_socket.recv_from(&mut buf) => {
                   if let Ok((len, addr )) = a {
                    trace!("{:?} bytes received from {:?}", len, addr);
                    // to the router
                        match self.tx.send((buf[..len].to_vec(), addr)).await {
                            Ok(_) => {
                                trace!("Sent back to router with addr:{}", addr);
                            }
                            Err(error) => {
                                error!("Error sending to ruter: {:?}", error);
                                break;
                            }
                        };
                    } else {
                        error!("Error recieven from socket");
                        break;
                    }
               },
               b = self.rx.recv() => {
                   if let Some((res_buf, addr)) = b {
                        match self.udp_socket.send_to(res_buf.as_slice(), addr).await {
                            Ok(_) => {
                                    trace!("Sent back to client {:?} buf:{:?}", addr, "somebuffer");
                            },
                            Err(error) => {
                                    error!("Error happened local recieve {:?} ", error);
                                    break;
                            }
                        }
                    } else {
                        error!("Error happened local recieve");
                        break;

                    }
               },
               _ = sleep(Duration::from_secs(timeout)) => {
                    (*globals::IDLE_IN_UDP_SOCKET).increment(1);
                    info!("No traffic on ProxyEndpoint {:?}, exiting", &self.endpoint);
                    break;
               }
            }
        }
        if let Err(err) = self
            .msg_tx
            .send(ProxyManagerMsg::UdpEndpointEnded {
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

#[derive(Debug)]
struct ProxyRouterClient {
    tx: Sender<(Vec<u8>, SocketAddr)>, // sender to be copied to ms faced clients
    rx: Receiver<(Vec<u8>, SocketAddr)>, // reciever for routing incoming
    clients: FxHashMap<SocketAddr, ProxyClientHandler>,
    endpoint: Endpoint,
    msg_tx: Sender<ProxyManagerMsg>,
}

impl ProxyRouterClient {
    fn new(
        tx: Sender<(Vec<u8>, SocketAddr)>,
        rx: Receiver<(Vec<u8>, SocketAddr)>,
        msg_tx: Sender<ProxyManagerMsg>,
        endpoint: Endpoint,
    ) -> Self {
        let clients = FxHashMap::<SocketAddr, ProxyClientHandler>::default();
        ProxyRouterClient {
            tx,
            rx,
            clients,
            endpoint,
            msg_tx,
        }
    }

    #[inline]
    async fn get_or_create_client(
        &mut self,
        addr: &SocketAddr,
        sender: Sender<ProxyClientMsg>,
    ) -> Result<&ProxyClientHandler, Box<dyn Error + Send + Sync>> {
        let pch = match self.clients.entry(addr.to_owned()) {
            Entry::Occupied(eo) => eo.into_mut(),
            Entry::Vacant(ev) => {
                let h =
                    ProxyClientHandler::new(self.endpoint.clone(), *addr, self.tx.clone(), sender)
                        .await?;
                ev.insert(h)
            }
        };
        Ok(pch)
    }

    #[inline]
    async fn run(mut self) {
        let timeout = *consts::TRAFFIC_WAIT_TIMEOUT;
        let (tx, mut client_rx) = mpsc::channel::<ProxyClientMsg>(10);
        loop {
            select! {
                a = self.rx.recv() => {
                   if let Some((data, addr)) = a {
                         if let Ok(client) = self.get_or_create_client(&addr, tx.clone()).await.fix_box() {
                            if let Err(_) = client.send(data).await {
                                error!("Can not send data to proxy clinet {:?}, removing {:?}", client, addr);
                                self.clients.remove(&addr);
                                if self.clients.len() == 0 {
                                    break;
                                }
                            }
                        } else {
                            error!("Can not get or create client for {:?}", &addr);
                            break;
                        }
                    } else  {
                        info!("No channel any more finishing ProxyRouterClient");
                        break;
                    }
                },
                b = client_rx.recv() => {
                    match b {
                        Some(msg) => match msg {
                            ProxyClientMsg::ClientEnded { addr } => {
                                self.clients.remove(&addr);
                                if self.clients.len() == 0 {
                                    break;
                                }
                            }
                        },
                        None => break,
                    }
                },
                _ = sleep(Duration::from_secs(timeout)) => {
                    (*globals::IDLE_OUT_UDP_SOCKET).increment(1);
                    info!("ProxyRouterClient: No traffic for {:?} exiting", &self.endpoint);
                    break;
               }
            }
        }
        if let Err(err) = self
            .msg_tx
            .send(ProxyManagerMsg::UdpEndpointEnded {
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

#[derive(Debug)]
pub(crate) struct ProxyEndpointHandler {
    pub local_port: u16,
    endpoint_handle: JoinHandle<()>,
    router_handle: JoinHandle<()>,
}

impl ProxyEndpointHandler {
    pub async fn new(
        endpoint: Endpoint,
        msg_tx: Sender<ProxyManagerMsg>,
    ) -> Result<Self, Box<dyn Error>> {
        let local_socket = UdpSocket::bind("0.0.0.0:0").await?;
        let port: u16 = local_socket.local_addr()?.port();

        debug!("udp port allocated {:?}", port);
        let channel_size = *consts::UDP_CHANNEL_SIZE;
        let (tx, rx) = mpsc::channel::<(Vec<u8>, SocketAddr)>(channel_size);
        let (router_tx, router_rx) = mpsc::channel::<(Vec<u8>, SocketAddr)>(channel_size);

        let pr_endpoint = ProxyEndpoint::new(
            local_socket,
            router_tx,
            rx,
            msg_tx.clone(),
            endpoint.clone(),
        );

        let handle = tokio::spawn(async move {
            pr_endpoint.run().await;
        });
        let router = ProxyRouterClient::new(tx.clone(), router_rx, msg_tx.clone(), endpoint);
        let router_handle = tokio::spawn(async move {
            router.run().await;
        });

        Ok(ProxyEndpointHandler {
            local_port: port,
            endpoint_handle: handle,
            router_handle: router_handle,
        })
    }

    pub async fn kill(&self) {
        self.endpoint_handle.abort();
        self.router_handle.abort();
    }
}

#[derive(Debug)]
struct ProxyClient {
    udp_socket: UdpSocket,
    tx: Sender<(Vec<u8>, SocketAddr)>,
    rx: Receiver<Vec<u8>>,
    addr: SocketAddr,
    msg_tx: Sender<ProxyClientMsg>,
}

impl ProxyClient {
    fn new(
        udp_socket: UdpSocket,
        tx: Sender<(Vec<u8>, SocketAddr)>,
        rx: Receiver<Vec<u8>>,
        addr: SocketAddr,
        msg_tx: Sender<ProxyClientMsg>,
    ) -> Self {
        (*globals::OUT_UDP_SOCKET_COUNTER).increment(1);
        ProxyClient {
            udp_socket,
            tx,
            rx,
            addr,
            msg_tx,
        }
    }

    #[inline]
    async fn run(mut self) {
        let timeout = *consts::TRAFFIC_WAIT_TIMEOUT;
        let mut buf = [0; 4 * 1024];
        loop {
            select! {
                a = self.udp_socket.recv(&mut buf) => {
                    if let Ok(len) = a {
                        trace!("{:?} bytes received from {:?}", len, &self.addr);
                        if let Err(error) = self.tx.send((buf[..len].to_vec(), self.addr)).await {
                            error!("Error happened router send {:?} ", error);
                            break;
                        }
                    } else {
                        error!("ProxyClient: Error sending to socket, existing");
                        break;
                    }
               },
               b = self.rx.recv() => {
                if let Some(res_buf) = b {
                   match self.udp_socket.send(res_buf.as_slice()).await {
                       Ok(_) => {
                            trace!("Sent buffer to server");
                        },
                        Err(error) => {
                           error!("ProxyClient: Error happened remote recieve {:?} ", error);
                           break;
                        }
                    }
                } else {
                    info!("ProxyClient: Channel is closed for {:?}, exiting", &self.addr);
                    break;
                }
               },
               _ = sleep(Duration::from_secs(timeout)) => {
                    info!("ProxyClient: No traffic for {:?} exiting", &self.addr);
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

#[derive(Clone, Debug)]
struct ProxyClientHandler {
    //agent handler
    tx: Sender<Vec<u8>>,
}

impl ProxyClientHandler {
    async fn new(
        endpoint: Endpoint,
        client_addr: SocketAddr,
        server_sender: Sender<(Vec<u8>, SocketAddr)>,
        msg_sender: Sender<ProxyClientMsg>,
    ) -> Result<Self, Box<dyn Error + Send + Sync>> {
        let remote_addr = endpoint
            .to_string()
            .to_socket_addrs()?
            .next()
            .ok_or("Something went wrong")?;
        let channel_size = *consts::UDP_CHANNEL_SIZE;
        let (tx, rx) = mpsc::channel::<Vec<u8>>(channel_size);
        let udp_socket = UdpSocket::bind("0.0.0.0:0").await?;
        let _ = match udp_socket.connect(remote_addr).await {
            Ok(_) => {
                debug!(
                    //it cause tests to fail not sure why
                    "connected to remote addr {:?} for {:?} ",
                    &remote_addr, &client_addr
                );
                let client =
                    ProxyClient::new(udp_socket, server_sender, rx, client_addr, msg_sender);
                tokio::spawn(async move {
                    client.run().await;
                })
            }
            Err(error) => {
                error!(error = ?error,  "Error happened rm_socket_error");
                return Err("Something went wrong".to_string().into());
            }
        };
        Ok(ProxyClientHandler { tx })
    }

    #[inline]
    async fn send(&self, data: Vec<u8>) -> Result<(), Box<dyn Error + Sync + Send>> {
        self.tx.send(data).await.fix_box()?;
        Ok(())
    }
}
