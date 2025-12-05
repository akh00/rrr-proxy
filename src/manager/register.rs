use anyhow::anyhow;
use std::sync::Arc;
use std::time::Duration;
use tracing::trace;

use futures::SinkExt;
use futures::{channel::mpsc, future::select, pin_mut};
use futures_channel::mpsc::UnboundedSender;
use log::debug;
use tokio::time::sleep;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::{Message, Utf8Bytes};

use crate::consts;
use crate::manager::load::ReportLoadT;
use crate::manager::models::{LoadReport, RegisterRequest};

struct RegisterClient {
    url: String,
    data: RegisterRequest,
}

impl RegisterClient {
    fn new(url: String, data: RegisterRequest) -> Self {
        RegisterClient { url, data }
    }

    async fn connect(
        &self,
        receiver: &mut mpsc::UnboundedReceiver<Message>,
    ) -> Result<(), anyhow::Error> {
        use futures::stream::StreamExt;
        match connect_async(&self.url).await {
            Ok((ws_stream, _response)) => {
                let (mut write, read) = ws_stream.split();
                let str_load = serde_json::to_string(&self.data)?;
                write.send(Message::from(str_load)).await?;
                let mpsc_to_ws = receiver.map(Message::from).map(Ok).forward(write);
                let ws_to_mpsc = {
                    read.for_each(|message| async {
                        let _ = message.map(|msg| debug!("Recieved:{:?}", msg.into_text()));
                    })
                };

                pin_mut!(mpsc_to_ws, ws_to_mpsc);
                select(mpsc_to_ws, ws_to_mpsc).await;
            }
            Err(error) => {
                log::error!("Unhandled WS msg: {:?}", error);
            }
        }
        log::error!("Connection failed");
        Err(anyhow!("Connection failed"))
    }
}

pub struct RegisterAgent {
    url: String,
    load_reporter: Arc<dyn ReportLoadT + Send + Sync>,
}

impl RegisterAgent {
    pub fn new(url: String, load: Arc<dyn ReportLoadT + Send + Sync>) -> Self {
        RegisterAgent {
            url: url,
            load_reporter: load,
        }
    }

    pub async fn heartbeat(
        mut self,
        mut tx: UnboundedSender<Message>,
    ) -> Result<(), anyhow::Error> {
        loop {
            tokio::select!(
                _  = sleep(Duration::from_millis(5000)) => {
                    if let Ok(load) = Arc::get_mut(&mut self.load_reporter).unwrap().get_load() {
                        let str_load  = serde_json::to_string(&LoadReport{ _type: "load", load: load.percent })?;
                        trace!("Sending report:{}", str_load);
                        if let Err(_) = tx.send(Message::Text(Utf8Bytes::from(&str_load))).await {
                            return Ok(());
                        } else {
                            trace!("Sent report:{}", str_load);
                        };
                    }  else {
                        return Err(anyhow!("Can not get load"));
                    }
                }

            );
        }
    }

    pub async fn run(self) -> Result<(), anyhow::Error> {
        let (tx, mut rx) = futures_channel::mpsc::unbounded();
        let client = RegisterClient::new(
            self.url.clone(),
            RegisterRequest {
                _type: "register",
                guid: (*consts::PROXY_GUID).to_string(),
                public_ipv4: (*consts::PUBLIC_IPV4).clone(),
                public_ipv6: (*consts::PUBLIC_IPV6).clone(),
                private_ip: (*consts::PRIVATE_IP).clone(),
                az: (*consts::AVAILABILITY_ZONE).to_string(),
            },
        );
        if let Ok(_) = tokio::try_join!(client.connect(&mut rx), self.heartbeat(tx)) {
            Ok(())
        } else {
            Err(anyhow!("error during registration happened"))
        }
    }
}
