use anyhow::anyhow;
use std::sync::Arc;
use std::time::Duration;
use tracing::{info, trace};

use futures::SinkExt;
use futures::{channel::mpsc, future::select, pin_mut};
use futures_channel::mpsc::{UnboundedReceiver, UnboundedSender};
use log::debug;
use signal_hook_tokio::Signals;
use tokio::time::sleep;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::{Message, Utf8Bytes};

use futures::stream::StreamExt;

use crate::consts;
use crate::manager::load::ReportLoadT;
use crate::manager::models::{DeregisterRequest, LoadReport, RegisterRequest};

pub struct SignalHandler {
    signals: Signals,
    delay: u32,
    tx: UnboundedSender<Message>,
}

impl SignalHandler {
    pub fn new(signals: Signals, delay: u32, tx: UnboundedSender<Message>) -> Self {
        SignalHandler { signals, delay, tx }
    }

    pub async fn run(&mut self) -> Result<(), anyhow::Error> {
        while let Some(signal) = self.signals.next().await {
            info!("Signal catched {:?} scheduling exit", signal);
            let str_disconnect = serde_json::to_string(&DeregisterRequest {
                _type: "deregister",
                drain_period_sec: self.delay,
            })?;
            info!("Sending derigister request:{:?}", str_disconnect);
            let _ = self
                .tx
                .send(Message::Text(Utf8Bytes::from(&str_disconnect)))
                .await?;
            break;
        }
        let _ = sleep(Duration::from_secs(self.delay.into())).await;
        Ok(())
    }
}

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
                info!("Registered to {:?}", &self.url.clone());
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
    pub tx: UnboundedSender<Message>,
    rx: Option<UnboundedReceiver<Message>>,
}

impl RegisterAgent {
    pub fn new(url: String, load: Arc<dyn ReportLoadT + Send + Sync>) -> Self {
        let (tx, rx) = futures_channel::mpsc::unbounded();
        RegisterAgent {
            url: url,
            load_reporter: load,
            tx: tx,
            rx: Some(rx),
        }
    }

    pub async fn heartbeat(
        &mut self,
        mut tx: UnboundedSender<Message>,
    ) -> Result<(), anyhow::Error> {
        let delay = *consts::HEARTBIT_DELAY;
        loop {
            tokio::select!(
                _  = sleep(Duration::from_millis(delay)) => {
                    if let Ok(load) = Arc::get_mut(&mut self.load_reporter).ok_or("Can not get load").unwrap().get_load() {
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

    pub async fn run(&mut self) -> Result<(), anyhow::Error> {
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
        let max_attempt = *consts::MAX_REGISTRATION_ATTEMPTS;
        let mut cur_attempt = 0;
        let delay = *consts::HEARTBIT_DELAY;
        let mut reciever = std::mem::replace(&mut self.rx, None).unwrap();
        loop {
            match tokio::try_join!(
                client.connect(&mut reciever),
                self.heartbeat(self.tx.clone())
            ) {
                Ok(_) => {
                    return Ok(());
                }
                Err(err) => {
                    cur_attempt += 1;
                    if cur_attempt > max_attempt {
                        return Err(anyhow!("error during registration happened"));
                    } else {
                        info!(
                            "Proxy registration failed: {:?}, sleeping  for {:?}",
                            err, delay
                        );
                        sleep(Duration::from_millis(delay)).await;
                    }
                }
            }
        }
    }
}
