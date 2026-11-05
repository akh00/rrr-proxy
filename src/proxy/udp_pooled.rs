use rustc_hash::FxHashMap;
use socket2::{Domain, Protocol, Socket, Type};
use std::{error::Error, net::SocketAddr, sync::Arc};
use tokio::{
    net::UdpSocket,
    select,
    sync::{
        mpsc::{self, Receiver, Sender},
        oneshot,
    },
    task::JoinHandle,
};
use tracing::{debug, error, info, trace, warn};

use super::ProxyClientMsg;
use super::stun;
use super::udp_client::{ProxyClient, ProxyClientHandler, allocate_handler};
use crate::manager::pmetrics::globals;

//
//  A pooled inbound socket shared by many sessions. The first datagram of an unknown client
//  must be a STUN binding request: its local ufrag selects the target. Afterwards the client
//  address alone routes the flow, so media packets are never parsed. The socket never idles
//  out and outlives any single session; routing changes arrive over the control channel so
//  the receive loop owns all state without locks.
//

#[derive(Debug)]
enum PooledUdpMsg {
    AddRoute {
        session_id: String,
        ufrag: String,
        target: SocketAddr,
        respond_to: oneshot::Sender<()>,
    },
    RemoveSession {
        session_id: String,
        respond_to: oneshot::Sender<bool>,
    },
}

#[derive(Debug)]
struct PooledClient {
    handler: ProxyClientHandler,
    ufrag: Arc<str>,
}

#[derive(Debug)]
struct PooledUdpEndpoint {
    udp_socket: Arc<UdpSocket>,
    control_rx: Receiver<PooledUdpMsg>,
    routes: FxHashMap<Arc<str>, SocketAddr>,
    sessions: FxHashMap<String, Arc<str>>,
    clients: FxHashMap<SocketAddr, PooledClient>,
}

impl PooledUdpEndpoint {
    fn new(udp_socket: Arc<UdpSocket>, control_rx: Receiver<PooledUdpMsg>) -> Self {
        (*globals::IN_UDP_SOCKET_COUNTER).increment(1);
        PooledUdpEndpoint {
            udp_socket,
            control_rx,
            routes: FxHashMap::default(),
            sessions: FxHashMap::default(),
            clients: FxHashMap::default(),
        }
    }

    async fn run(mut self) {
        let mut buf = [0; 4 * 1024];
        let (client_tx, mut client_rx) = mpsc::channel::<ProxyClientMsg>(10);
        loop {
            select! {
                a = self.udp_socket.recv_from(&mut buf) => match a {
                    Ok((len, addr)) => self.on_datagram(&buf[..len], addr, &client_tx).await,
                    // a shared socket must not die on a transient error
                    Err(error) => error!(error = ?error, "Error receiving on pooled udp socket"),
                },
                Some(msg) = client_rx.recv() => {
                    if let ProxyClientMsg::ClientEnded { addr } = msg {
                        // the address may already be served by a newer relay
                        if self.clients.get(&addr).is_some_and(|c| c.handler.sender.is_closed()) {
                            self.clients.remove(&addr);
                        }
                    }
                },
                msg = self.control_rx.recv() => match msg {
                    Some(msg) => self.on_control(msg),
                    None => break,
                },
            }
        }
        info!("Pooled udp endpoint closed");
    }

    #[inline]
    async fn on_datagram(
        &mut self,
        data: &[u8],
        addr: SocketAddr,
        client_tx: &Sender<ProxyClientMsg>,
    ) {
        (*globals::IN_CLIENT_UDP_MESSAGE).increment(1);
        trace!("{:?} bytes received from {:?}", data.len(), addr);
        if let Some(client) = self.clients.get(&addr) {
            if ProxyClient::send(&client.handler.socket, data, &client.handler.sender)
                .await
                .is_ok()
            {
                return;
            }
            // the client relay has ended, route this datagram afresh
            self.clients.remove(&addr);
        }
        let Some((ufrag, target)) = stun::local_ufrag(data)
            .and_then(|ufrag| self.routes.get_key_value(ufrag))
            .map(|(ufrag, target)| (Arc::clone(ufrag), *target))
        else {
            (*globals::UNROUTED_POOLED_UDP_MESSAGE).increment(1);
            trace!("Dropping unroutable datagram from {:?}", addr);
            return;
        };
        match allocate_handler(
            target,
            addr,
            Arc::clone(&self.udp_socket),
            client_tx.clone(),
        )
        .await
        {
            Ok(handler) => {
                debug!(
                    "Routing client {:?} by ufrag {:?} to {:?}",
                    addr, ufrag, target
                );
                if ProxyClient::send(&handler.socket, data, &handler.sender)
                    .await
                    .is_ok()
                {
                    self.clients.insert(addr, PooledClient { handler, ufrag });
                }
            }
            Err(error) => error!(error = ?error, "Can not create client for {:?}", addr),
        }
    }

