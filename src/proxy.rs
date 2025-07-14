use log::{info, warn};
use std::fmt;
use std::net::SocketAddr;
use std::sync::Arc;
use std::{collections::HashMap, error::Error};
use tcp_clients::ProxyTcpEndpointHandler;
use tokio::sync::mpsc::{self, Sender};
use tokio::sync::{RwLock, oneshot};
use tokio::task::JoinHandle;

use crate::manager::models::{AllocateRequest, AllocateResponse};
use crate::proxy::udp_client::ProxyEndpointHandler;

pub mod tcp_clients;
pub mod udp_client;

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

#[derive(PartialEq, Eq, Hash, Debug, Clone)]
pub struct Endpoint {
    port: u16,
    ip: String,
}
impl Endpoint {
    fn new(port: u16, ip: &str) -> Endpoint {
        Endpoint {
            port,
            ip: ip.to_string(),
        }
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
}

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
    DeleteUdpProxyRs {},
    UdpEndpointEnded {
        endpoint: Endpoint,
    },
    TcpEndpointEnded {
        endpoint: Endpoint,
    },
}

pub struct ProxyManager {
    handler: JoinHandle<()>,
    tx: Sender<ProxyManagerMsg>,
    udp_endpoints: HashMap<u16, Endpoint>,
    tcp_endpoints: HashMap<u16, Endpoint>,
}

impl ProxyManager {
    pub fn new() -> Self {
        let (tx, mut rx) = mpsc::channel::<ProxyManagerMsg>(100);
        let sender_to_pass = tx.clone();
        let handler = tokio::spawn(async move {
            let mut udp_proxy = HashMap::<Endpoint, ProxyEndpointHandler>::new();
            let mut tcp_proxy = HashMap::<Endpoint, ProxyTcpEndpointHandler>::new();
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
                                        info!("The endpoint: {:?} ended", endpoint);
                                    }
                                    Err(error) => {
                                        warn!(
                                            "Error deleting endpoint: {:?}: {:?}",
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
                                        info!("The endpoint: {:?} ended", endpoint);
                                    }
                                    Err(error) => {
                                        warn!(
                                            "Error deleting endpoint: {:?}: {:?}",
                                            endpoint, error
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
            handler,
            tx: tx,
            udp_endpoints: HashMap::<u16, Endpoint>::new(),
            tcp_endpoints: HashMap::<u16, Endpoint>::new(),
        }
    }

    pub async fn delete_udp_proxy(&mut self, port: u16) -> Result<(), Box<dyn Error>> {
        let endpoint = match self.udp_endpoints.get(&port) {
            Some(endpoint) => endpoint,
            None => return Err(Box::<dyn Error>::from("No such endpoint")),
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
                ProxyManagerMsg::UdpProxyErrorRs { error } => Err(Box::<dyn Error>::from(error)),
                _ => Err(Box::<dyn Error>::from("Somethoing happened")),
            },
            Err(_) => Err(Box::<dyn Error>::from("Somethoing happened")),
        };
        Ok(())
    }
    pub async fn create_proxy(
        &mut self,
        request: AllocateRequest,
    ) -> Result<AllocateResponse, Box<dyn Error>> {
        let endpoint = Endpoint::new(request.target_udp_port, &request.target_ip);
        let (rx, one_shot_tx) = oneshot::channel::<ProxyManagerMsg>();
        self.tx
            .send(ProxyManagerMsg::CreateUdpProxy {
                respond_to: rx,
                endpoint: endpoint.clone(),
            })
            .await?;
        let udp_port = match one_shot_tx.await {
            Ok(res) => match res {
                ProxyManagerMsg::CreateUdpProxyRs { port } => {
                    self.udp_endpoints.insert(port, endpoint);
                    Ok(port)
                }
                ProxyManagerMsg::UdpProxyErrorRs { error } => Err(Box::<dyn Error>::from(error)),
                _ => Err(Box::<dyn Error>::from("Something happened")),
            },
            Err(_) => Err(Box::<dyn Error>::from("Something happened")),
        }?;
        let (rx, one_shot_tx) = oneshot::channel::<ProxyManagerMsg>();
        let endpoint = Endpoint::new(request.target_tcp_port, &request.target_ip);
        self.tx
            .send(ProxyManagerMsg::CreateTcpProxy {
                respond_to: rx,
                endpoint: endpoint.clone(),
            })
            .await?;
        let tcp_port = match one_shot_tx.await {
            Ok(res) => match res {
                ProxyManagerMsg::CreateTcpProxyRs { port } => {
                    self.tcp_endpoints.insert(port, endpoint);
                    Ok(port)
                }
                ProxyManagerMsg::UdpProxyErrorRs { error } => Err(Box::<dyn Error>::from(error)),
                _ => Err(Box::<dyn Error>::from("Something happened")),
            },
            Err(_) => Err(Box::<dyn Error>::from("Something happened")),
        }?;
        Ok(AllocateResponse {
            proxy_udp_port: udp_port,
            proxy_tcp_port: tcp_port,
            proxy_ssl_tcp_port: 433,
            load_percentage: 90,
        })
    }
    async fn create_udp_proxy_impl(
        udp_proxy: &mut HashMap<Endpoint, ProxyEndpointHandler>,
        endpoint: Endpoint,
        tx: Sender<ProxyManagerMsg>,
    ) -> Result<u16, Box<dyn Error>> {
        if !udp_proxy.contains_key(&endpoint) {
            let new_proxy = ProxyEndpointHandler::new(endpoint.clone(), tx).await?;
            let local_port = new_proxy.local_port;
            udp_proxy.insert(endpoint, new_proxy);
            Ok(local_port)
        } else {
            Err(Box::<dyn Error>::from(std::format!(
                "There are already proxy created for {:?}",
                endpoint
            )))
        }
    }

    async fn create_tcp_proxy_impl(
        tcp_proxy: &mut HashMap<Endpoint, ProxyTcpEndpointHandler>,
        endpoint: Endpoint,
        tx: Sender<ProxyManagerMsg>,
    ) -> Result<u16, Box<dyn Error>> {
        if !tcp_proxy.contains_key(&endpoint) {
            let new_proxy = ProxyTcpEndpointHandler::new(endpoint.clone(), tx).await?;
            let local_port = new_proxy.local_port;
            tcp_proxy.insert(endpoint, new_proxy);
            Ok(local_port)
        } else {
            Err(Box::<dyn Error>::from(std::format!(
                "There are already proxy created for {:?}",
                endpoint
            )))
        }
    }

    async fn delete_udp_proxy_impl(
        udp_proxy: &mut HashMap<Endpoint, ProxyEndpointHandler>,
        endpoint: Endpoint,
    ) -> Result<(), Box<dyn Error>> {
        match udp_proxy.get(&endpoint) {
            Some(handler) => {
                handler.kill().await;
                udp_proxy.remove(&endpoint);
                Ok(())
            }
            None => Err(Box::<dyn Error>::from(std::format!(
                "There aren't proxy to delete for {:?}",
                endpoint
            ))),
        }
    }
    async fn delete_tcp_proxy_impl(
        tcp_proxy: &mut HashMap<Endpoint, ProxyTcpEndpointHandler>,
        endpoint: Endpoint,
    ) -> Result<(), Box<dyn Error>> {
        match tcp_proxy.get(&endpoint) {
            Some(handler) => {
                handler.kill().await;
                tcp_proxy.remove(&endpoint);
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
