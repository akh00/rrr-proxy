use std::error::Error;

use futures::{channel::mpsc, future::select, pin_mut};
use log::debug;
use tokio::task::{JoinError, JoinHandle};
use tokio_tungstenite::connect_async;
use tungstenite::Message;

use crate::manager::FixBoxError;
use crate::manager::load::ReportLoadT;

struct RegisterClient {
    url: String,
    timeout: u64,
}

impl RegisterClient {
    fn new(url: String) -> Self {
        RegisterClient { url, timeout: 5000 }
    }

    async fn connect(self, receiver: mpsc::UnboundedReceiver<Message>) -> () {
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
    client: RegisterClient,
    load_reporer: Box<dyn ReportLoadT>,
}

impl RegisterAgent {
    pub fn new(url: String, timeout: u64, load: Box<dyn ReportLoadT>) -> Self {
        RegisterAgent {
            client: RegisterClient { url, timeout },
            load_reporer: load,
        }
    }

    pub async fn run(mut self) -> Result<(), &'static str> {
        let handle = tokio::spawn(async move { () });
        match handle.await {
            Ok(_) => Ok(()),
            Err(err) => Err("error happened"),
        }
    }
}