    fn on_control(&mut self, msg: PooledUdpMsg) {
        match msg {
            PooledUdpMsg::AddRoute {
                session_id,
                ufrag,
                target,
                respond_to,
            } => {
                let ufrag: Arc<str> = match self.routes.get_key_value(ufrag.as_str()) {
                    Some((known, _)) => Arc::clone(known),
                    None => ufrag.into(),
                };
                let rerouted = self.routes.insert(Arc::clone(&ufrag), target);
                if let Some(previous) = rerouted.filter(|previous| *previous != target) {
                    warn!(
                        "ufrag {:?} rerouted from {:?} to {:?}",
                        ufrag, previous, target
                    );
                    self.drop_clients(&ufrag);
                }
                let replaced = self.sessions.insert(session_id, Arc::clone(&ufrag));
                if let Some(previous) = replaced.filter(|previous| *previous != ufrag) {
                    self.forget_ufrag_if_unused(&previous);
                }
                let _ = respond_to.send(());
            }
            PooledUdpMsg::RemoveSession {
                session_id,
                respond_to,
            } => {
                let found = match self.sessions.remove(&session_id) {
                    Some(ufrag) => {
                        self.forget_ufrag_if_unused(&ufrag);
                        true
                    }
                    None => false,
                };
                let _ = respond_to.send(found);
            }
        }
    }

    // keep the route while another live session still uses the same ufrag
    fn forget_ufrag_if_unused(&mut self, ufrag: &Arc<str>) {
        if !self.sessions.values().any(|u| u == ufrag) {
            self.routes.remove(ufrag);
            self.drop_clients(ufrag);
        }
    }

    // dropping a ProxyClientHandler aborts its relay task
    fn drop_clients(&mut self, ufrag: &Arc<str>) {
        self.clients.retain(|_, client| client.ufrag != *ufrag);
    }
}

#[derive(Debug)]
pub(crate) struct PooledUdpEndpointHandler {
    pub local_port: u16,
    control_tx: Sender<PooledUdpMsg>,
    endpoint_handle: JoinHandle<()>,
}

impl PooledUdpEndpointHandler {
    /// Binds a pooled socket on `port` (0 for ephemeral). A fixed, externally published port
    /// has to survive a rolling restart, hence SO_REUSEPORT. An ephemeral one must not set it:
    /// the kernel may then hand out a port already held by another pooled socket of this uid.
    pub fn bind(port: u16) -> Result<Self, Box<dyn Error + Send + Sync>> {
        let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
        if port != 0 {
            socket.set_reuse_port(true)?;
        }
        socket.set_nonblocking(true)?;
        socket.bind(&SocketAddr::from(([0, 0, 0, 0], port)).into())?;
        let udp_socket = UdpSocket::from_std(socket.into())?;
        let local_port = udp_socket.local_addr()?.port();
        debug!("pooled udp port allocated {:?}", local_port);

        let (control_tx, control_rx) = mpsc::channel::<PooledUdpMsg>(100);
        let endpoint = PooledUdpEndpoint::new(Arc::new(udp_socket), control_rx);
        let endpoint_handle = tokio::spawn(endpoint.run());
        Ok(PooledUdpEndpointHandler {
            local_port,
            control_tx,
            endpoint_handle,
        })
    }

    pub async fn add_route(
        &self,
        session_id: String,
        ufrag: String,
        target: SocketAddr,
    ) -> Result<(), Box<dyn Error + Send + Sync>> {
        // acknowledged, so the route is live before the allocation is answered
        let (respond_to, response) = oneshot::channel();
        self.control_tx
            .send(PooledUdpMsg::AddRoute {
                session_id,
                ufrag,
                target,
                respond_to,
            })
            .await
            .map_err(|_| "Pooled udp endpoint is closed")?;
        Ok(response.await?)
    }

    /// Ends one session on the shared socket; returns whether the session was known.
    pub async fn remove_session(
        &self,
        session_id: String,
    ) -> Result<bool, Box<dyn Error + Send + Sync>> {
        let (respond_to, response) = oneshot::channel();
        self.control_tx
            .send(PooledUdpMsg::RemoveSession {
                session_id,
                respond_to,
            })
            .await
            .map_err(|_| "Pooled udp endpoint is closed")?;
        Ok(response.await?)
    }
}

impl Drop for PooledUdpEndpointHandler {
    fn drop(&mut self) {
        self.endpoint_handle.abort();
    }
}
