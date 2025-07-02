use core::str;
use std::{collections::HashMap, error::Error, net::SocketAddr, sync::Arc, time::Duration};
use tokio::{
    net::UdpSocket,
    select,
    sync::{
        mpsc::{self, Receiver, Sender},
        oneshot,
    },
    task::JoinHandle,
    time::{sleep, timeout},
};
use tracing::{debug, error, info, instrument};
//
//  agent pattern implementation
//
//
//
#[derive(Debug)]
enum ProxyClientMsg {
    KillEndpoint {
        respond_to: oneshot::Sender<ProxyClientMsg>,
    },
    EndpointEnded {},
}

#[derive(Debug)]
struct ProxyEndpoint {
    //agent
    udp_socket: UdpSocket,
    rx: Receiver<(Vec<u8>, SocketAddr)>, // reciever from ms faced clients
    tx: Sender<(Vec<u8>, SocketAddr)>,   //sender to routeer
    msg_tx: Sender<ProxyClientMsg>,
}

impl ProxyEndpoint {
    fn new(
        udp_socket: UdpSocket,
        tx: Sender<(Vec<u8>, SocketAddr)>,
        rx: Receiver<(Vec<u8>, SocketAddr)>,
        msg_tx: Sender<ProxyClientMsg>,
    ) -> Self {
        ProxyEndpoint {
            udp_socket,
            rx,
            tx,
            msg_tx,
        }
    }
    async fn run(mut self) {
        let mut buf = [0; 64 * 1024];
        loop {
            select! {
                a = self.udp_socket.recv_from(&mut buf) => {
                   let (len, addr )= a.unwrap();
                   debug!("{:?} bytes received from {:?}", len, addr);
                // to the router
                    match self.tx.send((buf[..len].to_vec(), addr)).await {
                        Ok(_) => {
                            debug!("Sent back to router with addr:{}", addr);
                        }
                        Err(error) => {
                            debug!("Error sending to ruter: {}", error);
                            break;
                        }
                    };
               },
               b = self.rx.recv() => {
                   let (res_buf, addr)= b.unwrap();
                   match self.udp_socket.send_to(res_buf.as_slice(), addr).await {
                       Ok(_) => {
                            debug!("Sent back to client {:?} buf:{:?}", addr, str::from_utf8(res_buf.as_slice()).unwrap());
                       },
                       Err(error) => {
                            debug!("Error happened local recieve{:?} ", error);
                            break;
                       }
                   }
               },
               _ = sleep(Duration::from_millis(1000)) => {
                    break;
               }
            }
        }
        if let Err(err) = self.msg_tx.send(ProxyClientMsg::EndpointEnded {}).await {
            error!("Error happened during sending end message {:?}", err);
        }
        ()
    }
}

#[derive(Debug)]
struct ProxyRouterClient {
    tx: Sender<(Vec<u8>, SocketAddr)>, // sender to be copied to ms faced clients
    rx: Receiver<(Vec<u8>, SocketAddr)>, // reciever for routing incoming
    clients: HashMap<SocketAddr, ProxyClientHandler>,
    remote_addr: SocketAddr,
}

impl ProxyRouterClient {
    fn new(
        tx: Sender<(Vec<u8>, SocketAddr)>,
        rx: Receiver<(Vec<u8>, SocketAddr)>,
        remote_addr: SocketAddr,
    ) -> Self {
        let clients = HashMap::<SocketAddr, ProxyClientHandler>::new();
        ProxyRouterClient {
            tx,
            rx,
            clients,
            remote_addr,
        }
    }

    async fn run(mut self) {
        loop {
            let _ = match self.rx.recv().await {
                Some((data, addr)) => {
                    let client = match self.clients.get(&addr) {
                        Some(client) => client,
                        None => {
                            if let Ok(client) =
                                ProxyClientHandler::new(self.remote_addr, addr, self.tx.clone())
                                    .await
                            {
                                self.clients.insert(addr, client);
                            } else {
                                error!(
                                    "Can not create new proxy clinet for remote_addr{:?}, addr: {:?}",
                                    self.remote_addr, addr
                                );
                                break;
                            }
                            self.clients.get(&addr).unwrap()
                        }
                    };
                    if let Err(_) = client.send(data).await {
                        error!("Can not send data to new created proxy clinet {:?}", client);
                        break;
                    }
                }
                None => {
                    info!("No channel any more finishing ProxyRouterClient");
                    break;
                }
            };
        }
    }
}

