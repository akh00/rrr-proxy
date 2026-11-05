use log::{info, warn};
use rustc_hash::FxHashMap;
use std::collections::hash_map::Entry;
use std::error::Error;
use std::fmt;
use std::net::{SocketAddr, ToSocketAddrs};
use std::sync::Arc;
use tcp_clients::ProxyTcpEndpointHandler;
use tokio::sync::mpsc::error::SendError;
use tokio::sync::mpsc::{self, Sender};
use tokio::sync::{RwLock, oneshot};
use tracing::{debug, error, instrument};

use crate::manager::models::{AllocateRequest, AllocateResponse};
use crate::proxy::udp_client::ProxyEndpointHandler;
use crate::proxy::udp_pooled::PooledUdpEndpointHandler;

pub mod stun;
pub mod tcp_clients;
pub mod udp_client;
pub mod udp_pooled;

pub trait FixBoxError<T> {
    fn fix_box(self) -> Result<T, Box<dyn Error + Send + Sync>>;
}
impl<T> FixBoxError<T> for Result<T, Box<dyn Error>> {
    fn fix_box(self) -> Result<T, Box<dyn Error + Send + Sync>> {
        match self {
            Err(err) => Err(err.to_string().into()),
            Ok(t) => Ok(t),
        }
    }
}
impl<T> FixBoxError<T> for Result<T, Box<dyn Error + Send + Sync>> {
    fn fix_box(self) -> Result<T, Box<dyn Error + Send + Sync>> {
        match self {
            Err(err) => Err(err.to_string().into()),
            Ok(t) => Ok(t),
        }
    }
}
impl<T> FixBoxError<T> for Result<T, SendError<Vec<u8>>> {
    fn fix_box(self) -> Result<T, Box<dyn Error + Send + Sync>> {
        match self {
            Err(err) => Err(err.to_string().into()),
            Ok(t) => Ok(t),
        }
    }
}

#[derive(PartialEq, Eq, Hash, Debug, Clone)]
pub struct Session {
    port: u16,
    session_id: String,
}

#[derive(PartialEq, Eq, Hash, Debug, Clone)]
pub struct Endpoint {
    port: u16,
    ip: String,
    session_id: String,
}
impl Endpoint {
    fn new(port: u16, ip: &str, session_id: &str) -> Endpoint {
        Endpoint {
            port,
            ip: ip.to_string(),
            session_id: session_id.to_string(),
        }
    }

    fn socket_addr(&self) -> std::io::Result<SocketAddr> {
        (self.ip.as_str(), self.port)
            .to_socket_addrs()?
            .next()
            .ok_or_else(|| std::io::Error::other("Can not resolve endpoint address"))
    }
}

impl fmt::Display for Endpoint {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}:{}", self.ip, self.port)
    }
}

#[derive(Debug)]
pub enum ProxyClientMsg {
    ClientEnded { addr: SocketAddr },
    DataSent {},
    Die {},
}

pub enum DataMsg {}

pub enum ProxyManagerMsg {
    CreateUdpProxy {
        respond_to: oneshot::Sender<ProxyManagerMsg>,
        endpoint: Endpoint,
    },
    CreateTcpProxy {
        respond_to: oneshot::Sender<ProxyManagerMsg>,
        endpoint: Endpoint,
    },
    CreateUdpProxyRs {
        port: u16,
    },
    CreateTcpProxyRs {
        port: u16,
    },
    UdpProxyErrorRs {
        error: String,
    },
    TcpProxyErrorRs {
        error: String,
    },
    DeleteUdpProxy {
        respond_to: oneshot::Sender<ProxyManagerMsg>,
        endpoint: Endpoint,
    },
    DeleteTcpProxy {
        respond_to: oneshot::Sender<ProxyManagerMsg>,
        endpoint: Endpoint,
    },
    DeleteUdpProxyRs {},
    DeleteTcpProxyRs {},
    UdpEndpointEnded {
        endpoint: Endpoint,
    },
    TcpEndpointEnded {
        endpoint: Endpoint,
    },
}

/// How inbound udp ports are handed out to sessions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UdpProxyMode {
    /// A dedicated port per session, closed when idle or deleted.
    PerSession,
    /// A fixed pool of ports shared by all sessions, routed by the ICE ufrag in STUN USERNAME.
    Pooled,
}

