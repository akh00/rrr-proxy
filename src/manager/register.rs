use std::error::Error;
use std::sync::Arc;
use std::time::Duration;

use futures::SinkExt;
use futures::channel::mpsc::UnboundedSender;
use futures::{channel::mpsc, future::select, pin_mut};
use log::debug;
use tokio::sync::RwLock;
use tokio::task::{JoinError, JoinHandle};
use tokio::time::sleep;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::{Message, Utf8Bytes};

use crate::manager::load::{self, ReportLoadT};
use crate::manager::models::LoadReport;

struct RegisterClient {
    url: String,
    timeout: u64,
}

impl RegisterClient {
    fn new(url: String, timeout: u64) -> Self {
        RegisterClient { url, timeout }
    }

    async fn connect(&self, receiver: &mut mpsc::UnboundedReceiver<Message>) -> () {
        use futures::stream::StreamExt;
        match connect_async(&self.url).await {
            Ok((ws_stream, _response)) => {
                let (write, mut read) = ws_stream.split();
                let mpsc_to_ws = receiver.map(Message::from).map(Ok).forward(write);
                let ws_to_mpsc = async {
                    while let Some(message) = read.next().await {
                        debug!("Received {:?}", message);
                    }
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
    fn register(&mut self) -> Result<(), u16> {
        Ok(())
    }
}

pub struct RegisterAgent {
    url: String,
    timeout: u64,
    load_reporter: Arc<RwLock<dyn ReportLoadT + Send + Sync>>,
}

impl RegisterAgent {
    pub fn new(
        url: String,
        timeout: u64,
        load: Arc<RwLock<dyn ReportLoadT + Send + Sync>>,
    ) -> Self {
        RegisterAgent {
            url: url,
            timeout: timeout,
            load_reporter: load,
        }
    }

    pub async fn run(self) -> Result<(), String> {
        let (mut tx, mut rx) = futures_channel::mpsc::unbounded();
        let handle = tokio::spawn(async move {
            let client = RegisterClient::new(self.url, self.timeout);
            loop {
                tokio::select!(
                    _ = client.connect(&mut rx) => {
                        break;
                    },
                    _  = sleep(Duration::from_millis(1000)) => {
                        if let Ok(load) = self.load_reporter.blocking_write().get_load() {
                            let str_load  = serde_json::to_string(&LoadReport{ percent: load.percent }).ok().unwrap();
                            if let Err(_) = tx.send(Message::Text(Utf8Bytes::from(str_load))).await { break }
                         else {
                            break;
                        }
                    }
                }
                );
            }
            ()
        });
        match handle.await {
            Ok(_) => Ok(()),
            Err(err) => Err(err.to_string()),
        }
    }
}
