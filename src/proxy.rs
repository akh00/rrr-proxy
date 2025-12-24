use log::{info, warn};
use std::fmt;
use std::net::SocketAddr;
use std::sync::Arc;
use std::{collections::HashMap, error::Error};
use tcp_clients::ProxyTcpEndpointHandler;
use tokio::sync::mpsc::{self, Sender};
use tokio::sync::{RwLock, oneshot};
use tracing::{debug, error, instrument};

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

/*pub(crate) enum ProxyHandler {
    TcpHandler(ProxyTcpEndpointHandler),
    UdpHandler(ProxyEndpointHandler),
}
*/
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

#[derive(Debug)]
pub struct ProxyManager {
    tx: Sender<ProxyManagerMsg>,
    udp_endpoints: HashMap<Session, Endpoint>,
    tcp_endpoints: HashMap<Session, Endpoint>,
}

impl ProxyManager {
    pub fn new() -> Self {
        let (tx, mut rx) = mpsc::channel::<ProxyManagerMsg>(100);
        let sender_to_pass = tx.clone();
        let _ = tokio::spawn(async move {
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
            udp_endpoints: HashMap::<Session, Endpoint>::new(),
            tcp_endpoints: HashMap::<Session, Endpoint>::new(),
        }
    }

    pub async fn delete_udp_proxy(
        &mut self,
        port: u16,
        session_id: String,
    ) -> Result<(), anyhow::Error> {
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
        }?;
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
        udp_proxy: &mut HashMap<Endpoint, ProxyEndpointHandler>,
        endpoint: Endpoint,
        tx: Sender<ProxyManagerMsg>,
    ) -> Result<u16, Box<dyn Error>> {
        if let Some(handler) = udp_proxy.get(&endpoint) {
            warn!("There are already proxy created for {:?}", endpoint);
            Ok(handler.local_port)
        } else {
            let new_proxy = ProxyEndpointHandler::new(endpoint.clone(), tx).await?;
            let local_port = new_proxy.local_port;
            udp_proxy.insert(endpoint, new_proxy);
            Ok(local_port)
        }
    }

    #[instrument(level = "debug")]
    async fn create_tcp_proxy_impl(
        tcp_proxy: &mut HashMap<Endpoint, ProxyTcpEndpointHandler>,
        endpoint: Endpoint,
        tx: Sender<ProxyManagerMsg>,
    ) -> Result<u16, Box<dyn Error>> {
        if let Some(handler) = tcp_proxy.get(&endpoint) {
            warn!("There are already proxy created for {:?}", endpoint);
            Ok(handler.local_port)
        } else {
            let new_proxy = ProxyTcpEndpointHandler::new(endpoint.clone(), tx).await?;
            let local_port = new_proxy.local_port;
            tcp_proxy.insert(endpoint, new_proxy);
            Ok(local_port)
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
