use core::net::SocketAddr;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;
use std::{collections::HashMap, error::Error};
use tokio::sync::mpsc::{self, Sender};
use tokio::sync::{RwLock, oneshot};
use tokio::task::JoinHandle;

use crate::proxy::udp_client::ProxyEndpointHandler;
use crate::signaling::models::{UdpAllocateRequest, UdpAllocateResponse};

pub mod tcp;
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

enum ProxyManagerMsg {
    CreateUdpProxy {
        respond_to: oneshot::Sender<ProxyManagerMsg>,
        endpoint: Endpoint,
    },
    CreateUdpProxyRs {
        port: u16,
    },
    UdpProxyErrorRs {
        error: String,
    },
    DeleteUdpProxy {
        respond_to: oneshot::Sender<ProxyManagerMsg>,
        endpoint: Endpoint,
    },
    DeleteUdpProxyRs {},
}

pub struct ProxyManager {
    handler_udp: JoinHandle<()>,
    handler_tcp: JoinHandle<()>,
    udp_tx: Sender<ProxyManagerMsg>,
    endpoints: HashMap<u16, Endpoint>,
}

impl ProxyManager {
    pub fn new() -> Self {
        let (tx, mut rx) = mpsc::channel::<ProxyManagerMsg>(100);
        let handler_udp = tokio::spawn(async move {
            let mut udp_proxy = HashMap::<Endpoint, ProxyEndpointHandler>::new();
            loop {
                while let Some(msg) = rx.recv().await {
                    match msg {
                        ProxyManagerMsg::CreateUdpProxy {
                            respond_to,
                            endpoint,
                        } => {
                            let _ = match Self::create_udp_proxy_impl(&mut udp_proxy, endpoint)
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
                                Ok(port) => respond_to.send(ProxyManagerMsg::DeleteUdpProxyRs {}),
                                Err(error) => respond_to.send(ProxyManagerMsg::UdpProxyErrorRs {
                                    error: error.to_string(),
                                }),
                            };
                        }
                        _ => break,
                    }
                }
            }
        });
        let handler_tcp = tokio::spawn(async move {
            std::thread::sleep(Duration::from_secs(1));
        });
        ProxyManager {
            handler_udp,
            handler_tcp,
            udp_tx: tx,
            endpoints: HashMap::<u16, Endpoint>::new(),
        }
    }
    pub async fn delete_udp_proxy(&mut self, port: u16) -> Result<(), Box<dyn Error>> {
        let endpoint = match self.endpoints.get(&port) {
            Some(endpoint) => endpoint,
            None => return Err(Box::<dyn Error>::from("No such endpoint")),
        };
        let (rx, tx) = oneshot::channel::<ProxyManagerMsg>();
        self.udp_tx
            .send(ProxyManagerMsg::DeleteUdpProxy {
                respond_to: rx,
                endpoint: endpoint.clone(),
            })
            .await?;
        let _ = match tx.await {
            Ok(res) => match res {
                ProxyManagerMsg::DeleteUdpProxyRs {} => Ok(()),
                ProxyManagerMsg::UdpProxyErrorRs { error } => Err(Box::<dyn Error>::from(error)),
                _ => Err(Box::<dyn Error>::from("Somethoing happened")),
            },
            Err(_) => Err(Box::<dyn Error>::from("Somethoing happened")),
        };
        Ok(())
    }
    pub async fn create_udp_proxy(
        &mut self,
        request: UdpAllocateRequest,
    ) -> Result<UdpAllocateResponse, Box<dyn Error>> {
        let endpoint = Endpoint::new(request.target_udp_port, &request.target_ip);
        let (rx, tx) = oneshot::channel::<ProxyManagerMsg>();
        self.udp_tx
            .send(ProxyManagerMsg::CreateUdpProxy {
                respond_to: rx,
                endpoint: endpoint.clone(),
            })
            .await?;
        match tx.await {
            Ok(res) => match res {
                ProxyManagerMsg::CreateUdpProxyRs { port } => {
                    self.endpoints.insert(port, endpoint);
                    Ok(UdpAllocateResponse {
                        proxy_udp_port: port,
                        proxy_tcp_port: 80,
                        proxy_ssl_tcp_port: 443,
                        load_percentage: 0,
                    })
                }
                ProxyManagerMsg::UdpProxyErrorRs { error } => Err(Box::<dyn Error>::from(error)),
                _ => Err(Box::<dyn Error>::from("Something happened")),
            },
            Err(_) => Err(Box::<dyn Error>::from("Something happened")),
        }
    }
    async fn create_udp_proxy_impl(
        udp_proxy: &mut HashMap<Endpoint, ProxyEndpointHandler>,
        endpoint: Endpoint,
    ) -> Result<u16, Box<dyn Error>> {
        if !udp_proxy.contains_key(&endpoint) {
            let new_addr =
                SocketAddr::from_str(&std::format!("{}:{}", endpoint.ip, endpoint.port))?;
            let new_proxy = ProxyEndpointHandler::new(new_addr).await?;
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
    async fn delete_udp_proxy_impl(
        udp_proxy: &mut HashMap<Endpoint, ProxyEndpointHandler>,
        endpoint: Endpoint,
    ) -> Result<(), Box<dyn Error>> {
        match udp_proxy.get(&endpoint) {
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