#[derive(Debug)]
pub struct ProxyManager {
    tx: Sender<ProxyManagerMsg>,
    udp_endpoints: FxHashMap<Session, Endpoint>,
    tcp_endpoints: FxHashMap<Session, Endpoint>,
    udp_mode: UdpProxyMode,
    udp_pool: Vec<PooledUdpEndpointHandler>,
    next_pooled: usize,
}

impl ProxyManager {
    pub fn new() -> Self {
        let (tx, mut rx) = mpsc::channel::<ProxyManagerMsg>(100);
        let sender_to_pass = tx.clone();
        let _ = tokio::spawn(async move {
            let mut udp_proxy = FxHashMap::<Endpoint, ProxyEndpointHandler>::default();
            let mut tcp_proxy = FxHashMap::<Endpoint, ProxyTcpEndpointHandler>::default();
            loop {
                while let Some(msg) = rx.recv().await {
                    match msg {
                        ProxyManagerMsg::CreateTcpProxy {
                            respond_to,
                            endpoint,
                        } => {
                            let _ = match Self::create_tcp_proxy_impl(
                                &mut tcp_proxy,
                                endpoint,
                                sender_to_pass.clone(),
                            )
                            .await
                            {
                                Ok(port) => {
                                    respond_to.send(ProxyManagerMsg::CreateTcpProxyRs { port })
                                }
                                Err(error) => respond_to.send(ProxyManagerMsg::TcpProxyErrorRs {
                                    error: error.to_string(),
                                }),
                            };
                        }
                        ProxyManagerMsg::CreateUdpProxy {
                            respond_to,
                            endpoint,
                        } => {
                            let _ = match Self::create_udp_proxy_impl(
                                &mut udp_proxy,
                                endpoint,
                                sender_to_pass.clone(),
                            )
                            .await
                            {
                                Ok(port) => {
                                    respond_to.send(ProxyManagerMsg::CreateUdpProxyRs { port })
                                }
                                Err(error) => respond_to.send(ProxyManagerMsg::UdpProxyErrorRs {
                                    error: error.to_string(),
                                }),
                            };
                        }
                        ProxyManagerMsg::DeleteTcpProxy {
                            respond_to,
                            endpoint,
                        } => {
                            let _ = match Self::delete_tcp_proxy_impl(&mut tcp_proxy, endpoint)
                                .await
                            {
                                Ok(_) => respond_to.send(ProxyManagerMsg::DeleteTcpProxyRs {}),
                                Err(error) => respond_to.send(ProxyManagerMsg::TcpProxyErrorRs {
                                    error: error.to_string(),
                                }),
                            };
                        }
                        ProxyManagerMsg::DeleteUdpProxy {
                            respond_to,
                            endpoint,
                        } => {
                            let _ = match Self::delete_udp_proxy_impl(&mut udp_proxy, endpoint)
                                .await
                            {
                                Ok(_) => respond_to.send(ProxyManagerMsg::DeleteUdpProxyRs {}),
                                Err(error) => respond_to.send(ProxyManagerMsg::UdpProxyErrorRs {
                                    error: error.to_string(),
                                }),
                            };
                        }
                        ProxyManagerMsg::UdpEndpointEnded { endpoint } => {
                            let _ =
                                match Self::delete_udp_proxy_impl(&mut udp_proxy, endpoint.clone())
                                    .await
                                {
                                    Ok(_) => {
                                        info!("The udp endpoint: {:?} ended", endpoint);
                                    }
                                    Err(error) => {
                                        warn!(
                                            "Error deleting udp endpoint: {:?}: {:?}",
                                            endpoint, error
                                        );
                                    }
                                };
                            continue;
                        }
                        ProxyManagerMsg::TcpEndpointEnded { endpoint } => {
                            let _ =
                                match Self::delete_tcp_proxy_impl(&mut tcp_proxy, endpoint.clone())
                                    .await
                                {
                                    Ok(_) => {
                                        info!("The tcp endpoint: {:?} ended", &endpoint);
                                    }
                                    Err(error) => {
                                        warn!(
                                            "Error deleting tcp endpoint: {:?}: {:?}",
                                            &endpoint, error
                                        );
                                    }
                                };
                            continue;
                        }
                        _ => break,
                    }
                }
            }
        });
        ProxyManager {
            tx: tx,
            udp_endpoints: FxHashMap::<Session, Endpoint>::default(),
            tcp_endpoints: FxHashMap::<Session, Endpoint>::default(),
            udp_mode: UdpProxyMode::PerSession,
            udp_pool: Vec::new(),
            next_pooled: 0,
        }
    }

