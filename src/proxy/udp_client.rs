use rustc_hash::FxHashMap;
use std::{
    collections::hash_map::Entry,
    error::Error,
    net::{SocketAddr, ToSocketAddrs},
    sync::Arc,
    time::Duration,
};
use tokio::{
    net::UdpSocket,
    select,
    sync::mpsc::{self, Sender},
    task::JoinHandle,
    time::sleep,
};
use tracing::{debug, error, info, trace};

use super::{Endpoint, ProxyClientMsg, ProxyManagerMsg};
use crate::manager::pmetrics;
use crate::proxy::FixBoxError;
use crate::proxy::MAX_DTGRAM_SIZE;
use crate::{consts, manager::pmetrics::globals};
//
//  agent pattern implementation
//
//
//

#[derive(Debug)]
struct ProxyEndpoint {
    //agent
    udp_socket: Arc<UdpSocket>,
    msg_tx: Sender<ProxyManagerMsg>,
    endpoint: Endpoint,
    clients: FxHashMap<SocketAddr, ProxyClientHandler>,
}

impl ProxyEndpoint {
    fn new(
        udp_socket: Arc<UdpSocket>,
        msg_tx: Sender<ProxyManagerMsg>,
        endpoint: Endpoint,
    ) -> Self {
        (*pmetrics::globals::IN_UDP_SOCKET_COUNTER).increment(1);
        let clients = FxHashMap::<SocketAddr, ProxyClientHandler>::default();
        ProxyEndpoint {
            udp_socket,
            msg_tx,
            endpoint,
            clients,
        }
    }

    #[inline]
    async fn get_or_create_client(
        &mut self,
        addr: &SocketAddr,
        target_socket: Arc<UdpSocket>,
        sender: Sender<ProxyClientMsg>,
    ) -> Result<&ProxyClientHandler, Box<dyn Error + Send + Sync>> {
        let pch = match self.clients.entry(*addr) {
            Entry::Occupied(eo) => eo.into_mut(),
            Entry::Vacant(ev) => {
                let h = allocate_handler(&self.endpoint, *addr, target_socket, sender).await?;
                ev.insert(h)
            }
        };
        Ok(pch)
    }

    #[inline]
    async fn run(mut self) {
        let timeout = *consts::TRAFFIC_WAIT_TIMEOUT;
        let mut buf = vec![0_u8; MAX_DTGRAM_SIZE];
        let in_udp_counter = &(*pmetrics::globals::IN_CLIENT_UDP_MESSAGE);
        let in_udp_idle_counter = &(*pmetrics::globals::IDLE_IN_UDP_SOCKET);
        let (msg_tx, mut client_rx) = mpsc::channel::<ProxyClientMsg>(10);
        loop {
            select! {
                a = self.udp_socket.recv_from(&mut buf) => {
                   if let Ok((len, addr )) = a {
                    if len <= 0 {
                        continue;
                    }
                    in_udp_counter.increment(1);
                    trace!("{:?} bytes received from {:?}", len, addr);
                    // to the router
                         if let Ok(client) = self.get_or_create_client(&addr, self.udp_socket.clone(), msg_tx.clone()).await.fix_box() {
                            if let Err(_) = client.socket.send(&buf[..len]).await {
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
                    } else {
                        error!("Error recieven from socket");
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
                    in_udp_idle_counter.increment(1);
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
pub(crate) struct ProxyEndpointHandler {
    pub local_port: u16,
    endpoint_handle: JoinHandle<()>,
}

impl ProxyEndpointHandler {
    pub async fn new(
        endpoint: Endpoint,
        msg_tx: Sender<ProxyManagerMsg>,
    ) -> Result<Self, Box<dyn Error>> {
        let local_socket = UdpSocket::bind("0.0.0.0:0").await?;
        let port: u16 = local_socket.local_addr()?.port();

        debug!("udp port allocated {:?}", port);

        let pr_endpoint =
            ProxyEndpoint::new(Arc::new(local_socket), msg_tx.clone(), endpoint.clone());

        let handle = tokio::spawn(async move {
            pr_endpoint.run().await;
        });

        Ok(ProxyEndpointHandler {
            local_port: port,
            endpoint_handle: handle,
        })
    }

    pub async fn kill(&self) {
        self.endpoint_handle.abort();
    }
}

#[derive(Debug)]
struct ProxyClientHandler {
    socket: Arc<UdpSocket>,
    handler: JoinHandle<()>,
}

#[derive(Debug)]
struct ProxyClient {
    socket: Arc<UdpSocket>,
    target_socket: Arc<UdpSocket>,
    addr: SocketAddr,
    msg_tx: Sender<ProxyClientMsg>,
}

impl ProxyClient {
    fn new(
        socket: Arc<UdpSocket>,
        target_socket: Arc<UdpSocket>,
        addr: SocketAddr,
        msg_tx: Sender<ProxyClientMsg>,
    ) -> Self {
        (*globals::OUT_UDP_SOCKET_COUNTER).increment(1);

        ProxyClient {
            socket,
            target_socket,
            addr,
            msg_tx,
        }
    }

    #[inline]
    async fn run(self) {
        let timeout = *consts::TRAFFIC_WAIT_TIMEOUT;
        let mut buf = vec![0_u8; MAX_DTGRAM_SIZE];
        let in_udp_server_counter = &(*pmetrics::globals::IN_MS_UDP_MESSAGE);
        let in_udp_idle = &(*pmetrics::globals::IDLE_OUT_UDP_SOCKET);
        loop {
            select! {
                a = self.socket.recv(&mut buf) => {
                    if let Ok(len) = a {
                        if len <= 0 {
                            continue;
                        }
                        in_udp_server_counter.increment(1);
                        trace!("{:?} bytes received from {:?}", len, &self.addr);
                        if let Err(error) = self.target_socket.send_to(&buf[..len], self.addr).await {
                            error!("Error happened router send {:?} ", error);
                            break;
                        }
                    } else {
                        error!("ProxyClient: Error sending to socket, existing");
                        break;
                    }
               },
               _ = sleep(Duration::from_secs(timeout)) => {
                    in_udp_idle.increment(1);
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

impl Drop for ProxyClientHandler {
    fn drop(&mut self) {
        self.handler.abort();
    }
}

#[inline]
async fn allocate_handler(
    endpoint: &Endpoint,
    client_addr: SocketAddr,
    target_socket: Arc<UdpSocket>,
    msg_sender: Sender<ProxyClientMsg>,
) -> Result<ProxyClientHandler, Box<dyn Error + Send + Sync>> {
    let remote_addr = endpoint
        .to_string()
        .to_socket_addrs()?
        .next()
        .ok_or("Something went wrong")?;
    let udp_socket = UdpSocket::bind("0.0.0.0:0").await?;
    let _ = match udp_socket.connect(remote_addr).await {
        Ok(_) => {
            debug!(
                //it cause tests to fail not sure why
                "connected to remote addr {:?} for {:?} ",
                &remote_addr, &client_addr
            );
            let arc_socket = Arc::new(udp_socket);
            let client =
                ProxyClient::new(arc_socket.clone(), target_socket, client_addr, msg_sender);

            let handler = tokio::spawn(async move {
                client.run().await;
            });
            return Ok(ProxyClientHandler {
                socket: arc_socket.clone(),
                handler,
            });
        }
        Err(error) => {
            error!(error = ?error,  "Error happened rm_socket_error");
            return Err("Something went wrong".to_string().into());
        }
    };
}
