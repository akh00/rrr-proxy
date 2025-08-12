use std::sync::Arc;
use std::time::Duration;

use futures::SinkExt;
use futures::{channel::mpsc, future::select, pin_mut};
use futures_channel::mpsc::UnboundedSender;
use log::debug;
use tokio::time::sleep;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::{Message, Utf8Bytes};

use crate::manager::load::{self, ReportLoadT};
use crate::manager::models::LoadReport;

struct RegisterClient {
    url: String,
}

impl RegisterClient {
    fn new(url: String) -> Self {
        RegisterClient { url }
    }

    async fn connect(&self, receiver: &mut mpsc::UnboundedReceiver<Message>) -> () {
        use futures::stream::StreamExt;
        match connect_async(&self.url).await {
            Ok((ws_stream, _response)) => {
                debug!("Connected!!!! {:?}", self.url);
                let (write, mut read) = ws_stream.split();
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
    }
}

pub struct RegisterAgent {
    url: String,
    load_reporter: Arc<dyn ReportLoadT + Send + Sync>,
}

impl RegisterAgent {
    pub fn new(url: String, timeout: u64, load: Arc<dyn ReportLoadT + Send + Sync>) -> Self {
        RegisterAgent {
            url: url,
            load_reporter: load,
        }
    }

    pub async fn heartbeat(mut self, mut tx: UnboundedSender<Message>) -> Result<(), String> {
        loop {
            tokio::select!(
                _  = sleep(Duration::from_millis(5000)) => {
                    if let Ok(load) = Arc::get_mut(&mut self.load_reporter).unwrap().get_load() {
                        let str_load  = serde_json::to_string(&LoadReport{ percent: load.percent }).ok().unwrap();
                        debug!("Sending report:{}", str_load);
                        if let Err(_) = tx.send(Message::Text(Utf8Bytes::from(&str_load))).await {
                            return Ok(());
                        } else {
                            debug!("Sent report:{}", str_load);
                        };
                    }  else {
                        return Err("Can not get load".to_string());
                    }
                }

            );
        }
    }

    pub async fn run(self) -> Result<(), String> {
        let (tx, mut rx) = futures_channel::mpsc::unbounded();
        let client = RegisterClient::new(self.url.clone());
        if let (_, Ok(_)) = tokio::join!(client.connect(&mut rx), self.heartbeat(tx)) {
            Ok(())
        } else {
            Err("load calculationg erroir happened".to_string())
        }
    }
}