    /// A `pool_size` of 0 is the per-session mode. Otherwise binds `pool_size` pooled ports from
    /// `base_port` (0 for ephemeral ports), tolerating partial failures as long as one binds.
    pub fn with_udp_pool(pool_size: usize, base_port: u16) -> Result<Self, anyhow::Error> {
        let mut manager = Self::new();
        for i in 0..pool_size {
            let port = match base_port {
                0 => 0,
                _ => u16::try_from(i)
                    .ok()
                    .and_then(|i| base_port.checked_add(i))
                    .ok_or_else(|| anyhow::anyhow!("udp pool exceeds the port range"))?,
            };
            match PooledUdpEndpointHandler::bind(port) {
                Ok(handler) => manager.udp_pool.push(handler),
                Err(error) => error!(
                    error = ?error,
                    "Failed to bind pooled udp socket {} of {} on port {}",
                    i + 1,
                    pool_size,
                    port
                ),
            }
        }
        if pool_size > 0 {
            if manager.udp_pool.is_empty() {
                return Err(anyhow::anyhow!(
                    "Could not bind any of the {} pooled udp sockets",
                    pool_size
                ));
            }
            manager.udp_mode = UdpProxyMode::Pooled;
        }
        Ok(manager)
    }

    pub fn udp_mode(&self) -> UdpProxyMode {
        self.udp_mode
    }

    pub fn pooled_udp_ports(&self) -> Vec<u16> {
        self.udp_pool.iter().map(|p| p.local_port).collect()
    }

    pub async fn delete_udp_proxy(
        &mut self,
        port: u16,
        session_id: String,
    ) -> Result<(), anyhow::Error> {
        if self.udp_mode == UdpProxyMode::Pooled {
            return self.delete_pooled_udp_proxy(port, session_id).await;
        }
        let endpoint = match self.udp_endpoints.get(&Session { port, session_id }) {
            Some(endpoint) => endpoint,
            None => return Err(anyhow::anyhow!("No such endpoint")),
        };
        let (rx, one_shot_tx) = oneshot::channel::<ProxyManagerMsg>();
        self.tx
            .send(ProxyManagerMsg::DeleteUdpProxy {
                respond_to: rx,
                endpoint: endpoint.clone(),
            })
            .await?;
        let _ = match one_shot_tx.await {
            Ok(res) => match res {
                ProxyManagerMsg::DeleteUdpProxyRs {} => Ok(()),
                ProxyManagerMsg::UdpProxyErrorRs { error } => Err(anyhow::anyhow!(error)),
                _ => Err(anyhow::anyhow!(
                    "Something happened during udp endpoint deletion"
                )),
            },
            Err(_) => Err(anyhow::anyhow!(
                "Something happened during udp endpoint deletion",
            )),
        };
        Ok(())
    }

    pub async fn delete_tcp_proxy(
        &mut self,
        port: u16,
        session_id: String,
    ) -> Result<(), anyhow::Error> {
        let endpoint = match self.tcp_endpoints.get(&Session { port, session_id }) {
            Some(endpoint) => endpoint,
            None => return Err(anyhow::anyhow!("No such endpoint")),
        };
        let (rx, one_shot_tx) = oneshot::channel::<ProxyManagerMsg>();
        self.tx
            .send(ProxyManagerMsg::DeleteTcpProxy {
                respond_to: rx,
                endpoint: endpoint.clone(),
            })
            .await?;
        let _ = match one_shot_tx.await {
            Ok(res) => match res {
                ProxyManagerMsg::DeleteTcpProxyRs {} => Ok(()),
                ProxyManagerMsg::TcpProxyErrorRs { error } => Err(anyhow::anyhow!(error)),
                _ => Err(anyhow::anyhow!(
                    "Something happened during tcp endpoint deletion",
                )),
            },
            Err(_) => Err(anyhow::anyhow!(
                "Something happened during tcp endpoint deletion",
            )),
        };
        Ok(())
    }