#[derive(Debug)]
pub(crate) struct ProxyEndpointHandler {
    pub local_port: u16,
    msg_reciever: Receiver<ProxyClientMsg>,
    endpoint_handle: JoinHandle<()>,
    router_handle: JoinHandle<()>,
}

impl ProxyEndpointHandler {
    #[instrument(level = "debug")]
    pub async fn new(remote_addr: SocketAddr) -> Result<Self, Box<dyn Error>> {
        let local_socket = UdpSocket::bind("0.0.0.0:0").await?;
        let port: u16 = local_socket.local_addr()?.port();
        let (tx, rx) = mpsc::channel::<(Vec<u8>, SocketAddr)>(1000);
        let (router_tx, router_rx) = mpsc::channel::<(Vec<u8>, SocketAddr)>(1000);

        let (mgr_sender, msg_reciever) = mpsc::channel::<ProxyClientMsg>(2);
        let endpoint = ProxyEndpoint::new(local_socket, router_tx, rx);
        let handle = tokio::spawn(async move {
            endpoint.run().await;
        });
        let router = ProxyRouterClient::new(tx.clone(), router_rx, remote_addr);
        let router_handle = tokio::spawn(async move {
            router.run().await;
        });
        let pe_handler = ProxyEndpointHandler {
            local_port: port,
            msg_reciever: msg_reciever,
            endpoint_handle: handle,
            router_handle: router_handle,
        };
        Ok(pe_handler)
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
}

impl ProxyClient {
    fn new(
        udp_socket: UdpSocket,
        tx: Sender<(Vec<u8>, SocketAddr)>,
        rx: Receiver<Vec<u8>>,
        addr: SocketAddr,
    ) -> Self {
        ProxyClient {
            udp_socket,
            tx,
            rx,
            addr,
        }
    }

    #[instrument(level = "debug")]
    async fn run(mut self) {
        let mut buf = [0; 64 * 1024];
        loop {
            select! {
                a = self.udp_socket.recv(&mut buf) => {
                    if let Ok(len) = a {
                        debug!("{:?} bytes received from {:?}", len, self.addr);
                        if let Err(error) = self.tx.send((buf[..len].to_vec(), self.addr)).await {
                            error!("Error happened router send {:?} ", error);
                            break;
                        }
                } else {
                    {
                        debug!("ProxyClient: Error sending to socket, existing");
                        break;
                    }
                }

               },
               b = self.rx.recv() => {
                if let Some(res_buf) = b {
                   match self.udp_socket.send(res_buf.as_slice()).await {
                       Ok(_) => {
                            debug!("Sent to ms {:?}", str::from_utf8(res_buf.as_slice()).unwrap());
                            continue;
                       },
                    Err(error) => {
                           error!("ProxyClient: Error happened remote recieve {:?} ", error);
                           break;
                       }
                    }
                } else {
                    debug!("ProxyClient: Error recieving from channel , existing");
                    break;
                }
               },
               else => {
                    debug!("Something happened, existing");
                    break;
               }
            }
        }
    }
}

#[derive(Clone, Debug)]
struct ProxyClientHandler {
    //agent handler
    tx: Sender<Vec<u8>>,
    handler: Arc<JoinHandle<()>>, // maybe return type
}

impl ProxyClientHandler {
    async fn new(
        remote_addr: SocketAddr,
        cleints_addr: SocketAddr,
        server_sender: Sender<(Vec<u8>, SocketAddr)>,
    ) -> Result<Self, Box<dyn Error + Send + Sync>> {
        let (tx, rx) = mpsc::channel::<Vec<u8>>(1000);
        let udp_socket = UdpSocket::bind("0.0.0.0:0").await?;
        let handler = match udp_socket.connect(remote_addr).await {
            Ok(_) => {
                let client = ProxyClient::new(udp_socket, server_sender, rx, cleints_addr);
                tokio::spawn(async move {
                    client.run().await;
                })
            }
            Err(error) => {
                println!("Error happened rm_socket_error {:?} ", error);
                return Err("Something went wrong".to_string().into());
            }
        };
        Ok(ProxyClientHandler {
            tx,
            handler: Arc::new(handler),
        })
    }
    async fn send(&self, data: Vec<u8>) -> Result<(), Box<dyn Error>> {
        self.tx.send(data).await?;
        Ok(())
    }
}