    //  #[instrument(level = "debug")]
    pub async fn create_proxy(
        &mut self,
        request: AllocateRequest,
    ) -> Result<AllocateResponse, Box<dyn Error>> {
        let endpoint = Endpoint::new(
            request.target_udp_port.parse::<u16>().unwrap(),
            &request.target_ip,
            &request.session_id,
        );
        let udp_port = match self.udp_mode {
            UdpProxyMode::PerSession => self.create_udp_proxy_internal(endpoint).await?,
            UdpProxyMode::Pooled => {
                self.create_pooled_udp_proxy(endpoint, request.ufrag.as_deref())
                    .await?
            }
        };
        let mut tcp_port = request.target_tcp_port.parse::<u16>()?;
        if tcp_port != 0 {
            tcp_port = self
                .create_tcp_proxy_internal(&request.target_ip, tcp_port, &request.session_id)
                .await?;
        }
        let mut ssl_tcp_port = request.target_ssl_tcp_port.parse::<u16>()?;
        if ssl_tcp_port != 0 {
            ssl_tcp_port = self
                .create_tcp_proxy_internal(&request.target_ip, ssl_tcp_port, &request.session_id)
                .await?;
        }
        Ok(AllocateResponse {
            proxy_udp_port: udp_port,
            proxy_tcp_port: tcp_port,
            proxy_ssl_tcp_port: ssl_tcp_port,
        })
    }

    async fn create_udp_proxy_internal(
        &mut self,
        endpoint: Endpoint,
    ) -> Result<u16, Box<dyn Error>> {
        let (rx, one_shot_tx) = oneshot::channel::<ProxyManagerMsg>();
        self.tx
            .send(ProxyManagerMsg::CreateUdpProxy {
                respond_to: rx,
                endpoint: endpoint.clone(),
            })
            .await?;
        match one_shot_tx.await {
            Ok(res) => match res {
                ProxyManagerMsg::CreateUdpProxyRs { port } => {
                    debug!(
                        "udp proxy created port:{:?}, endpoint: {:?}",
                        port, &endpoint
                    );
                    self.udp_endpoints.insert(
                        Session {
                            port,
                            session_id: endpoint.session_id.clone(),
                        },
                        endpoint,
                    );
                    Ok(port)
                }
                ProxyManagerMsg::UdpProxyErrorRs { error } => {
                    error!(error = error, "ProxyManagerMsg::UdpProxyErrorRs");
                    Err(Box::<dyn Error>::from(error))
                }
                _ => {
                    error!(
                        error = "uncknown message recieved",
                        "ProxyManagerMsg::CreateUdpProxyRs"
                    );
                    Err(Box::<dyn Error>::from("Something happened"))
                }
            },
            Err(err) => {
                error!(
                    error = err.to_string(),
                    "Error receiving udp proxy creation event",
                );
                Err(Box::<dyn Error>::from("Something happened"))
            }
        }
    }

    async fn create_pooled_udp_proxy(
        &mut self,
        endpoint: Endpoint,
        ufrag: Option<&str>,
    ) -> Result<u16, Box<dyn Error>> {
        let ufrag = match ufrag {
            Some(ufrag) if !ufrag.is_empty() => ufrag,
            _ => {
                return Err(Box::<dyn Error>::from(
                    "ufrag is required in pooled udp mode",
                ));
            }
        };
        let target = endpoint.socket_addr()?;
        // round robin spreads sessions evenly over the pooled sockets
        let pooled = &self.udp_pool[self.next_pooled % self.udp_pool.len()];
        self.next_pooled = self.next_pooled.wrapping_add(1);
        let port = pooled.local_port;
        pooled
            .add_route(endpoint.session_id.clone(), ufrag.to_string(), target)
            .await
            .map_err(|err| err.to_string())?;
        debug!(
            "pooled udp proxy port:{:?}, endpoint: {:?}",
            port, &endpoint
        );
        self.udp_endpoints.insert(
            Session {
                port,
                session_id: endpoint.session_id.clone(),
            },
            endpoint,
        );
        Ok(port)
    }

    async fn delete_pooled_udp_proxy(
        &mut self,
        port: u16,
        session_id: String,
    ) -> Result<(), anyhow::Error> {
        let session = Session { port, session_id };
        if self.udp_endpoints.remove(&session).is_none() {
            return Err(anyhow::anyhow!("No such endpoint"));
        }
        match self.udp_pool.iter().find(|p| p.local_port == port) {
            Some(pooled) => match pooled.remove_session(session.session_id).await {
                Ok(_) => Ok(()),
                Err(err) => Err(anyhow::anyhow!(err.to_string())),
            },
            None => Err(anyhow::anyhow!("No such pooled port")),
        }
    }

    async fn create_tcp_proxy_internal(
        &mut self,
        target_ip: &String,
        target_tcp_port: u16,
        session_id: &String,
    ) -> Result<u16, Box<dyn Error + 'static>> {
        let (rx, one_shot_tx) = oneshot::channel::<ProxyManagerMsg>();
        let endpoint = Endpoint::new(target_tcp_port, &target_ip, &session_id);
        if let Err(_) = self
            .tx
            .send(ProxyManagerMsg::CreateTcpProxy {
                respond_to: rx,
                endpoint: endpoint.clone(),
            })
            .await
        {
            Err(Box::<dyn Error>::from(
                "Can not initiate tcp proxy creation",
            ))
        } else {
            match one_shot_tx.await {
                Ok(res) => match res {
                    ProxyManagerMsg::CreateTcpProxyRs { port } => {
                        self.tcp_endpoints.insert(
                            Session {
                                port,
                                session_id: endpoint.session_id.clone(),
                            },
                            endpoint,
                        );
                        Ok(port)
                    }
                    ProxyManagerMsg::TcpProxyErrorRs { error } => {
                        Err(Box::<dyn Error>::from(error))
                    }
                    _ => Err(Box::<dyn Error>::from("Bad messsage")),
                },
                Err(_) => Err(Box::<dyn Error>::from(
                    "Tcp allocation result has not been recieved",
                )),
            }
        }
    }

    #[instrument(level = "debug")]
    async fn create_udp_proxy_impl(
        udp_proxy: &mut FxHashMap<Endpoint, ProxyEndpointHandler>,
        endpoint: Endpoint,
        tx: Sender<ProxyManagerMsg>,
    ) -> Result<u16, Box<dyn Error>> {
        let proxy = match udp_proxy.entry(endpoint.clone()) {
            Entry::Occupied(oe) => {
                warn!("There are already udp proxy created");
                oe.into_mut()
            }
            Entry::Vacant(ve) => {
                let pe = ProxyEndpointHandler::new(endpoint.clone(), tx).await?;
                ve.insert(pe)
            }
        };
        Ok(proxy.local_port)
    }

    #[instrument(level = "debug")]
    async fn create_tcp_proxy_impl(
        tcp_proxy: &mut FxHashMap<Endpoint, ProxyTcpEndpointHandler>,
        endpoint: Endpoint,
        tx: Sender<ProxyManagerMsg>,
    ) -> Result<u16, Box<dyn Error>> {
        let proxy = match tcp_proxy.entry(endpoint.clone()) {
            Entry::Occupied(oe) => {
                warn!("There are already udp proxy created");
                oe.into_mut()
            }
            Entry::Vacant(ve) => {
                let pe = ProxyTcpEndpointHandler::new(endpoint.clone(), tx).await?;
                ve.insert(pe)
            }
        };
        Ok(proxy.local_port)
    }

    async fn delete_udp_proxy_impl(
        udp_proxy: &mut FxHashMap<Endpoint, ProxyEndpointHandler>,
        endpoint: Endpoint,
    ) -> Result<(), Box<dyn Error>> {
        match udp_proxy.remove(&endpoint) {
            Some(handler) => {
                handler.kill().await;
                Ok(())
            }
            None => Err(Box::<dyn Error>::from(std::format!(
                "There aren't proxy to delete for {:?}",
                endpoint
            ))),
        }
    }
    async fn delete_tcp_proxy_impl(
        tcp_proxy: &mut FxHashMap<Endpoint, ProxyTcpEndpointHandler>,
        endpoint: Endpoint,
    ) -> Result<(), Box<dyn Error>> {
        match tcp_proxy.remove(&endpoint) {
            Some(handler) => {
                handler.kill().await;
                Ok(())
            }
            None => Err(Box::<dyn Error>::from(std::format!(
                "There aren't proxy to delete for {:?}",
                endpoint
            ))),
        }
    }
}

pub type ProxyManagerShared = Arc<RwLock<ProxyManager>>;
